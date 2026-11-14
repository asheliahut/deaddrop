use std::{
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, EndpointId, RelayMode, SecretKey, endpoint::Connection};
use iroh_blobs::{
    BlobFormat, Hash,
    api::remote::GetProgressItem,
    format::collection::Collection,
    get::request::get_hash_seq_and_sizes,
    protocol::GetRequest,
    store::{fs::FsStore, mem::MemStore},
    ticket::BlobTicket,
};
use n0_future::StreamExt;
use tokio_util::io::SyncIoBridge;

use crate::{
    crypt::{self, Key, Manifest},
    gate,
    net::Refusal,
    torrent::human,
    ui,
};

/// Largest hash seq the size probe accepts: 32 bytes per file, so this
/// allows about a million files.
const MAX_HASH_SEQ_BYTES: u64 = 32 * 1024 * 1024;

/// Why a download can keep failing, shown when the client gives up. A 2.1+
/// server says why it turns a client away (see [`Refused`]); an older one
/// only says "permission".
const GAVE_UP: &str = "the download did not finish. If the server refused it \
    (\"permission\"): another client may be downloading from this single-use server, \
    its one download may be done already, or it was reset to a different torrent and \
    this ticket no longer works";

/// The server turned this client away, and said why.
#[derive(Debug)]
struct Refused {
    why: Refusal,
    /// This client's persistent identity (from `deaddrop id`), if it used one.
    me: Option<EndpointId>,
    /// How long the client waited for a busy slot before giving up.
    waited: Option<Duration>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.why, &self.me) {
            (Refusal::NotAllowed, Some(id)) => write!(
                f,
                "the server only accepts specific clients, and this one ({id}) isn't on its \
                 allow list. Ask the server's operator to add it with `--allow {id}`."
            ),
            (Refusal::NotAllowed, None) => f.write_str(
                "the server only accepts specific clients, and this fetch had no identity \
                 (it used a new random id). Run `deaddrop id` to create one, give the id it \
                 prints to the server's operator, and fetch again once they have added it.",
            ),
            (Refusal::Busy, _) => {
                f.write_str(
                    "another client is downloading from this single-use server, so it turned \
                     this one away",
                )?;
                if let Some(w) = self.waited {
                    write!(f, " (waited {}s for the slot to free up)", w.as_secs())?;
                }
                f.write_str(
                    ". If that download fails partway, the server lets the next client in: \
                     run the same command again later.",
                )
            }
            (Refusal::Downloaded, _) => f.write_str(
                "this single-use drop has already been downloaded (by another client, or an \
                 earlier run), and the server deletes the data afterwards. Ask the sender \
                 for a new ticket.",
            ),
            (Refusal::Expired, _) => f.write_str(
                "this ticket expired before it was downloaded, and the server deleted the \
                 data. Ask the sender for a new ticket.",
            ),
            (Refusal::Replaced, _) => f.write_str(
                "this ticket no longer works: the server was reset to serve something else. \
                 Ask the sender for the new ticket.",
            ),
            (Refusal::Unknown, _) => f.write_str(
                "the server isn't serving this ticket. Check that it was copied completely \
                 and is the latest one.",
            ),
        }
    }
}

impl std::error::Error for Refused {}

/// The server's reason for turning this client away, if it gave one.
fn refusal(err: &anyhow::Error) -> Option<Refusal> {
    err.downcast_ref::<Refused>().map(|r| r.why)
}

/// Bind the client's endpoint, with its persistent identity if it has one
/// (returned too, for error messages).
async fn bind(
    relay: &RelayMode,
    identity: Option<SecretKey>,
) -> Result<(Endpoint, Option<EndpointId>)> {
    let persistent = identity.is_some();
    let endpoint = crate::net::bind_with(relay, identity, &[]).await?;
    let id = persistent.then(|| endpoint.id());
    if let Some(id) = id {
        say!("using identity {}", id.fmt_short());
    }
    Ok((endpoint, id))
}

/// Turn a failure on `conn` into [`Refused`] if the server closed the
/// connection to turn this client away.
async fn check_refused(
    err: anyhow::Error,
    conn: &Connection,
    me: Option<EndpointId>,
) -> anyhow::Error {
    // The request can fail a moment before the close frame (and its
    // reason) is processed; give it a little time.
    if conn.close_reason().is_none() {
        tokio::time::timeout(Duration::from_millis(500), conn.closed())
            .await
            .ok();
    }
    match Refusal::of(conn) {
        Some(why) => Refused {
            why,
            me,
            waited: None,
        }
        .into(),
        None => err,
    }
}

/// Longest wait between two reconnect attempts.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

pub async fn run(
    ticket: BlobTicket,
    out: PathBuf,
    key: Option<Key>,
    relay: &RelayMode,
    identity: Option<SecretKey>,
    retry_for: Duration,
) -> Result<()> {
    ensure!(
        ticket.format() == BlobFormat::HashSeq,
        "this ticket is for a single blob, not a deaddrop collection"
    );
    tokio::fs::create_dir_all(&out).await?;

    // Download into a temporary store inside the output directory, write
    // (and if needed decrypt) the files out of it, then delete it.
    let store_dir = out.join(format!(".deaddrop-{}", ticket.hash().fmt_short()));
    let (res, have) = fetch(
        &ticket,
        &out,
        &store_dir,
        key.as_ref(),
        relay,
        identity,
        retry_for,
    )
    .await;
    match &res {
        Ok(()) => {
            tokio::fs::remove_dir_all(&store_dir).await.ok();
        }
        // Turned away for good: what was downloaded can never be finished.
        Err(e) if refusal(e).is_some_and(|r| r != Refusal::NotAllowed && !r.is_temporary()) => {
            if tokio::fs::remove_dir_all(&store_dir).await.is_ok() && have > 0 {
                say!("deleted the partial download; it can't be finished");
            }
        }
        // Nothing was downloaded: nothing to keep.
        Err(_) if have == 0 => {
            tokio::fs::remove_dir_all(&store_dir).await.ok();
        }
        // Keep whatever was downloaded: the server may be single use, and a
        // rerun of the same command only fetches what is missing (or, if it
        // is all there, doesn't contact the server). NeedKey already says so.
        Err(e) if !e.is::<NeedKey>() && store_dir.exists() => say!(
            "the partial download is kept in {}; run the same command again to resume \
             (or delete that folder to give up)",
            store_dir.display()
        ),
        Err(_) => {}
    }
    res
}

/// Print the files the server offers, with their sizes, without
/// downloading them. Only metadata is fetched, so this doesn't use up a
/// single-use server's download (from deaddrop 2.1 on: an older server
/// counts it as the download).
pub async fn list(
    ticket: BlobTicket,
    key: Option<Key>,
    relay: &RelayMode,
    identity: Option<SecretKey>,
) -> Result<()> {
    ensure!(
        ticket.format() == BlobFormat::HashSeq,
        "this ticket is for a single blob, not a deaddrop collection"
    );
    let (endpoint, me) = bind(relay, identity).await?;
    let res = async {
        say!("connecting to {}", ticket.addr().id);
        let conn = endpoint
            .connect(ticket.addr().clone(), iroh_blobs::ALPN)
            .await
            .context("failed to connect to the server")?;
        let res = match peek(&conn, &ticket, key.as_ref()).await {
            Ok(l) => Ok(l),
            Err(e) => Err(check_refused(e, &conn, me).await),
        };
        // Close right away, so a single-use server frees its slot.
        conn.close(0u32.into(), b"done");
        res
    }
    .await;
    endpoint.close().await;
    if let Err(e) = &res
        && refusal(e).is_some()
    {
        return res.map(|_| ());
    }
    let listing = res.context(
        "could not list the files. If the server refused (\"permission\"): another \
         client may be downloading from this single-use server, its one download may be \
         done already, or the ticket no longer works after a reset",
    )?;

    let total: u64 = listing.files.iter().map(|(_, size)| size).sum();
    if let Some(name) = &listing.name {
        say!("{name}");
    }
    for (name, size) in &listing.files {
        println!("{:>10}  {name}", human(*size));
    }
    let count = listing.files.len();
    let plural = if count == 1 { "" } else { "s" };
    match (listing.encrypted, listing.name.is_some()) {
        (false, _) => say!("{count} file{plural}, {}", human(total)),
        (true, true) => say!(
            "{count} file{plural}, {} encrypted (the files are a little smaller decrypted)",
            human(total)
        ),
        (true, false) => say!(
            "{count} encrypted file{plural}, {}. The names are encrypted too: pass the \
             decryption key in DEADDROP_KEY or with --key to see them",
            human(total)
        ),
    }
    Ok(())
}

/// What `fetch --list` found.
struct Listing {
    /// The torrent's name, from an encrypted collection's manifest.
    name: Option<String>,
    encrypted: bool,
    /// Name (or, encrypted without a key, index) and size of each file.
    files: Vec<(String, u64)>,
}

/// Read the file names and sizes over `conn`. Uses only requests the server
/// gate counts as peeks: the size probe, the collection's names, and (with
/// a key) the encrypted manifest.
async fn peek(conn: &Connection, ticket: &BlobTicket, key: Option<&Key>) -> Result<Listing> {
    let root = ticket.hash();
    let (_, sizes) = get_hash_seq_and_sizes(conn, &root, MAX_HASH_SEQ_BYTES, None)
        .await
        .context("failed to get the file sizes")?;

    let store = MemStore::new();
    store
        .remote()
        .execute_get(conn.clone(), GetRequest::new(root, gate::names_probe()))
        .await
        .context("failed to get the file names")?;
    let collection = Collection::load(root, &*store).await?;
    // The hash seq's first child is the names blob, then one per file.
    let file_sizes = sizes.get(1..).unwrap_or_default();
    ensure!(
        file_sizes.len() == collection.len(),
        "the server sent {} size(s) for {} file(s)",
        file_sizes.len(),
        collection.len()
    );

    let encrypted =
        matches!(collection.iter().next(), Some((name, _)) if name == crypt::MANIFEST_NAME);
    if !encrypted {
        if key.is_some() {
            say!("note: the data is not encrypted; ignoring the decryption key");
        }
        let files = collection
            .iter()
            .zip(file_sizes)
            .map(|((name, _), size)| (name.clone(), *size))
            .collect();
        return Ok(Listing {
            name: None,
            encrypted,
            files,
        });
    }

    // Encrypted: the entries after the manifest are named by index.
    let file_sizes = &file_sizes[1..];
    let Some(key) = key else {
        let files = (1..)
            .zip(file_sizes)
            .map(|(i, size)| (format!("(encrypted file {i})"), *size))
            .collect();
        return Ok(Listing {
            name: None,
            encrypted,
            files,
        });
    };
    let (_, manifest_hash) = collection.iter().next().context("empty collection")?;
    store
        .remote()
        .execute_get(conn.clone(), GetRequest::new(root, gate::manifest_probe()))
        .await
        .context("failed to get the encrypted file names")?;
    let manifest = store.blobs().get_bytes(*manifest_hash).await?;
    let manifest = Manifest::decrypt(key, &manifest)?;
    ensure!(
        manifest.files.len() == file_sizes.len(),
        "the manifest lists {} file(s) but the collection has {}",
        manifest.files.len(),
        file_sizes.len()
    );
    let files = manifest
        .files
        .into_iter()
        .zip(file_sizes)
        .map(|(name, size)| (name, *size))
        .collect();
    Ok(Listing {
        name: Some(manifest.name),
        encrypted,
        files,
    })
}

/// The data is encrypted but no key was given.
#[derive(Debug)]
struct NeedKey;

impl std::fmt::Display for NeedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the data is encrypted: pass the decryption key from `deaddrop serve` \
             in DEADDROP_KEY or with --key. The download is kept, so running the \
             same command again with the key doesn't need the server.",
        )
    }
}

impl std::error::Error for NeedKey {}

async fn fetch(
    ticket: &BlobTicket,
    out: &Path,
    store_dir: &Path,
    key: Option<&Key>,
    relay: &RelayMode,
    identity: Option<SecretKey>,
    retry_for: Duration,
) -> (Result<()>, u64) {
    let store = match FsStore::load(store_dir).await {
        Ok(s) => s,
        Err(e) => return (Err(e.into()), 0),
    };
    let res = async {
        // Left over from an earlier run that had no key.
        let local = store.remote().local(ticket.hash_and_format()).await?;
        if local.is_complete() {
            say!("already downloaded; not contacting the server");
        } else {
            let (endpoint, me) = bind(relay, identity).await?;
            let res = download_with_retries(&endpoint, me, &store, ticket, retry_for).await;
            endpoint.close().await;
            res?;
        }
        write_files(&store, ticket, out, key).await
    }
    .await;
    // How much of the download there is, to decide whether to keep it.
    let have = local_bytes(&store, ticket).await;
    store.shutdown().await.ok();
    (res, have)
}

/// Bytes of the collection already in the local store.
async fn local_bytes(store: &FsStore, ticket: &BlobTicket) -> u64 {
    match store.remote().local(ticket.hash_and_format()).await {
        Ok(local) => local.local_bytes(),
        Err(_) => 0,
    }
}

/// Download, reconnecting after a failure until `retry_for` has passed
/// without any progress. Each attempt only fetches what is still missing.
///
/// A dropped connection frees a single-use server's slot only once the
/// server notices, which can take up to iroh's idle timeout (about 30 s), so
/// the first reconnects may be refused; they just count as attempts without
/// progress.
async fn download_with_retries(
    endpoint: &Endpoint,
    me: Option<EndpointId>,
    store: &FsStore,
    ticket: &BlobTicket,
    retry_for: Duration,
) -> Result<()> {
    let mut have = local_bytes(store, ticket).await;
    if have > 0 {
        say!("resuming: {} already downloaded", human(have));
    }
    let mut last_progress = Instant::now();
    let mut delay = Duration::from_secs(1);
    // When the client started waiting for a busy single-use slot.
    let mut busy_since: Option<Instant> = None;
    // Whether the server ever said another client was downloading.
    let mut saw_busy = false;
    loop {
        let mut err = match download(endpoint, me, store, ticket).await {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let now_have = local_bytes(store, ticket).await;
        if now_have > have {
            have = now_have;
            last_progress = Instant::now();
            delay = Duration::from_secs(1);
        }
        let why = refusal(&err);
        if let Some(r) = err.downcast_mut::<Refused>()
            && why == Some(Refusal::Busy)
        {
            r.waited = busy_since.map(|t| t.elapsed());
        }
        match why {
            // Retrying won't change the server's mind.
            Some(r) if !r.is_temporary() => return Err(err),
            Some(_) => {
                saw_busy = true;
                if last_progress.elapsed() + delay > retry_for {
                    return Err(err);
                }
                // Say it once, not on every attempt.
                if busy_since.is_none() {
                    busy_since = Some(Instant::now());
                    if have > 0 {
                        say!(
                            "the server's download slot is still taken, probably by this \
                             client's dropped connection (it can take the server about 30 s \
                             to notice); waiting"
                        );
                    } else {
                        say!(
                            "another client is downloading from this single-use server; \
                             waiting up to {}s in case it drops out",
                            retry_for.as_secs()
                        );
                    }
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_RETRY_DELAY);
                continue;
            }
            None => {}
        }
        busy_since = None;
        if last_progress.elapsed() + delay > retry_for {
            if saw_busy {
                return Err(err.context(
                    "the server was busy with another client's download and then stopped \
                     answering: most likely that download finished and the single-use server \
                     deleted the data and shut down. Ask the sender for a new ticket.",
                ));
            }
            return Err(err.context(GAVE_UP));
        }
        say!("{err:#}");
        say!(
            "retrying in {}s; the {} downloaded so far is kept",
            delay.as_secs(),
            human(have)
        );
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
}

async fn download(
    endpoint: &Endpoint,
    me: Option<EndpointId>,
    store: &FsStore,
    ticket: &BlobTicket,
) -> Result<()> {
    say!("connecting to {}", ticket.addr().id);
    let conn = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .context("failed to connect to the server")?;
    let res = match transfer(&conn, store, ticket).await {
        Ok(()) => Ok(()),
        Err(e) => Err(check_refused(e, &conn, me).await),
    };
    // Close right away, on errors too, so a single-use server frees its slot
    // (for our next attempt) instead of waiting for the connection to time out.
    match &res {
        Ok(()) => conn.close(0u32.into(), b"done"),
        Err(_) => conn.close(1u32.into(), b"failed"),
    }
    res
}

async fn transfer(conn: &Connection, store: &FsStore, ticket: &BlobTicket) -> Result<()> {
    // What an earlier, interrupted attempt already downloaded. The fetch
    // below only asks for the rest, and reports progress from zero.
    let resumed = local_bytes(store, ticket).await;

    // Ask for the size first, so the bar can show a total and ETA. Not
    // fatal: without it the bar just has no total.
    let total = match get_hash_seq_and_sizes(conn, &ticket.hash(), MAX_HASH_SEQ_BYTES, None).await {
        Ok((hash_seq, sizes)) => Some(hash_seq.len() as u64 * 32 + sizes.iter().sum::<u64>()),
        Err(e) => {
            tracing::debug!("size probe failed: {e}");
            None
        }
    };

    let started = Instant::now();
    let bar = ui::transfer_bar("downloading", total);
    let mut lines = ui::Every::new(Duration::from_secs(5));
    match total {
        Some(t) if bar.is_none() => say!("downloading {}", human(t)),
        _ => {}
    }
    if let Some(bar) = &bar {
        bar.set_position(resumed);
    }
    let mut stream = store
        .remote()
        .fetch(conn.clone(), ticket.hash_and_format())
        .stream();
    let stats = loop {
        match stream.next().await {
            Some(GetProgressItem::Progress(n)) => match (&bar, resumed + n) {
                (Some(bar), n) => {
                    if bar.length().is_some_and(|len| n > len) {
                        bar.set_length(n);
                    }
                    bar.set_position(n);
                }
                (None, n) if lines.due() => match total {
                    Some(t) => say!(
                        "downloaded {} / {} ({:.1}%)",
                        human(n),
                        human(t),
                        n as f64 / t.max(1) as f64 * 100.0
                    ),
                    None => say!("downloaded {}", human(n)),
                },
                (None, _) => {}
            },
            Some(GetProgressItem::Done(stats)) => break stats,
            Some(GetProgressItem::Error(e)) => {
                if let Some(bar) = &bar {
                    ui::fail_bar(
                        bar,
                        format!("download failed after {}", human(bar.position())),
                    );
                }
                bail!("download failed: {e}");
            }
            None => {
                if let Some(bar) = &bar {
                    ui::fail_bar(bar, "download ended without a result".into());
                }
                bail!("download ended without a result")
            }
        }
    };
    let summary = format!(
        "downloaded {}",
        ui::rate_summary(stats.payload_bytes_read, started.elapsed())
    );
    match &bar {
        Some(bar) => ui::finish_bar(bar, summary),
        None => say!("{summary}"),
    }
    Ok(())
}

/// Write the downloaded collection's files into `out`, decrypting them if
/// the collection is encrypted.
async fn write_files(
    store: &FsStore,
    ticket: &BlobTicket,
    out: &Path,
    key: Option<&Key>,
) -> Result<()> {
    let collection = Collection::load(ticket.hash(), &**store).await?;
    let encrypted =
        matches!(collection.iter().next(), Some((name, _)) if name == crypt::MANIFEST_NAME);
    if encrypted {
        let key = key.ok_or(NeedKey)?;
        write_encrypted(store, &collection, out, key).await
    } else {
        if key.is_some() {
            say!("note: the data is not encrypted; ignoring the decryption key");
        }
        write_plain(store, &collection, out).await
    }
}

/// Write the files of an unencrypted collection, named as in the collection.
async fn write_plain(store: &FsStore, collection: &Collection, out: &Path) -> Result<()> {
    // Check every name before writing anything.
    let targets = collection
        .iter()
        .map(|(name, hash)| Ok((*hash, safe_join(out, name)?)))
        .collect::<Result<Vec<_>>>()?;
    for (hash, target) in targets {
        write_blob(store, hash, None, &target)
            .await
            .with_context(|| format!("failed to write {}", target.display()))?;
        say!("wrote {}", target.display());
    }
    Ok(())
}

/// Decrypt the manifest of an encrypted collection, then decrypt its files
/// under the names the manifest gives.
async fn write_encrypted(
    store: &FsStore,
    collection: &Collection,
    out: &Path,
    key: &Key,
) -> Result<()> {
    let mut entries = collection.iter();
    let (_, manifest_hash) = entries.next().context("empty collection")?;
    let manifest = store.blobs().get_bytes(*manifest_hash).await?;
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
    say!("decrypting {}", manifest.name);

    for (hash, target) in files.into_iter().zip(targets) {
        write_blob(store, hash, Some(key), &target)
            .await
            .with_context(|| format!("failed to write {}", target.display()))?;
        say!("wrote {}", target.display());
    }

    Ok(())
}

/// Write one blob into `target`, decrypting it if `key` is given. It is
/// written under a temporary name and renamed at the end, so a failure never
/// leaves a partial file under the real name.
async fn write_blob(store: &FsStore, hash: Hash, key: Option<&Key>, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let file_name = target
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .into_owned();
    let partial = target.with_file_name(format!(".{file_name}.deaddrop-partial"));
    let reader = SyncIoBridge::new(store.blobs().reader(hash));
    let res = tokio::task::spawn_blocking({
        let (key, partial) = (key.cloned(), partial.clone());
        move || -> Result<()> {
            let input = std::io::BufReader::new(reader);
            let mut output = std::io::BufWriter::new(std::fs::File::create(&partial)?);
            match key {
                Some(key) => {
                    crypt::decrypt(&key, input, output)?;
                }
                None => {
                    let mut input = input;
                    std::io::copy(&mut input, &mut output)?;
                    std::io::Write::flush(&mut output)?;
                }
            }
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
