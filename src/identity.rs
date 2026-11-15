//! A client's persistent iroh identity, so a server can allow only specific
//! clients (`serve --allow <ID>`).
//!
//! `fetch` normally connects with a new, random endpoint id each time. Once
//! `deaddrop id` has created an identity file, `fetch` uses it instead, so
//! the client's id stays the same and can be put on a server's allow list.
//! The file holds the secret key as 64 hex characters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use iroh::{EndpointId, SecretKey};

/// The default identity file for this platform, if one can be found
/// (`~/.local/share/deaddrop/identity.key` on Linux).
pub fn default_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "deaddrop").map(|d| d.data_dir().join("identity.key"))
}

/// The identity `fetch` should use: the `explicit` file (which must exist),
/// or the default file if it exists. `None`: use a new random id.
pub fn for_fetch(explicit: Option<&Path>) -> Result<Option<SecretKey>> {
    match explicit {
        Some(path) => {
            if !path.is_file() {
                bail!(
                    "identity file {} does not exist; create it with `deaddrop id --identity {}`",
                    path.display(),
                    path.display()
                );
            }
            load(path).map(Some)
        }
        None => match default_path() {
            Some(path) if path.is_file() => load(&path).map(Some),
            _ => Ok(None),
        },
    }
}

pub fn load(path: &Path) -> Result<SecretKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read identity file {}", path.display()))?;
    let key = text
        .trim()
        .parse::<SecretKey>()
        .with_context(|| format!("invalid identity file {}", path.display()))?;
    warn_if_readable(path);
    Ok(key)
}

/// Load the identity at `path`, creating it first if it doesn't exist (or,
/// with `replace`, even if it does).
pub fn load_or_create(path: &Path, replace: bool) -> Result<(SecretKey, bool)> {
    if path.is_file() && !replace {
        return Ok((load(path)?, false));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let key = SecretKey::generate();
    // Write to a temporary file and rename it into place, so a crash never
    // leaves a half-written identity.
    let tmp = path.with_extension("key.tmp");
    write_private(&tmp, &format!("{}\n", hex(&key.to_bytes())))
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to write identity file {}", path.display()))?;
    Ok((key, true))
}

/// Parse a server's `--allow` / `serve.allow` entry.
pub fn parse_id(s: &str) -> Result<EndpointId> {
    s.trim().parse::<EndpointId>().with_context(|| {
        format!("invalid endpoint id {s:?} (expected the output of `deaddrop id`)")
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

/// The identity is a secret: anyone who has it can pass as this client.
#[cfg(unix)]
fn warn_if_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path)
        && meta.permissions().mode() & 0o077 != 0
    {
        say!(
            "warning: identity file {} can be read by other users; consider `chmod 600 {}`",
            path.display(),
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_reload() {
        let dir = std::env::temp_dir().join(format!("deaddrop-id-test-{}", std::process::id()));
        let path = dir.join("sub/identity.key");
        let (key, created) = load_or_create(&path, false).unwrap();
        assert!(created);
        let (again, created) = load_or_create(&path, false).unwrap();
        assert!(!created);
        assert_eq!(key.public(), again.public());
        assert_eq!(
            for_fetch(Some(&path)).unwrap().unwrap().public(),
            key.public()
        );
        let (new, created) = load_or_create(&path, true).unwrap();
        assert!(created);
        assert_ne!(new.public(), key.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(for_fetch(Some(&path)).is_err());
    }

    #[test]
    fn ids_parse() {
        let key = SecretKey::generate();
        let id = key.public();
        assert_eq!(parse_id(&id.to_string()).unwrap(), id);
        assert_eq!(parse_id(&format!(" {id}\n")).unwrap(), id);
        assert!(parse_id("not-an-id").is_err());
        assert!(parse_id(&id.to_string()[..10]).is_err());
    }
}
