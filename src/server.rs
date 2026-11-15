use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use iroh::{EndpointId, RelayMode, protocol::Router};
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
    net::Refusal,
    notify::{self, BoxFuture, NotifierConfig, Notifiers},
    reset,
    source::{self, Source},
    torrent,
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
    /// If not empty, only these clients may download (or list).
    pub allow: Vec<EndpointId>,
    /// Stop serving each ticket this long after it is announced.
    pub expire_after: Option<Duration>,
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
    // Absolute, so local paths can be checked against it: deaddrop deletes
    // things in its data dir, so it must never serve files from there.
    tokio::fs::create_dir_all(&data_dir)
        .await
        .with_context(|| format!("can't create the data dir {}", data_dir.display()))?;
    let data_dir = tokio::fs::canonicalize(&data_dir).await?;
    let res = serve(opts, &notifiers, &data_dir).await;
    if ephemeral && let Err(e) = tokio::fs::remove_dir_all(&data_dir).await {
        say!("warning: failed to remove {}: {e}", data_dir.display());
    }
    res
}

/// Data that has been downloaded (or found on disk) and imported, ready to
/// serve.
struct Served {
    name: String,
    files: usize,
    total: u64,
    /// Keeps the collection alive in the store.
    root: TempTag,
    /// Keep the file blobs alive in the store.
    tags: Vec<TempTag>,
    /// deaddrop's working dir for this data (downloads, encrypted copies).
    /// Deleted on retire. Local files served in place are never in here.
    dir: PathBuf,
    /// Whether the served files are deaddrop's own (a torrent, stdin, a
    /// download) rather than the user's files, served in place.
    owned: bool,
    /// Decrypts this collection, if it is encrypted. Only kept in memory.
    key: Option<crypt::Key>,
}

/// A reset whose torrent is still downloading.
struct PendingReset<'a> {
    reply: oneshot::Sender<Result<reset::NewTicket, String>>,
    _busy: reset::BusyGuard,
    work: BoxFuture<'a, Result<Served>>,
    /// Where the new torrent is downloading, to clean up if the server exits.
    dir: PathBuf,
}

async fn serve(opts: ServeOptions, notifiers: &Notifiers, data_dir: &Path) -> Result<()> {
    let single_use = opts.single_use;
    let encrypt = opts.encrypt;
    let hide_key = opts.hide_key;
    let expire_after = opts.expire_after;
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
        data_dir,
    )
    .await?;

    // 3. Serve the collection over iroh.
    let endpoint = crate::net::bind_with(&opts.relay, None, &opts.allow).await?;
    if !opts.allow.is_empty() {
        say!(
            "only accepting downloads from {} allowed client(s)",
            opts.allow.len()
        );
    }
    let (events, gate, mut done) =
        gate::watcher(first.root.hash(), first.total, single_use, encrypt);
    let blobs = BlobsProtocol::new(&store, Some(events));
    let mut router = Router::builder(endpoint)
        .accept(iroh_blobs::ALPN, gate::GatedBlobs::new(blobs, gate.clone()));
    let mut jobs = None;
    if let Some(token) = opts.reset_token {
        let (handler, rx) = reset::handler(token);
        router = router.accept(reset::ALPN, handler);
        jobs = Some(rx);
    }
    let router = router.spawn();
    router.endpoint().online().await;

    // 4. Print the ticket and tell anyone who should know.
    let mut expires = expire_after.map(Expiry::start);
    let mut ticket = announce(
        &router,
        &first,
        notifiers,
        single_use,
        false,
        hide_key,
        expires.as_ref(),
    )
    .await;
    if jobs.is_some() {
        say!("resets are enabled: `deaddrop reset <ticket> <source>` with the reset token");
    }
    let mut current = Some(first);

    let mut generation = 0u64;
    let mut pending: Option<PendingReset> = None;
    // Set when a single-use download finished and the server exits.
    let mut finished = false;
    // Set when the ticket expired and the server exits; deleted after the
    // loop, once a download still in progress is done.
    let mut expired: Option<Served> = None;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                say!("interrupted, shutting down");
                break;
            }
            _ = Expiry::wait(&expires), if current.is_some() => {
                let after = expires.take().expect("guarded").after;
                gate.stop(Refusal::Expired);
                let old = current.take().expect("guarded");
                say!("ticket expired after {}; it no longer works", fmt_duration(after));
                if opts.wait_for_reset {
                    say!("deleting the data and waiting for a reset");
                    say!("to serve something else: deaddrop reset {ticket} <source>");
                    tokio::spawn(retire(old, gate.clone()));
                    continue;
                }
                expired = Some(old);
                break;
            }
            Some(root) = done.recv() => {
                // A stale report for a collection a reset already replaced.
                if current.as_ref().map(|c| c.root.hash()) != Some(root) {
                    continue;
                }
                gate.stop(Refusal::Downloaded);
                expires = None;
                let old = current.take().expect("checked above");
                if !opts.wait_for_reset {
                    // The data is no longer needed, even in a kept data dir.
                    say!("single-use download finished; deleting the data and shutting down");
                    retire(old, gate.clone()).await;
                    finished = true;
                    break;
                }
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
                    work: Box::pin(prepare_reset(&store, source, dir.clone(), &opts.peers, encrypt, data_dir)),
                    dir,
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
                        // Each ticket gets the full time.
                        expires = expire_after.map(Expiry::start);
                        ticket = announce(
                            &router,
                            &new,
                            notifiers,
                            single_use,
                            true,
                            hide_key,
                            expires.as_ref(),
                        )
                        .await;
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

    // A half-done reset would never be served; don't leave it on disk.
    if let Some(p) = pending.take() {
        drop(p.work);
        remove_dir(&p.dir).await;
    }
    // An expired ticket serves no new requests, but a download that was
    // already running may finish (until Ctrl-C).
    if let Some(old) = &expired
        && gate.is_busy(old.root.hash())
    {
        say!("letting the download in progress finish (Ctrl-C to cut it off)");
        tokio::select! {
            _ = gate.wait_idle(old.root.hash()) => {}
            _ = tokio::signal::ctrl_c() => say!("interrupted, cutting off the download"),
        }
    }
    router.shutdown().await.ok();
    if let Some(old) = expired {
        // The connections are closed, so this doesn't wait long.
        say!("deleting the data and shutting down");
        retire(old, gate.clone()).await;
        finished = true;
    }
    drop(current);
    store.shutdown().await.ok();
    if finished {
        // GC never got to run on the retired collection, and the store keeps
        // small files (and the encrypted manifest) inline in its database.
        // Nothing in it is served any more, so remove it too.
        remove_dir(&store_dir).await;
    }
    Ok(())
}

async fn recv_job(jobs: &mut Option<mpsc::Receiver<reset::Job>>) -> Option<reset::Job> {
    match jobs {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Get the source's files and import them into the store, encrypted or not.
///
/// Layout under `dir`: `plain/` holds what deaddrop downloads (a torrent,
/// stdin, an http(s) file); local files are served from where they are and
/// never copied there. Without encryption the store references the files in
/// place. With encryption, `plain/` is deleted as soon as the files are
/// encrypted (or encryption fails), and `enc/` holds one age file per file,
/// which the store references in place. The user's own files are left
/// alone either way.
async fn prepare(
    store: &FsStore,
    source: Source,
    dir: PathBuf,
    peers: &[SocketAddr],
    encrypt: bool,
    data_dir: &Path,
) -> Result<Served> {
    let plain = dir.join("plain");
    let fetched = match source::fetch(source, &plain, peers, data_dir).await {
        Ok(f) => f,
        Err(e) => {
            // Don't leave a partial download in a kept data dir.
            remove_dir(&plain).await;
            return Err(e);
        }
    };
    if fetched.files.is_empty() {
        remove_dir(&plain).await;
        anyhow::bail!("nothing to serve: {} has no files", fetched.name);
    }
    if !encrypt {
        return import_plain(store, fetched, dir).await;
    }
    let res = encrypt_and_import(store, fetched, dir).await;
    // Never leave plaintext behind, whether or not encryption worked.
    remove_dir(&plain).await;
    res
}

/// Import the downloaded files as they are, as a collection named with
/// their real paths. The files stay on disk until the collection is retired.
async fn import_plain(store: &FsStore, fetched: source::Fetched, dir: PathBuf) -> Result<Served> {
    let source::Fetched { name, files, owned } = fetched;
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
        owned,
        key: None,
    })
}

async fn encrypt_and_import(
    store: &FsStore,
    fetched: source::Fetched,
    dir: PathBuf,
) -> Result<Served> {
    let source::Fetched { name, files, owned } = fetched;
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
        // Delete each downloaded plaintext file as soon as it is encrypted,
        // so the extra disk space needed is about one file. The user's own
        // files are never deleted.
        if owned {
            tokio::fs::remove_file(&f.path).await.ok();
        }
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
        owned,
        key: Some(key),
    })
}

/// Like [`prepare`], but starts from an empty directory and removes it again
/// if the download fails. Resets only ever bring in torrents: a reset
/// request must not make the server read its own files or fetch arbitrary
/// URLs.
async fn prepare_reset(
    store: &FsStore,
    source: torrent::Source,
    dir: PathBuf,
    peers: &[SocketAddr],
    encrypt: bool,
    data_dir: &Path,
) -> Result<Served> {
    say!("reset: downloading a {}", source.describe());
    // Left over from an earlier run with the same data dir; never served now.
    remove_dir(&dir).await;
    let res = prepare(
        store,
        Source::Torrent(source),
        dir.clone(),
        peers,
        encrypt,
        data_dir,
    )
    .await;
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
        owned,
        ..
    } = old;
    drop(root);
    drop(tags);
    remove_dir(&dir).await;
    if owned {
        say!("deleted the files of {name}");
    } else {
        say!("stopped serving {name} (the original files are left as they are)");
    }
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
    expires: Option<&Expiry>,
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
    if let Some(e) = expires {
        say!(
            "this ticket expires in {}, unless it is used up first",
            fmt_duration(e.after)
        );
    }
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
            expires_at: expires.map(|e| e.unix_secs()),
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

/// When the current ticket stops working.
struct Expiry {
    /// How long tickets last.
    after: Duration,
    deadline: tokio::time::Instant,
    /// The same moment on the wall clock, for notifications.
    at: SystemTime,
}

impl Expiry {
    fn start(after: Duration) -> Self {
        Self {
            after,
            deadline: tokio::time::Instant::now() + after,
            at: SystemTime::now() + after,
        }
    }

    /// Resolves at the deadline; never, without one.
    async fn wait(expiry: &Option<Self>) {
        match expiry {
            Some(e) => tokio::time::sleep_until(e.deadline).await,
            None => std::future::pending().await,
        }
    }

    fn unix_secs(&self) -> u64 {
        self.at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }
}

/// Parse a duration like `90s`, `30m`, `24h`, `7d` or `1h30m`. A bare
/// number is seconds. `0`, `never` and `off` mean no expiry (`None`).
pub fn parse_duration(s: &str) -> Result<Option<Duration>> {
    let s = s.trim();
    if matches!(
        s.to_ascii_lowercase().as_str(),
        "0" | "never" | "off" | "none"
    ) {
        return Ok(None);
    }
    let bad = || format!("invalid duration {s:?} (use e.g. 90s, 30m, 24h, 7d or 1h30m)");
    anyhow::ensure!(!s.is_empty(), "{}", bad());
    let mut total: u64 = 0;
    let mut rest = s;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        anyhow::ensure!(digits > 0, "{}", bad());
        let n: u64 = rest[..digits].parse().with_context(bad)?;
        rest = &rest[digits..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let unit = match &rest[..unit_len] {
            // A bare number is seconds, but only on its own.
            "" if digits == s.len() => 1,
            "s" => 1,
            "m" => 60,
            "h" => 60 * 60,
            "d" => 24 * 60 * 60,
            _ => anyhow::bail!("{}", bad()),
        };
        rest = &rest[unit_len..];
        total = n
            .checked_mul(unit)
            .and_then(|v| total.checked_add(v))
            .with_context(bad)?;
    }
    // tokio panics on deadlines too far in the future; ten years is plenty.
    anyhow::ensure!(
        total <= 10 * 365 * 24 * 60 * 60,
        "duration {s:?} is too long"
    );
    Ok(Some(Duration::from_secs(total)))
}

/// Show a duration the way it's written: `1d2h`, `30m`, `90s` → `1m30s`.
pub fn fmt_duration(d: Duration) -> String {
    let mut secs = d.as_secs();
    if secs == 0 {
        return "0s".into();
    }
    let mut out = String::new();
    for (unit, len) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        if secs >= len {
            out.push_str(&format!("{}{unit}", secs / len));
            secs %= len;
        }
    }
    out
}

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("deaddrop-{}-{nanos}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        let p = |s: &str| parse_duration(s).unwrap().map(|d| d.as_secs());
        assert_eq!(p("90"), Some(90));
        assert_eq!(p("90s"), Some(90));
        assert_eq!(p("30m"), Some(1800));
        assert_eq!(p("24h"), Some(86_400));
        assert_eq!(p("7d"), Some(604_800));
        assert_eq!(p("1h30m"), Some(5400));
        assert_eq!(p(" 2d12h "), Some(216_000));
        assert_eq!(p("0"), None);
        assert_eq!(p("never"), None);
        assert_eq!(p("OFF"), None);
        for bad in [
            "",
            "h",
            "5x",
            "1h30",
            "-5m",
            "1.5h",
            "30 m",
            "99999999999999d",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn durations_print() {
        let f = |s| fmt_duration(Duration::from_secs(s));
        assert_eq!(f(90), "1m30s");
        assert_eq!(f(86_400), "1d");
        assert_eq!(f(93_600), "1d2h");
        assert_eq!(f(0), "0s");
    }
}
