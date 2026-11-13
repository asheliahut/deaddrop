//! iroh endpoint setup shared by `serve`, `fetch` and `reset`: relay
//! settings from the config file, and reporting which network path each
//! connection uses.

use std::future::Future;

use anyhow::Result;
use iroh::{
    Endpoint, RelayMode, TransportAddr,
    endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, PathEvent, presets},
};
use n0_future::StreamExt;

/// Bind an endpoint with n0's address lookup and the given relay mode.
pub async fn bind(relay: &RelayMode) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0).hooks(PathReporter);
    // The N0 preset already picks n0's relays (honouring iroh's own
    // staging override), so only touch it for the other modes.
    if *relay != RelayMode::Default {
        builder = builder.relay_mode(relay.clone());
    }
    Ok(builder.bind().await?)
}

/// Prints the path each connection uses, and every later switch, e.g. from
/// the relay to a hole-punched direct path.
#[derive(Debug)]
struct PathReporter;

impl EndpointHooks for PathReporter {
    fn after_handshake<'a>(
        &'a self,
        conn: &'a Connection,
    ) -> impl Future<Output = AfterHandshakeOutcome> + Send + 'a {
        // Subscribe before reading the current path, so a switch in between
        // isn't missed. The event stream doesn't keep the connection alive
        // and ends when it closes.
        let mut events = conn.path_events();
        let peer = conn.remote_id().fmt_short();
        let mut current = conn
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| p.remote_addr().clone());
        if let Some(addr) = &current {
            report(peer, addr, false);
        }
        tokio::spawn(async move {
            while let Some(event) = events.next().await {
                // Only report relay <-> direct switches: iroh also moves
                // between direct addresses (e.g. several local interfaces),
                // which isn't worth a line each time.
                if let PathEvent::Selected { remote_addr, .. } = event
                    && current.as_ref().map(TransportAddr::is_relay) != Some(remote_addr.is_relay())
                {
                    report(peer, &remote_addr, current.is_some());
                    current = Some(remote_addr);
                }
            }
        });
        async { AfterHandshakeOutcome::accept() }
    }
}

fn report(peer: impl std::fmt::Display, addr: &TransportAddr, switched: bool) {
    let path = describe(addr);
    let verb = if switched { "switched to" } else { "using" };
    say!("connection to {peer} {verb} {path}");
}

fn describe(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Ip(a) => format!("a direct path ({a})"),
        TransportAddr::Relay(url) => format!("the relay {url}"),
        other => format!("{other:?}"),
    }
}
