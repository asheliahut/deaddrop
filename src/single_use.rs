//! Provider event handling for `--single-use`.
//!
//! - Only one client connection is allowed at a time. Other connections are
//!   rejected while it is active.
//! - Only `get` requests for the collection root are allowed.
//! - Once a request for the root completes and that client's connection
//!   closes, the server is told to shut down and every later connection is
//!   rejected. If the client never closes the connection, a grace timer does
//!   the shutdown instead.
//! - If the client goes away before finishing, the slot opens up again.

use std::time::Duration;

use iroh_blobs::{
    Hash,
    provider::events::{
        AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage,
        RequestMode, RequestUpdate,
    },
};
use tokio::sync::{mpsc, oneshot};

/// How long to wait for the client to close the connection after the server
/// has sent everything.
const CLOSE_GRACE: Duration = Duration::from_secs(30);

pub fn watcher(root: Hash) -> (EventSender, oneshot::Receiver<()>) {
    let mask = EventMask {
        connected: ConnectMode::Intercept,
        get: RequestMode::InterceptLog,
        get_many: RequestMode::Disabled,
        push: RequestMode::Disabled,
        observe: ObserveMode::None,
        ..EventMask::DEFAULT
    };
    let (events, mut rx) = EventSender::channel(32, mask);
    let (done_tx, done_rx) = oneshot::channel();
    // Per-request watchers report a finished root transfer on this channel.
    let (completed_tx, mut completed_rx) = mpsc::channel::<u64>(4);

    tokio::spawn(async move {
        let mut active: Option<u64> = None;
        let mut completed = false;
        let mut done_tx = Some(done_tx);
        let mut grace = std::pin::pin!(tokio::time::sleep(Duration::MAX));
        let mut grace_armed = false;

        let finish = |done_tx: &mut Option<oneshot::Sender<()>>| {
            if let Some(tx) = done_tx.take() {
                tx.send(()).ok();
            }
        };

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    match msg {
                        ProviderMessage::ClientConnected(msg) => {
                            let res = if completed || active.is_some() {
                                Err(AbortReason::Permission)
                            } else {
                                active = Some(msg.connection_id);
                                eprintln!(
                                    "client connected: {}",
                                    msg.endpoint_id.map(|e| e.to_string()).unwrap_or_default()
                                );
                                Ok(())
                            };
                            msg.tx.send(res).await.ok();
                        }
                        ProviderMessage::ConnectionClosed(msg) => {
                            if active == Some(msg.connection_id) {
                                if completed {
                                    eprintln!("client finished and disconnected");
                                    finish(&mut done_tx);
                                } else {
                                    eprintln!("client disconnected before finishing; waiting for another client");
                                    active = None;
                                }
                            }
                        }
                        ProviderMessage::GetRequestReceived(msg) => {
                            let conn = msg.connection_id;
                            let ok = active == Some(conn) && msg.request.hash == root;
                            let res = if ok { Ok(()) } else { Err(AbortReason::Permission) };
                            msg.tx.send(res).await.ok();
                            if ok {
                                let mut updates = msg.rx;
                                let completed_tx = completed_tx.clone();
                                tokio::spawn(async move {
                                    while let Ok(Some(update)) = updates.recv().await {
                                        match update {
                                            RequestUpdate::Completed(_) => {
                                                completed_tx.send(conn).await.ok();
                                                break;
                                            }
                                            RequestUpdate::Aborted(_) => break,
                                            _ => {}
                                        }
                                    }
                                });
                            }
                        }
                        _ => {}
                    }
                }
                Some(conn) = completed_rx.recv() => {
                    if active == Some(conn) && !completed {
                        completed = true;
                        eprintln!("transfer complete; shutting down once the client disconnects");
                        grace.as_mut().reset(tokio::time::Instant::now() + CLOSE_GRACE);
                        grace_armed = true;
                    }
                }
                _ = &mut grace, if grace_armed => {
                    eprintln!("client did not disconnect in time; shutting down");
                    finish(&mut done_tx);
                    grace_armed = false;
                }
            }
        }
    });

    (events, done_rx)
}
