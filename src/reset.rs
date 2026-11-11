//! Reset protocol: replace the torrent a server is serving.
//!
//! A client that has the ticket and the server's reset token opens a
//! connection with [`ALPN`] and sends one request on a bidirectional stream:
//!
//! ```text
//! u16 BE  token length
//! [u8]    token
//! u8      kind: 0 = magnet / http(s) URL (UTF-8), 1 = .torrent file contents
//! [u8]    payload, up to the end of the stream
//! ```
//!
//! The server answers with newline-delimited JSON [`Reply`] messages:
//! `accepted` (or `rejected`), then once the new torrent is downloaded and
//! being served, `ready` with the new ticket (or `failed`). The client may
//! hang up after `accepted`; the server carries on with the reset.
//!
//! The server only registers this protocol when a reset token is configured.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler},
};
use iroh_blobs::ticket::BlobTicket;
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncBufReadExt,
    sync::{mpsc, oneshot},
};

use crate::torrent::{MAX_TORRENT_BYTES, Source};

pub const ALPN: &[u8] = b"deaddrop/reset/0";

/// Tokens shorter than this are refused at startup.
pub const MIN_TOKEN_LEN: usize = 16;

const KIND_URL: u8 = 0;
const KIND_TORRENT: u8 = 1;

/// Room for the token and header on top of the largest torrent file.
const MAX_REQUEST: usize = MAX_TORRENT_BYTES + 64 * 1024;

/// How long to wait for the client to hang up after the last reply.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

/// Slows down token guessing.
const BAD_TOKEN_DELAY: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Reply {
    Accepted,
    Rejected { error: String },
    Ready { ticket: String, key: String },
    Failed { error: String },
}

impl Reply {
    /// For error messages; never includes the key.
    fn status(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::Ready { .. } => "ready",
            Self::Failed { .. } => "failed",
        }
    }
}

/// What a finished reset hands back to the requester.
pub struct NewTicket {
    pub ticket: String,
    /// The new torrent's decryption key.
    pub key: String,
}

/// Check a reset token's format. Used for both `serve` and `reset`.
pub fn check_token(token: &str) -> Result<()> {
    ensure!(
        token.len() >= MIN_TOKEN_LEN,
        "the reset token must be at least {MIN_TOKEN_LEN} characters"
    );
    ensure!(
        token.len() <= u16::MAX as usize,
        "the reset token is too long"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

/// A reset the server should carry out.
pub struct Job {
    pub source: Source,
    pub requester: EndpointId,
    /// The new ticket and key, or why the reset failed.
    pub reply: oneshot::Sender<Result<NewTicket, String>>,
    /// Marks the server as busy until this is dropped.
    pub busy: BusyGuard,
}

pub struct BusyGuard(Arc<AtomicBool>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct Handler {
    token: Arc<str>,
    jobs: mpsc::Sender<Job>,
    busy: Arc<AtomicBool>,
}

// Hand-written so the token never shows up in debug output.
impl std::fmt::Debug for Handler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reset::Handler(<redacted>)")
    }
}

/// The protocol handler, and the queue of accepted resets for the server.
pub fn handler(token: String) -> (Handler, mpsc::Receiver<Job>) {
    let (jobs, rx) = mpsc::channel(1);
    let h = Handler {
        token: token.into(),
        jobs,
        busy: Arc::new(AtomicBool::new(false)),
    };
    (h, rx)
}

impl ProtocolHandler for Handler {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        if let Err(e) = self.handle(&conn, remote).await {
            eprintln!("reset: request from {remote} failed: {e:#}");
        }
        // Give the client a moment to read the last reply and hang up.
        tokio::time::timeout(CLOSE_GRACE, conn.closed()).await.ok();
        Ok(())
    }
}

impl Handler {
    async fn handle(&self, conn: &Connection, remote: EndpointId) -> Result<()> {
        let (mut send, mut recv) = conn.accept_bi().await?;
        let buf = recv
            .read_to_end(MAX_REQUEST)
            .await
            .context("failed to read the request")?;

        let (token, source) = match parse_request(&buf) {
            Ok(r) => r,
            Err(e) => return reject(&mut send, &format!("bad request: {e:#}")).await,
        };
        if !constant_time_eq(token, self.token.as_bytes()) {
            eprintln!("reset: rejected request from {remote}: wrong token");
            tokio::time::sleep(BAD_TOKEN_DELAY).await;
            return reject(&mut send, "wrong reset token").await;
        }
        if self.busy.swap(true, Ordering::SeqCst) {
            return reject(&mut send, "another reset is already in progress").await;
        }
        let busy = BusyGuard(self.busy.clone());

        eprintln!("reset: {remote} asked to switch to a {}", source.describe());
        let (reply_tx, reply_rx) = oneshot::channel();
        let job = Job {
            source,
            requester: remote,
            reply: reply_tx,
            busy,
        };
        if self.jobs.send(job).await.is_err() {
            return reject(&mut send, "the server is shutting down").await;
        }
        // The client may hang up from here on; the reset goes ahead anyway.
        write_reply(&mut send, &Reply::Accepted).await.ok();

        let reply = match reply_rx.await {
            Ok(Ok(NewTicket { ticket, key })) => Reply::Ready { ticket, key },
            Ok(Err(error)) => Reply::Failed { error },
            Err(_) => Reply::Failed {
                error: "the server stopped before the reset finished".into(),
            },
        };
        write_reply(&mut send, &reply).await.ok();
        send.finish().ok();
        Ok(())
    }
}

async fn reject(send: &mut SendStream, error: &str) -> Result<()> {
    write_reply(
        send,
        &Reply::Rejected {
            error: error.into(),
        },
    )
    .await?;
    send.finish().ok();
    Ok(())
}

async fn write_reply(send: &mut SendStream, reply: &Reply) -> Result<()> {
    let mut line = serde_json::to_vec(reply)?;
    line.push(b'\n');
    send.write_all(&line).await?;
    Ok(())
}

fn parse_request(buf: &[u8]) -> Result<(&[u8], Source)> {
    let (len, rest) = buf.split_first_chunk::<2>().context("truncated request")?;
    let len = u16::from_be_bytes(*len) as usize;
    ensure!(rest.len() > len, "truncated request");
    let (token, rest) = rest.split_at(len);
    let (&kind, payload) = rest.split_first().context("truncated request")?;
    let source = match kind {
        KIND_URL => {
            let url = std::str::from_utf8(payload).context("URL is not valid UTF-8")?;
            Source::url(url.trim().to_string())?
        }
        KIND_TORRENT => Source::bytes(payload.to_vec())?,
        other => bail!("unknown source kind {other}"),
    };
    Ok((token, source))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

fn encode_request(token: &str, source: &Source) -> Vec<u8> {
    let (kind, payload) = match source {
        Source::Url(u) => (KIND_URL, u.as_bytes()),
        Source::Bytes(b) => (KIND_TORRENT, b.as_slice()),
    };
    let mut buf = Vec::with_capacity(3 + token.len() + payload.len());
    buf.extend_from_slice(&(token.len() as u16).to_be_bytes());
    buf.extend_from_slice(token.as_bytes());
    buf.push(kind);
    buf.extend_from_slice(payload);
    buf
}

/// Ask the server named in `ticket` to switch to `source`. With `wait`, block
/// until the new torrent is being served and print the new ticket.
pub async fn request(ticket: BlobTicket, source: Source, token: String, wait: bool) -> Result<()> {
    check_token(&token)?;
    let endpoint = Endpoint::builder(presets::N0).bind().await?;
    let res = run_request(&endpoint, &ticket, &source, &token, wait).await;
    endpoint.close().await;
    res
}

async fn run_request(
    endpoint: &Endpoint,
    ticket: &BlobTicket,
    source: &Source,
    token: &str,
    wait: bool,
) -> Result<()> {
    eprintln!("connecting to {}", ticket.addr().id);
    let conn = endpoint
        .connect(ticket.addr().clone(), ALPN)
        .await
        .context(
            "failed to connect to the server for a reset (it must be running \
             with a reset token)",
        )?;
    let (mut send, recv) = conn.open_bi().await?;
    send.write_all(&encode_request(token, source)).await?;
    send.finish()?;

    let mut lines = tokio::io::BufReader::new(recv).lines();
    let mut next = async || -> Result<Reply> {
        let line = lines
            .next_line()
            .await
            .context("lost the connection to the server")?
            .context("the server closed the connection without answering")?;
        serde_json::from_str(&line).context("bad reply from the server")
    };

    match next().await? {
        Reply::Accepted => eprintln!("reset accepted; the server is downloading the new torrent"),
        Reply::Rejected { error } => bail!("the server refused the reset: {error}"),
        other => bail!("unexpected reply from the server: {}", other.status()),
    }
    if !wait {
        conn.close(0u32.into(), b"bye");
        return Ok(());
    }
    eprintln!("waiting for it to finish (Ctrl-C stops waiting; the reset carries on)");
    let res = match next().await? {
        Reply::Ready { ticket, key } => {
            eprintln!("the server is now serving the new torrent.");
            crate::print_ticket(&ticket, &key)
        }
        Reply::Failed { error } => Err(anyhow::anyhow!(
            "reset failed: {error} (the server keeps serving the old torrent)"
        )),
        other => Err(anyhow::anyhow!(
            "unexpected reply from the server: {}",
            other.status()
        )),
    };
    conn.close(0u32.into(), b"bye");
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let token = "0123456789abcdef";
        let buf = encode_request(token, &Source::Url("magnet:?xt=urn:btih:x".into()));
        let (t, s) = parse_request(&buf).unwrap();
        assert_eq!(t, token.as_bytes());
        assert!(matches!(s, Source::Url(u) if u == "magnet:?xt=urn:btih:x"));

        let buf = encode_request(token, &Source::Bytes(vec![1, 2, 3]));
        let (_, s) = parse_request(&buf).unwrap();
        assert!(matches!(s, Source::Bytes(b) if b == [1, 2, 3]));
    }

    #[test]
    fn rejects_local_paths_and_garbage() {
        let mut buf = encode_request("0123456789abcdef", &Source::Url("/etc/passwd".into()));
        assert!(parse_request(&buf).is_err());
        buf.truncate(5);
        assert!(parse_request(&buf).is_err());
        assert!(parse_request(&[]).is_err());
        assert!(parse_request(&[0, 1, b'x', 9]).is_err());
    }

    #[test]
    fn token_checks() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(check_token("short").is_err());
        assert!(check_token("0123456789abcdef").is_ok());
    }
}
