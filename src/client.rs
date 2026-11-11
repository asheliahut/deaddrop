use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, endpoint::presets};
use iroh_blobs::{
    BlobFormat, Hash, api::remote::GetProgressItem, format::collection::Collection,
    store::fs::FsStore, ticket::BlobTicket,
};
use n0_future::StreamExt;
use tokio_util::io::SyncIoBridge;

use crate::{
    crypt::{self, Key, Manifest},
    torrent::human,
};

pub async fn run(ticket: BlobTicket, out: PathBuf, key: Key) -> Result<()> {
    ensure!(
        ticket.format() == BlobFormat::HashSeq,
        "this ticket is for a single blob, not a deaddrop collection"
    );
    tokio::fs::create_dir_all(&out).await?;

    // Download the ciphertext into a temporary store inside the output
    // directory, decrypt the files out of it, then delete it.
    let store_dir = out.join(format!(".deaddrop-{}", ticket.hash().fmt_short()));
    let res = fetch(&ticket, &out, &store_dir, &key).await;
    tokio::fs::remove_dir_all(&store_dir).await.ok();
    res
}

async fn fetch(ticket: &BlobTicket, out: &Path, store_dir: &Path, key: &Key) -> Result<()> {
    let store = FsStore::load(store_dir).await?;
    let endpoint = Endpoint::builder(presets::N0).bind().await?;
    let res = download(&endpoint, &store, ticket, out, key).await;
    // Close cleanly on errors too, so a single-use server frees its slot
    // right away instead of waiting for the connection to time out.
    endpoint.close().await;
    store.shutdown().await.ok();
    res
}

async fn download(
    endpoint: &Endpoint,
    store: &FsStore,
    ticket: &BlobTicket,
    out: &Path,
    key: &Key,
) -> Result<()> {
    eprintln!("connecting to {}", ticket.addr().id);
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .context("failed to connect to the server")?;

    let mut stream = store
        .remote()
        .fetch(conn.clone(), ticket.hash_and_format())
        .stream();
    let mut last_print = std::time::Instant::now();
    let stats = loop {
        match stream.next().await {
            Some(GetProgressItem::Progress(n)) => {
                if last_print.elapsed().as_millis() >= 500 {
                    eprint!("\rdownloaded {}          ", human(n));
                    last_print = std::time::Instant::now();
                }
            }
            Some(GetProgressItem::Done(stats)) => break stats,
            Some(GetProgressItem::Error(e)) => {
                eprintln!();
                bail!(
                    "download failed: {e}\n\
                     (if the server was reset to a different torrent, this ticket no longer works; \
                     ask for the new one)"
                );
            }
            None => bail!("download ended without a result"),
        }
    };
    eprintln!(
        "\rdownloaded {} in {:.1?}          ",
        human(stats.payload_bytes_read),
        stats.elapsed
    );

    // We have everything; close the connection so a single-use server can shut down.
    conn.close(0u32.into(), b"done");

    let collection = Collection::load(ticket.hash(), &**store).await?;
    let mut entries = collection.iter();
    let manifest_hash = match entries.next() {
        Some((name, hash)) if name == crypt::MANIFEST_NAME => *hash,
        _ => bail!("the server sent unencrypted data; it needs a deaddrop version with encryption"),
    };
    let manifest = store.blobs().get_bytes(manifest_hash).await?;
    let manifest = Manifest::decrypt(key, &manifest)?;
    let files: Vec<_> = entries.map(|(_, hash)| *hash).collect();
    ensure!(
        manifest.files.len() == files.len(),
        "the manifest lists {} file(s) but the collection has {}",
        manifest.files.len(),
        files.len()
    );
    // Check every name before writing anything.
    let targets = manifest
        .files
        .iter()
        .map(|name| safe_join(out, name))
        .collect::<Result<Vec<_>>>()?;
    eprintln!("decrypting {}", manifest.name);

    for (hash, target) in files.into_iter().zip(targets) {
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        decrypt_to(store, hash, key, &target)
            .await
            .with_context(|| format!("failed to write {}", target.display()))?;
        eprintln!("wrote {}", target.display());
    }

    Ok(())
}

/// Decrypt one blob into `target`. It is written under a temporary name and
/// renamed at the end, so a failure never leaves a partial file under the
/// real name.
async fn decrypt_to(store: &FsStore, hash: Hash, key: &Key, target: &Path) -> Result<()> {
    let file_name = target
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .into_owned();
    let partial = target.with_file_name(format!(".{file_name}.deaddrop-partial"));
    let reader = SyncIoBridge::new(store.blobs().reader(hash));
    let res = tokio::task::spawn_blocking({
        let (key, partial) = (key.clone(), partial.clone());
        move || -> Result<()> {
            let out = std::fs::File::create(&partial)?;
            crypt::decrypt(
                &key,
                std::io::BufReader::new(reader),
                std::io::BufWriter::new(out),
            )?;
            Ok(())
        }
    })
    .await?;
    match res {
        Ok(()) => Ok(tokio::fs::rename(&partial, target).await?),
        Err(e) => {
            tokio::fs::remove_file(&partial).await.ok();
            Err(e)
        }
    }
}

/// Join a server-supplied relative path onto `base`. Reject anything that
/// could escape `base` (absolute paths, `..`, drive prefixes).
fn safe_join(base: &Path, name: &str) -> Result<PathBuf> {
    let rel = Path::new(name);
    ensure!(!name.is_empty(), "empty file name in collection");
    for c in rel.components() {
        match c {
            Component::Normal(_) => {}
            _ => bail!("refusing unsafe path from server: {name:?}"),
        }
    }
    Ok(base.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_escapes() {
        let base = Path::new("/out");
        assert!(safe_join(base, "a/b.txt").is_ok());
        assert!(safe_join(base, "../etc/passwd").is_err());
        assert!(safe_join(base, "/etc/passwd").is_err());
        assert!(safe_join(base, "a/../../x").is_err());
        assert!(safe_join(base, "").is_err());
    }
}
