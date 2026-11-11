use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use iroh::{Endpoint, endpoint::presets, protocol::Router};
use iroh_blobs::{
    BlobFormat, BlobsProtocol,
    api::{
        TempTag,
        blobs::{AddPathOptions, ImportMode},
    },
    format::collection::Collection,
    store::fs::FsStore,
    ticket::BlobTicket,
};

use crate::{
    notify::{self, NotifierConfig, Notifiers},
    single_use, torrent,
};

pub struct ServeOptions {
    /// Magnet URI, .torrent URL, or local .torrent path.
    pub source: String,
    pub single_use: bool,
    pub data_dir: Option<PathBuf>,
    pub peers: Vec<SocketAddr>,
    pub notify: Vec<NotifierConfig>,
}

pub async fn run(opts: ServeOptions) -> Result<()> {
    // Check notifiers before doing anything slow.
    let notifiers = Notifiers::new(&opts.notify)?;

    // If no data dir was given, use a temporary one and remove it on exit.
    let (data_dir, ephemeral) = match opts.data_dir.clone() {
        Some(d) => (d, false),
        None => (temp_dir(), true),
    };
    let res = serve(&opts, &notifiers, &data_dir).await;
    if ephemeral && let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
        eprintln!("warning: failed to remove {}: {e}", data_dir.display());
    }
    res
}

async fn serve(
    opts: &ServeOptions,
    notifiers: &Notifiers,
    data_dir: &std::path::Path,
) -> Result<()> {
    let single_use = opts.single_use;
    let torrent_dir = data_dir.join("torrent");
    let store_dir = data_dir.join("blobs");

    // 1. Download the torrent (leech only), then stop the torrent session.
    let torrent::Download { name, files } =
        torrent::download(&opts.source, &torrent_dir, opts.peers.clone()).await?;
    if files.is_empty() {
        anyhow::bail!("the torrent has no files");
    }

    // 2. Import the files into an iroh-blobs store. TryReference keeps them in
    //    place instead of copying, so we don't use twice the disk space.
    let store = FsStore::load(&store_dir)
        .await
        .context("failed to open the blob store")?;
    let mut tags: Vec<TempTag> = Vec::with_capacity(files.len());
    let mut collection = Collection::default();
    let mut total = 0u64;
    for f in &files {
        eprintln!("import: {}", f.name);
        total += tokio::fs::metadata(&f.path).await?.len();
        let tag = store
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: f.path.clone(),
                format: BlobFormat::Raw,
                mode: ImportMode::TryReference,
            })
            .temp_tag()
            .await
            .with_context(|| format!("failed to import {}", f.path.display()))?;
        collection.push(f.name.clone(), tag.hash());
        tags.push(tag);
    }
    let root = collection.store(&store).await?;
    let root_hash = root.hash();

    // 3. Serve the collection over iroh.
    let endpoint = Endpoint::builder(presets::N0).bind().await?;
    let (events, mut done) = if single_use {
        let (events, done) = single_use::watcher(root_hash);
        (Some(events), Some(done))
    } else {
        (None, None)
    };
    let blobs = BlobsProtocol::new(&store, events);
    let router = Router::builder(endpoint)
        .accept(iroh_blobs::ALPN, blobs)
        .spawn();
    router.endpoint().online().await;

    let ticket = BlobTicket::new(router.endpoint().addr(), root_hash, BlobFormat::HashSeq);
    eprintln!();
    eprintln!(
        "serving {} file(s), {} total{}",
        files.len(),
        torrent::human(total),
        if single_use {
            " (single use)"
        } else {
            " (multi use)"
        }
    );
    eprintln!("on the client, run:");
    eprintln!();
    eprintln!("    deaddrop fetch {ticket}");
    eprintln!();
    // Print the bare ticket on stdout for scripting. If stdout is a closed
    // pipe, keep serving: the ticket was already shown on stderr.
    crate::write_stdout(&format!("{ticket}\n")).ok();

    // 4. Tell anyone who should know.
    if !notifiers.is_empty() {
        let event = notify::Event::TicketReady(notify::TicketReady {
            name,
            ticket: ticket.to_string(),
            fetch_command: format!("deaddrop fetch {ticket}"),
            files: files.len(),
            total_bytes: total,
            single_use,
        });
        notifiers.send_all(&event).await;
    }

    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("interrupted, shutting down"),
        _ = async { match done.as_mut() { Some(d) => { d.await.ok(); } None => std::future::pending().await } } => {
            eprintln!("single-use download finished, shutting down");
        }
    }

    router.shutdown().await.ok();
    drop(root);
    drop(tags);
    store.shutdown().await.ok();
    Ok(())
}

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("deaddrop-{}-{nanos}", std::process::id()))
}
