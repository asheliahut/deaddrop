use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use iroh::{RelayMode, protocol::Router};
use iroh_blobs::{
    BlobFormat, BlobsProtocol,
    api::{
        TempTag,
        blobs::{AddPathOptions, ImportMode},
    },
    format::collection::Collection,
    store::{
        GcConfig,
        fs::{FsStore, options::Options},
    },
    ticket::BlobTicket,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    crypt, gate,
    notify::{self, BoxFuture, NotifierConfig, Notifiers},
    reset,
    torrent::{self, Source},
};

pub struct ServeOptions {
    pub source: Source,
    pub single_use: bool,
    pub data_dir: Option<PathBuf>,
    pub peers: Vec<SocketAddr>,
    pub notify: Vec<NotifierConfig>,
    /// Lets clients with this token replace the torrent.
    pub reset_token: Option<String>,
    /// Single use: after the download, delete the data and wait for a reset
    /// instead of exiting.
    pub wait_for_reset: bool,
    /// Encrypt the files with age before serving them. iroh already
    /// encrypts the connection; this also protects the data at rest and
    /// from anyone who has only the ticket.
    pub encrypt: bool,
    /// Never print the decryption key or send it to reset requesters; only
    /// notifiers with include_key get it.
    pub hide_key: bool,
    /// Relay servers to use.
    pub relay: RelayMode,
}

/// How often the blob store drops blobs that nothing references any more
/// (after a reset or a finished single-use download).
const GC_INTERVAL: Duration = Duration::from_secs(10);

pub async fn run(mut opts: ServeOptions) -> Result<()> {
    if opts.wait_for_reset {
        if !opts.single_use {
            say!("warning: ignoring --wait-for-reset; a multi-use server already keeps running");
            opts.wait_for_reset = false;
        } else if opts.reset_token.is_none() {
            anyhow::bail!(
                "--wait-for-reset needs a reset token (--reset-token or DEADDROP_RESET_TOKEN), \
                 otherwise nothing could be served after the first download"
            );
        }
    }
    if let Some(token) = &opts.reset_token {
        reset::check_token(token)?;
    }
    // Check notifiers before doing anything slow.
    let notifiers = Notifiers::new(&opts.notify)?;
    // hide_key only matters with encryption: without it there is no key.
    opts.hide_key &= opts.encrypt;
    if opts.hide_key && !notifiers.sends_key() {
        anyhow::bail!(
            "--hide-key needs a notifier that gets the key (include_key = true, or \
             `+key` with --notify), otherwise nobody could decrypt the data"
        );
    }

    // If no data dir was given, use a temporary one and remove it on exit.
    let (data_dir, ephemeral) = match opts.data_dir.clone() {
        Some(d) => (d, false),
        None => (temp_dir(), true),
    };
    let res = serve(opts, &notifiers, &data_dir).await;
    if ephemeral && let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
        say!("warning: failed to remove {}: {e}", data_dir.display());
    }
    res
}

/// A torrent that has been downloaded and imported, ready to serve.
struct Served {
    name: String,
    files: usize,
    total: u64,
    /// Keeps the collection alive in the store.
    root: TempTag,
    /// Keep the file blobs alive in the store.
    tags: Vec<TempTag>,
    /// Where the torrent's files are on disk.
    dir: PathBuf,
    /// Decrypts this collection, if it is encrypted. Only kept in memory.
    key: Option<crypt::Key>,
}

/// A reset whose torrent is still downloading.
struct PendingReset<'a> {
    reply: oneshot::Sender<Result<reset::NewTicket, String>>,
    _busy: reset::BusyGuard,
    work: BoxFuture<'a, Result<Served>>,
}

async fn serve(opts: ServeOptions, notifiers: &Notifiers, data_dir: &Path) -> Result<()> {
    let single_use = opts.single_use;
    let encrypt = opts.encrypt;
    let hide_key = opts.hide_key;
    let store_dir = data_dir.join("blobs");

    // 1-2. Download the torrent (leech only) and import it into the store.
    // GC removes what retired collections leave behind in the store; their
    // torrent files (imported by reference) are deleted by `retire`.
    let store_opts = Options {
        gc: Some(GcConfig {
            interval: GC_INTERVAL,
            add_protected: None,
        }),
        ..Options::new(&store_dir)
    };
    let store = FsStore::load_with_opts(store_dir.join("blobs.db"), store_opts)
        .await
        .context("failed to open the blob store")?;
    let first = prepare(
        &store,
        opts.source,
        data_dir.join("torrent"),
        &opts.peers,
        encrypt,
    )
    .await?;

    // 3. Serve the collection over iroh.
    let endpoint = crate::net::bind(&opts.relay).await?;
    let (events, gate, mut done) = gate::watcher(first.root.hash(), first.total, single_use);
    let blobs = BlobsProtocol::new(&store, Some(events));
    let mut router = Router::builder(endpoint).accept(iroh_blobs::ALPN, blobs);
    let mut jobs = None;
    if let Some(token) = opts.reset_token {
        let (handler, rx) = reset::handler(token);
        router = router.accept(reset::ALPN, handler);
        jobs = Some(rx);
    }
    let router = router.spawn();
    router.endpoint().online().await;

    // 4. Print the ticket and tell anyone who should know.
    let mut ticket = announce(&router, &first, notifiers, single_use, false, hide_key).await;
    if jobs.is_some() {
        say!("resets are enabled: `deaddrop reset <ticket> <source>` with the reset token");
    }
    let mut current = Some(first);

    let mut generation = 0u64;
    let mut pending: Option<PendingReset> = None;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                say!("interrupted, shutting down");
                break;
            }
            Some(root) = done.recv() => {
                // A stale report for a collection a reset already replaced.
                if current.as_ref().map(|c| c.root.hash()) != Some(root) {
                    continue;
                }
                if !opts.wait_for_reset {
                    say!("single-use download finished, shutting down");
                    break;
                }
                gate.set_root(None);
                let old = current.take().expect("checked above");
                say!("single-use download finished; deleting the data and waiting for a reset");
                say!("to serve something else: deaddrop reset {ticket} <source>");
                tokio::spawn(retire(old, gate.clone()));
            }
            Some(job) = recv_job(&mut jobs), if pending.is_none() => {
                generation += 1;
                let reset::Job { source, requester, reply, busy } = job;
                if current.is_some() {
                    say!("reset: requested by {requester}; still serving the current torrent until the new one is ready");
                } else {
                    say!("reset: requested by {requester}");
                }
                let dir = data_dir.join(format!("torrent-{generation}"));
                pending = Some(PendingReset {
                    reply,
                    _busy: busy,
                    work: Box::pin(prepare_reset(&store, source, dir, &opts.peers, encrypt)),
                });
            }
            res = async { pending.as_mut().expect("guarded").work.as_mut().await }, if pending.is_some() => {
                let PendingReset { reply, .. } = pending.take().expect("guarded");
                match res {
                    Ok(new) => {
                        gate.set_root(Some((new.root.hash(), new.total)));
                        if let Some(old) = current.take() {
                            say!("reset: old ticket {ticket} no longer works");
                            tokio::spawn(retire(old, gate.clone()));
                        }
                        ticket = announce(&router, &new, notifiers, single_use, true, hide_key).await;
                        current = Some(new);
                        let key = if hide_key {
                            None
                        } else {
                            current.as_ref().expect("just set").key.as_ref().map(|k| k.expose())
                        };
                        reply.send(Ok(reset::NewTicket {
                            ticket: ticket.to_string(),
                            key,
                            key_hidden: hide_key,
                        })).ok();
                    }
                    Err(e) => {
                        let what = if current.is_some() { "still serving the old torrent" } else { "still waiting" };
                        say!("reset: failed, {what}: {e:#}");
                        reply.send(Err(format!("{e:#}"))).ok();
                    }
                }
            }
        }
    }

    drop(pending);
    router.shutdown().await.ok();
    drop(current);
    store.shutdown().await.ok();
    Ok(())
}

async fn recv_job(jobs: &mut Option<mpsc::Receiver<reset::Job>>) -> Option<reset::Job> {
    match jobs {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Download a torrent and import it into the store, encrypted or not.
///
/// Layout under `dir`: `plain/` holds the torrent download. Without
/// encryption the store references those files in place. With encryption,
/// `plain/` is deleted as soon as the files are encrypted (or encryption
/// fails), and `enc/` holds one age file per torrent file, which the store
/// references in place.
async fn prepare(
    store: &FsStore,
    source: Source,
    dir: PathBuf,
    peers: &[SocketAddr],
    encrypt: bool,
) -> Result<Served> {
    let plain = dir.join("plain");
    let torrent::Download { name, files } =
        torrent::download(source, &plain, peers.to_vec()).await?;
    if files.is_empty() {
        remove_dir(&plain).await;
        anyhow::bail!("the torrent has no files");
    }
    if !encrypt {
        return import_plain(store, name, files, dir).await;
    }
    let res = encrypt_and_import(store, name, files, dir).await;
    // Never leave plaintext behind, whether or not encryption worked.
    remove_dir(&plain).await;
    res
}

/// Import the downloaded files as they are, as a collection named with
/// their real paths. The files stay on disk until the collection is retired.
async fn import_plain(
    store: &FsStore,
    name: String,
    files: Vec<torrent::DownloadedFile>,
    dir: PathBuf,
) -> Result<Served> {
    let mut tags: Vec<TempTag> = Vec::with_capacity(files.len());
    let mut collection = Collection::default();
    let mut total = 0u64;
    for f in &files {
        say!("import: {}", f.name);
        total += tokio::fs::metadata(&f.path).await?.len();
        // TryReference keeps the file in place instead of copying.
        let tag = store
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: f.path.clone(),
                format: BlobFormat::Raw,
                mode: ImportMode::TryReference,
            })
            .temp_tag()
            .await
            .with_context(|| format!("failed to import {}", f.name))?;
        collection.push(f.name.clone(), tag.hash());
        tags.push(tag);
    }
    let root = collection.store(store).await?;
    Ok(Served {
        name,
        files: files.len(),
        total,
        root,
        tags,
        dir,
        key: None,
    })
}

async fn encrypt_and_import(
    store: &FsStore,
    name: String,
    files: Vec<torrent::DownloadedFile>,
    dir: PathBuf,
) -> Result<Served> {
    let enc = dir.join("enc");
    // Left over from an earlier run with the same data dir.
    remove_dir(&enc).await;
    tokio::fs::create_dir_all(&enc).await?;

    let key = crypt::Key::generate();
    let recipient = key.recipient();
    let mut tags: Vec<TempTag> = Vec::with_capacity(files.len() + 1);
    let mut entries = Vec::with_capacity(files.len());
    let mut total = 0u64;
    for (i, f) in files.iter().enumerate() {
        say!("encrypt: {}", f.name);
        let dst = enc.join(format!("{i}.age"));
        total += tokio::task::spawn_blocking({
            let (recipient, src, dst) = (recipient.clone(), f.path.clone(), dst.clone());
            move || crypt::encrypt_file(&recipient, &src, &dst)
        })
        .await??;
        // Delete each plaintext file as soon as it is encrypted, so the
        // extra disk space needed is about one file.
        tokio::fs::remove_file(&f.path).await.ok();
        // TryReference keeps the encrypted file in place instead of copying.
        let tag = store
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: dst,
                format: BlobFormat::Raw,
                mode: ImportMode::TryReference,
            })
            .temp_tag()
            .await
            .with_context(|| format!("failed to import {}", f.name))?;
        // Entries are named by index; the real names are in the manifest.
        entries.push((i.to_string(), tag.hash()));
        tags.push(tag);
    }

    let manifest =
        crypt::Manifest::new(name.clone(), files.iter().map(|f| f.name.clone()).collect())
            .encrypt(&recipient)?;
    let manifest_tag = store.blobs().add_bytes(manifest).temp_tag().await?;
    let mut collection = Collection::default();
    collection.push(crypt::MANIFEST_NAME.to_string(), manifest_tag.hash());
    for (entry, hash) in entries {
        collection.push(entry, hash);
    }
    tags.push(manifest_tag);
    let root = collection.store(store).await?;
    Ok(Served {
        name,
        files: files.len(),
        total,
        root,
        tags,
        dir,
        key: Some(key),
    })
}

/// Like [`prepare`], but starts from an empty directory and removes it again
/// if the download fails.
async fn prepare_reset(
    store: &FsStore,
    source: Source,
    dir: PathBuf,
    peers: &[SocketAddr],
    encrypt: bool,
) -> Result<Served> {
    say!("reset: downloading a {}", source.describe());
    // Left over from an earlier run with the same data dir; never served now.
    remove_dir(&dir).await;
    let res = prepare(store, source, dir.clone(), peers, encrypt).await;
    if res.is_err() {
        remove_dir(&dir).await;
    }
    res
}

/// After a reset or a finished single-use download: wait until nobody is
/// downloading the collection, then release it and delete its files. GC
/// removes what is left of it in the blob store.
async fn retire(old: Served, gate: gate::Gate) {
    gate.wait_idle(old.root.hash()).await;
    let Served {
        root,
        tags,
        dir,
        name,
        ..
    } = old;
    drop(root);
    drop(tags);
    remove_dir(&dir).await;
    say!("deleted the files of {name}");
}

async fn remove_dir(dir: &Path) {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => say!("warning: failed to remove {}: {e}", dir.display()),
    }
}

/// Print the ticket for `served` and send it to the notifiers.
async fn announce(
    router: &Router,
    served: &Served,
    notifiers: &Notifiers,
    single_use: bool,
    reset: bool,
    hide_key: bool,
) -> BlobTicket {
    let ticket = BlobTicket::new(
        router.endpoint().addr(),
        served.root.hash(),
        BlobFormat::HashSeq,
    );
    say!();
    say!(
        "{}serving {} file(s), {} total{}",
        if reset { "reset: now " } else { "" },
        served.files,
        torrent::human(served.total),
        if single_use {
            " (single use)"
        } else {
            " (multi use)"
        }
    );
    let key = served.key.as_ref().map(|k| k.expose());
    // If stdout is a closed pipe, keep serving: it was shown on stderr too.
    let shown_key = if hide_key { None } else { key.as_deref() };
    crate::print_ticket(&ticket.to_string(), shown_key).ok();
    if hide_key {
        say!(
            "the decryption key is not shown (hide_key); it goes only to notifiers with include_key"
        );
    }

    if !notifiers.is_empty() {
        let event = notify::Event::TicketReady(notify::TicketReady {
            name: served.name.clone(),
            ticket: ticket.to_string(),
            fetch_command: format!("deaddrop fetch {ticket}"),
            files: served.files,
            total_bytes: served.total,
            single_use,
            reset,
            key: None,
        });
        let sent = notifiers.send_all(&event, key.as_deref()).await;
        if hide_key && sent.with_key == 0 {
            say!(
                "error: no notifier received the decryption key, and hide_key keeps it from \
                 being shown, so nobody can decrypt this data. Fix the notifiers and reset \
                 the server (or restart it) to get a new key."
            );
        }
    }
    ticket
}

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("deaddrop-{}-{nanos}", std::process::id()))
}
