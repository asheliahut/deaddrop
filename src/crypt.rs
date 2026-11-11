//! Encryption of the served data, so the server's disk, the blob store and
//! iroh only ever handle ciphertext.
//!
//! Each torrent gets a fresh age X25519 key. Every file is encrypted as its
//! own age file. The file names go into an encrypted [`Manifest`], which is
//! the first entry of the collection; the file entries themselves are named
//! by index only. The key never touches the server's disk: it is printed by
//! `serve` and sent to the notifiers that ask for it.

use std::{
    io::{BufRead, Read, Write},
    path::Path,
};

use age::{
    DecryptError,
    secrecy::ExposeSecret,
    x25519::{Identity, Recipient},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Name of the collection entry that holds the encrypted manifest.
pub const MANIFEST_NAME: &str = "deaddrop-manifest.age";

const MANIFEST_VERSION: u32 = 1;

/// A decryption key (an age X25519 identity, `AGE-SECRET-KEY-1...`).
#[derive(Clone)]
pub struct Key(Identity);

// Hand-written so the key never shows up in debug output.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(<redacted>)")
    }
}

impl Key {
    pub fn generate() -> Self {
        Self(Identity::generate())
    }

    /// Parse a key. The error doesn't echo the input, since it is a secret.
    pub fn parse(s: &str) -> Result<Self> {
        s.trim()
            .parse::<Identity>()
            .map(Self)
            .map_err(|_| anyhow::anyhow!("invalid decryption key (expected AGE-SECRET-KEY-1...)"))
    }

    /// The key as text, for printing and notifications.
    pub fn expose(&self) -> String {
        self.0.to_string().expose_secret().to_string()
    }

    /// The public half, which is all that encryption needs.
    pub fn recipient(&self) -> Recipient {
        self.0.to_public()
    }
}

/// What the client needs to restore the files.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// The torrent's name.
    pub name: String,
    /// File names (relative paths), in the order of the collection's file
    /// entries after the manifest.
    pub files: Vec<String>,
}

impl Manifest {
    pub fn new(name: String, files: Vec<String>) -> Self {
        Self {
            version: MANIFEST_VERSION,
            name,
            files,
        }
    }

    pub fn encrypt(&self, to: &Recipient) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        let mut out = Vec::with_capacity(json.len() + 256);
        encrypt(to, &json[..], &mut out)?;
        Ok(out)
    }

    pub fn decrypt(key: &Key, ciphertext: &[u8]) -> Result<Self> {
        let mut json = Vec::new();
        decrypt(key, ciphertext, &mut json)?;
        let m: Self = serde_json::from_slice(&json).context("the manifest is malformed")?;
        if m.version != MANIFEST_VERSION {
            bail!(
                "unsupported manifest version {} (this deaddrop understands {MANIFEST_VERSION}); upgrade deaddrop",
                m.version
            );
        }
        Ok(m)
    }
}

/// Encrypt `input` to `to`, streaming. Blocking.
pub fn encrypt(to: &Recipient, mut input: impl Read, output: impl Write) -> Result<u64> {
    let encryptor = age::Encryptor::with_recipients(std::iter::once(to as &dyn age::Recipient))
        .context("failed to set up encryption")?;
    let mut writer = encryptor.wrap_output(output)?;
    let n = std::io::copy(&mut input, &mut writer)?;
    writer.finish()?.flush()?;
    Ok(n)
}

/// Encrypt the file at `src` into a new file at `dst`. Blocking.
pub fn encrypt_file(to: &Recipient, src: &Path, dst: &Path) -> Result<u64> {
    let input =
        std::fs::File::open(src).with_context(|| format!("failed to open {}", src.display()))?;
    let output = std::fs::File::create(dst)
        .with_context(|| format!("failed to create {}", dst.display()))?;
    encrypt(
        to,
        std::io::BufReader::new(input),
        std::io::BufWriter::new(output),
    )
    .with_context(|| format!("failed to encrypt {}", src.display()))
}

/// Decrypt `input` with `key`, streaming. Blocking. Fails if the key is wrong
/// or the data was tampered with.
pub fn decrypt(key: &Key, input: impl BufRead, mut output: impl Write) -> Result<u64> {
    let decryptor = age::Decryptor::new_buffered(input).context("not encrypted data")?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&key.0 as &dyn age::Identity))
        .map_err(|e| match e {
            DecryptError::NoMatchingKeys => anyhow::anyhow!("wrong decryption key"),
            e => anyhow::anyhow!("decryption failed: {e}"),
        })?;
    let n = std::io::copy(&mut reader, &mut output).context("decryption failed")?;
    output.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = Key::generate();
        let data = vec![7u8; 200_000];
        let mut ct = Vec::new();
        encrypt(&key.recipient(), &data[..], &mut ct).unwrap();
        assert_ne!(ct, data);
        let mut pt = Vec::new();
        decrypt(&key, &ct[..], &mut pt).unwrap();
        assert_eq!(pt, data);
    }

    #[test]
    fn wrong_key_and_tampering_fail() {
        let key = Key::generate();
        let mut ct = Vec::new();
        encrypt(&key.recipient(), &b"secret"[..], &mut ct).unwrap();
        let err = decrypt(&Key::generate(), &ct[..], Vec::new()).unwrap_err();
        assert!(err.to_string().contains("wrong decryption key"));
        let last = ct.len() - 1;
        ct[last] ^= 1;
        assert!(decrypt(&key, &ct[..], Vec::new()).is_err());
    }

    #[test]
    fn key_text_round_trip_and_redaction() {
        let key = Key::generate();
        let text = key.expose();
        assert!(text.starts_with("AGE-SECRET-KEY-1"));
        let again = Key::parse(&format!("  {text}\n")).unwrap();
        assert_eq!(again.expose(), text);
        assert!(!format!("{key:?}").contains("AGE"));
        let err = Key::parse("AGE-SECRET-KEY-1nope").unwrap_err();
        assert!(!err.to_string().contains("nope"));
    }

    #[test]
    fn manifest_round_trip() {
        let key = Key::generate();
        let m = Manifest::new("t".into(), vec!["a/b.txt".into(), "c".into()]);
        let ct = m.encrypt(&key.recipient()).unwrap();
        assert!(!ct.windows(5).any(|w| w == b"b.txt"));
        let back = Manifest::decrypt(&key, &ct).unwrap();
        assert_eq!(back.files, ["a/b.txt", "c"]);
        assert!(Manifest::decrypt(&Key::generate(), &ct).is_err());
    }
}
