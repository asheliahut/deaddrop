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

use std::{collections::HashMap, time::Duration};

use iroh_blobs::{
    Hash,
    provider::events::{
        AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage,
        RequestMode, RequestUpdate,
    },
};
use tokio::sync::{mpsc, watch};

/// How long to wait for a single-use client to close the connection after
/// the server has sent everything.
const CLOSE_GRACE: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct State {
    single_use: bool,
    /// The collection being served, if any.
    root: Option<Hash>,
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
    pub fn set_root(&self, root: Option<Hash>) {
        self.0.send_modify(|s| {
            s.root = root;
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
pub fn watcher(root: Hash, single_use: bool) -> (EventSender, Gate, mpsc::Receiver<Hash>) {
    let mask = EventMask {
        connected: if single_use {
            ConnectMode::Intercept
        } else {
            ConnectMode::None
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
                            eprintln!(
                                "client connected: {}",
                                msg.endpoint_id.map(|e| e.to_string()).unwrap_or_default()
                            );
                        }
                        let res = if ok { Ok(()) } else { Err(AbortReason::Permission) };
                        msg.tx.send(res).await.ok();
                    }
                    ProviderMessage::ConnectionClosed(msg) => {
                        let mut finished = None;
                        state.send_if_modified(|s| {
                            if s.conn != Some(msg.connection_id) {
                                return false;
                            }
                            match s.completed {
                                // Keep the slot taken: no more downloads of this root.
                                Some(root) => finished = Some(root),
                                None => {
                                    eprintln!("client disconnected before finishing; waiting for another client");
                                    s.conn = None;
                                }
                            }
                            true
                        });
                        // Unless the grace timer already reported it.
                        if let Some(root) = finished && grace_root == Some(root) {
                            eprintln!("client finished and disconnected");
                            grace_root = None;
                            done_tx.send(root).await.ok();
                        }
                    }
                    ProviderMessage::GetRequestReceived(msg) => {
                        let conn = msg.connection_id;
                        let hash = msg.request.hash;
                        // Check and count in one step, so a reset can't slip in between.
                        let mut ok = false;
                        let mut single_use = false;
                        state.send_if_modified(|s| {
                            single_use = s.single_use;
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
                            tokio::spawn(async move {
                                while let Ok(Some(update)) = updates.recv().await {
                                    match update {
                                        RequestUpdate::Completed(_) => {
                                            if single_use {
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
                    eprintln!("transfer complete; waiting for the client to disconnect");
                    grace.as_mut().reset(tokio::time::Instant::now() + CLOSE_GRACE);
                    grace_root = Some(hash);
                }
            }
            _ = &mut grace, if grace_root.is_some() => {
                let root = grace_root.take().expect("guarded");
                // Only if no reset has replaced this root in the meantime.
                if state.borrow().completed == Some(root) {
                    eprintln!("client did not disconnect in time");
                    done_tx.send(root).await.ok();
                }
            }
        }
    }
}
