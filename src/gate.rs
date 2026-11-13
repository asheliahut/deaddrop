//! Decides which iroh-blobs requests the server answers.
//!
//! Both modes:
//! - Only `get` requests for the current collection root are allowed
//!   (`get_many` and `push` are disabled). After a reset swaps in a new root,
//!   requests for the old one are rejected, so the old ticket stops working.
//! - In-flight transfers are counted per root, so the server can wait for
//!   them to end before deleting a collection.
//!
//! Single use additionally:
//! - Only one client connection is allowed at a time; others are rejected.
//! - Once that client has the whole collection and disconnects (or
//!   [`CLOSE_GRACE`] after it has everything), the gate reports the root as
//!   finished and rejects every later connection until a new root is set.
//! - If the client goes away before finishing, the slot opens up again.
//!
//! Size probes (see [`size_probe`]) are answered but never count as a
//! finished download, so a client can learn the total size first.
//!
//! Each download gets a progress bar (or plain lines when stderr isn't a
//! terminal).

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use iroh_blobs::{
    Hash,
    protocol::{ChunkRanges, ChunkRangesExt, ChunkRangesSeq},
    provider::events::{
        AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage,
        RequestMode, RequestUpdate,
    },
};
use tokio::sync::{mpsc, watch};

use crate::{torrent::human, ui};

/// The ranges `iroh_blobs::get::request::get_hash_seq_and_sizes` asks for:
/// the whole hash seq and the last chunk (a size proof) of every child.
pub fn size_probe() -> ChunkRangesSeq {
    ChunkRangesSeq::from_ranges_infinite([ChunkRanges::all(), ChunkRanges::last_chunk()])
}

/// How long to wait for a single-use client to close the connection after
/// the server has sent everything.
const CLOSE_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct State {
    single_use: bool,
    /// The collection being served, if any.
    root: Option<Hash>,
    /// Its size in bytes, for progress bars.
    total: u64,
    /// Number of in-flight `get` requests per root.
    active: HashMap<Hash, usize>,
    /// Single use: the one connection currently allowed.
    conn: Option<u64>,
    /// Single use: the root that connection has fully downloaded.
    completed: Option<Hash>,
}

/// Handle for changing the served root and waiting on transfers.
#[derive(Debug, Clone)]
pub struct Gate(watch::Sender<State>);

impl Gate {
    /// Serve `root` from now on (`None`: serve nothing). Requests for any
    /// other root are rejected. In single-use mode this opens the slot for one
    /// new download.
    pub fn set_root(&self, root: Option<(Hash, u64)>) {
        self.0.send_modify(|s| {
            s.root = root.map(|(hash, _)| hash);
            s.total = root.map_or(0, |(_, total)| total);
            s.conn = None;
            s.completed = None;
        });
    }

    /// Wait until no transfers of `root` are in flight.
    pub async fn wait_idle(&self, root: Hash) {
        let mut rx = self.0.subscribe();
        rx.wait_for(|s| !s.active.contains_key(&root)).await.ok();
    }
}

/// Start the gate. In single-use mode, the receiver gets each root whose one
/// download has finished.
pub fn watcher(
    root: Hash,
    total: u64,
    single_use: bool,
) -> (EventSender, Gate, mpsc::Receiver<Hash>) {
    let mask = EventMask {
        connected: if single_use {
            ConnectMode::Intercept
        } else {
            // Only to name clients on their progress bars.
            ConnectMode::Notify
        },
        get: RequestMode::InterceptLog,
        get_many: RequestMode::Disabled,
        push: RequestMode::Disabled,
        observe: ObserveMode::None,
        ..EventMask::DEFAULT
    };
    let (events, rx) = EventSender::channel(32, mask);
    let gate = Gate(watch::Sender::new(State {
        single_use,
        root: Some(root),
        total,
        active: HashMap::new(),
        conn: None,
        completed: None,
    }));
    let (done_tx, done_rx) = mpsc::channel(4);
    tokio::spawn(run(rx, gate.0.clone(), done_tx));
    (events, gate, done_rx)
}

async fn run(
    mut rx: mpsc::Receiver<ProviderMessage>,
    state: watch::Sender<State>,
    done_tx: mpsc::Sender<Hash>,
) {
    // Per-request watchers report finished transfers here: (connection, root).
    let (completed_tx, mut completed_rx) = mpsc::channel::<(u64, Hash)>(4);
    let mut grace = std::pin::pin!(tokio::time::sleep(Duration::MAX));
    let mut grace_root: Option<Hash> = None;
    // Short endpoint ids of connected clients, for progress bars.
    let mut clients: HashMap<u64, String> = HashMap::new();

    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    ProviderMessage::ClientConnected(msg) => {
                        let mut ok = false;
                        state.send_if_modified(|s| {
                            ok = s.root.is_some() && s.conn.is_none() && s.completed.is_none();
                            if ok {
                                s.conn = Some(msg.connection_id);
                            }
                            ok
                        });
                        if ok {
                            say!(
                                "client connected: {}",
                                msg.endpoint_id.map(|e| e.to_string()).unwrap_or_default()
                            );
                            if let Some(id) = msg.endpoint_id {
                                clients.insert(msg.connection_id, id.fmt_short().to_string());
                            }
                        }
                        let res = if ok { Ok(()) } else { Err(AbortReason::Permission) };
                        msg.tx.send(res).await.ok();
                    }
                    ProviderMessage::ClientConnectedNotify(msg) => {
                        if let Some(id) = msg.endpoint_id {
                            say!("client connected: {id}");
                            clients.insert(msg.connection_id, id.fmt_short().to_string());
                        }
                    }
                    ProviderMessage::ConnectionClosed(msg) => {
                        clients.remove(&msg.connection_id);
                        let mut finished = None;
                        state.send_if_modified(|s| {
                            if s.conn != Some(msg.connection_id) {
                                return false;
                            }
                            match s.completed {
                                // Keep the slot taken: no more downloads of this root.
                                Some(root) => finished = Some(root),
                                None => {
                                    say!("client disconnected before finishing; waiting for another client");
                                    s.conn = None;
                                }
                            }
                            true
                        });
                        // Unless the grace timer already reported it.
                        if let Some(root) = finished && grace_root == Some(root) {
                            say!("client finished and disconnected");
                            grace_root = None;
                            done_tx.send(root).await.ok();
                        }
                    }
                    ProviderMessage::GetRequestReceived(msg) => {
                        let conn = msg.connection_id;
                        let hash = msg.request.hash;
                        let probe = msg.request.ranges == size_probe();
                        let mut total = 0;
                        // Check and count in one step, so a reset can't slip in between.
                        let mut ok = false;
                        let mut single_use = false;
                        state.send_if_modified(|s| {
                            single_use = s.single_use;
                            total = s.total;
                            ok = s.root == Some(hash)
                                && (!s.single_use || (s.conn == Some(conn) && s.completed.is_none()));
                            if ok {
                                *s.active.entry(hash).or_default() += 1;
                                return true;
                            }
                            // Single use: a client asking for something else (say, an
                            // old ticket after a reset) must not keep the slot taken
                            // until its connection times out.
                            if s.single_use && s.conn == Some(conn) && s.completed.is_none() {
                                s.conn = None;
                                return true;
                            }
                            false
                        });
                        let res = if ok { Ok(()) } else { Err(AbortReason::Permission) };
                        msg.tx.send(res).await.ok();
                        if ok {
                            let mut updates = msg.rx;
                            let state = state.clone();
                            let completed_tx = completed_tx.clone();
                            let client = clients
                                .get(&conn)
                                .cloned()
                                .unwrap_or_else(|| format!("connection {conn}"));
                            let mut progress = (!probe).then(|| SendProgress::new(client, total));
                            tokio::spawn(async move {
                                while let Ok(Some(update)) = updates.recv().await {
                                    if let Some(p) = &mut progress {
                                        p.update(&update);
                                    }
                                    match update {
                                        RequestUpdate::Completed(_) => {
                                            // A size probe is not the download.
                                            if single_use && !probe {
                                                completed_tx.send((conn, hash)).await.ok();
                                            }
                                            break;
                                        }
                                        RequestUpdate::Aborted(_) => break,
                                        _ => {}
                                    }
                                }
                                state.send_modify(|s| {
                                    if let Some(n) = s.active.get_mut(&hash) {
                                        *n -= 1;
                                        if *n == 0 {
                                            s.active.remove(&hash);
                                        }
                                    }
                                });
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some((conn, hash)) = completed_rx.recv() => {
                let marked = state.send_if_modified(|s| {
                    let ok = s.conn == Some(conn) && s.root == Some(hash) && s.completed.is_none();
                    if ok {
                        s.completed = Some(hash);
                    }
                    ok
                });
                if marked {
                    say!("transfer complete; waiting for the client to disconnect");
                    grace.as_mut().reset(tokio::time::Instant::now() + CLOSE_GRACE);
                    grace_root = Some(hash);
                }
            }
            _ = &mut grace, if grace_root.is_some() => {
                let root = grace_root.take().expect("guarded");
                // Only if no reset has replaced this root in the meantime.
                if state.borrow().completed == Some(root) {
                    say!("client did not disconnect in time");
                    done_tx.send(root).await.ok();
                }
            }
        }
    }
}

/// Progress of sending one download to a client.
struct SendProgress {
    client: String,
    bar: Option<indicatif::ProgressBar>,
    started: Instant,
    /// Bytes of the blobs already sent in full.
    done: u64,
    /// Size of the blob being sent.
    current: u64,
    lines: ui::Every,
}

impl SendProgress {
    fn new(client: String, total: u64) -> Self {
        let bar = ui::transfer_bar(&format!("sending to {client}"), Some(total));
        if bar.is_none() {
            say!("sending {} to {client}", human(total));
        }
        Self {
            client,
            bar,
            started: Instant::now(),
            done: 0,
            current: 0,
            lines: ui::Every::new(Duration::from_secs(5)),
        }
    }

    fn update(&mut self, update: &RequestUpdate) {
        let client = &self.client;
        match update {
            RequestUpdate::Started(t) => {
                self.done += self.current;
                self.current = t.size;
                // The total counts file bytes; the collection's own blobs
                // add a little on top.
                if let Some(bar) = &self.bar
                    && bar.length().is_some_and(|len| len < self.done + t.size)
                {
                    bar.set_length(self.done + t.size);
                }
            }
            RequestUpdate::Progress(p) => {
                let sent = self.done + p.end_offset;
                match &self.bar {
                    Some(bar) => bar.set_position(sent),
                    None if self.lines.due() => say!("sending to {client}: {}", human(sent)),
                    None => {}
                }
            }
            RequestUpdate::Completed(c) => {
                let summary = format!(
                    "sent {} to {client}",
                    ui::rate_summary(c.stats.payload_bytes_sent, self.started.elapsed())
                );
                match &self.bar {
                    Some(bar) => ui::finish_bar(bar, summary),
                    None => say!("{summary}"),
                }
            }
            RequestUpdate::Aborted(a) => {
                let summary = format!(
                    "transfer to {client} stopped after {}",
                    human(a.stats.payload_bytes_sent)
                );
                match &self.bar {
                    Some(bar) => ui::fail_bar(bar, summary),
                    None => say!("{summary}"),
                }
            }
        }
    }
}

impl Drop for SendProgress {
    /// The update stream can end without a final event (e.g. a dropped
    /// connection); don't leave a spinning bar behind.
    fn drop(&mut self) {
        if let Some(bar) = &self.bar
            && !bar.is_finished()
        {
            ui::fail_bar(bar, format!("transfer to {} ended", self.client));
        }
    }
}
