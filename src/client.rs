use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, endpoint::presets};
use iroh_blobs::{
    BlobFormat, api::remote::GetProgressItem, format::collection::Collection, store::fs::FsStore,
    ticket::BlobTicket,
};
use n0_future::StreamExt;

use crate::torrent::human;

pub async fn run(ticket: BlobTicket, out: PathBuf) -> Result<()> {
    ensure!(
        ticket.format() == BlobFormat::HashSeq,
        "this ticket is for a single blob, not a deaddrop collection"
    );
    tokio::fs::create_dir_all(&out).await?;

    // Download into a temporary store inside the output directory (same
    // filesystem, so exports are cheap), then export the files and delete it.
    let store_dir = out.join(format!(".deaddrop-{}", ticket.hash().fmt_short()));
    let res = fetch(&ticket, &out, &store_dir).await;
    tokio::fs::remove_dir_all(&store_dir).await.ok();
    res
}

async fn fetch(ticket: &BlobTicket, out: &Path, store_dir: &Path) -> Result<()> {
    let store = FsStore::load(store_dir).await?;
    let endpoint = Endpoint::builder(presets::N0).bind().await?;

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
                bail!("download failed: {e}");
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

    let collection = Collection::load(ticket.hash(), &*store).await?;
    for (name, hash) in collection.iter() {
        let target = safe_join(out, name)?;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        store
            .blobs()
            .export(*hash, &target)
            .finish()
            .await
            .with_context(|| format!("failed to write {}", target.display()))?;
        eprintln!("wrote {}", target.display());
    }

    endpoint.close().await;
    store.shutdown().await.ok();
    Ok(())
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
