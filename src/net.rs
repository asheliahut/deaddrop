//! iroh endpoint setup shared by `serve`, `fetch` and `reset`: relay
//! settings from the config file, reporting which network path each
//! connection uses, the server's client allow list, and the reasons a
//! server gives when it turns a client away.

use std::{collections::HashSet, future::Future, sync::Arc};

use anyhow::Result;
use iroh::{
    Endpoint, EndpointId, RelayMode, SecretKey, TransportAddr,
    endpoint::{
        AfterHandshakeOutcome, ApplicationClose, Connection, ConnectionError, EndpointHooks,
        PathEvent, VarInt, presets,
    },
};
use n0_future::StreamExt;

/// Why a server turned a client away. Sent as the connection's close code
/// (0x6464 is "dd" in ASCII), with a short human-readable reason, so the
/// client can tell the user what happened and whether waiting could help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The server has an allow list and this client isn't on it.
    NotAllowed,
    /// Single use: another client holds the one download slot. It frees up
    /// if that client drops out before finishing, so waiting can help.
    Busy,
    /// Single use: the one download already happened.
    Downloaded,
    /// The ticket expired (`serve --expire-after`).
    Expired,
    /// A reset replaced the ticket's torrent.
    Replaced,
    /// The server never served this ticket.
    Unknown,
}

impl Refusal {
    const ALL: [Self; 6] = [
        Self::NotAllowed,
        Self::Busy,
        Self::Downloaded,
        Self::Expired,
        Self::Replaced,
        Self::Unknown,
    ];

    pub fn code(self) -> VarInt {
        VarInt::from_u32(match self {
            Self::NotAllowed => 0x6464,
            Self::Busy => 0x6465,
            Self::Downloaded => 0x6466,
            Self::Expired => 0x6467,
            Self::Replaced => 0x6468,
            Self::Unknown => 0x6469,
        })
    }

    /// The reason sent over the wire, also logged by the server. Older
    /// clients show it as is.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotAllowed => "not on this server's allow list",
            Self::Busy => "another client is downloading from this single-use server",
            Self::Downloaded => "this single-use drop has already been downloaded",
            Self::Expired => "this ticket has expired",
            Self::Replaced => "this ticket was replaced by a reset",
            Self::Unknown => "this server isn't serving that ticket",
        }
    }

    /// Whether trying again later could work.
    pub fn is_temporary(self) -> bool {
        self == Self::Busy
    }

    /// Close `conn` with this reason.
    pub fn close(self, conn: &Connection) {
        conn.close(self.code(), self.reason().as_bytes());
    }

    /// Why the server closed `conn`, if it turned this client away.
    pub fn of(conn: &Connection) -> Option<Self> {
        match conn.close_reason() {
            Some(ConnectionError::ApplicationClosed(ApplicationClose { error_code, .. })) => {
                Self::ALL.into_iter().find(|r| r.code() == error_code)
            }
            _ => None,
        }
    }
}

/// Bind an endpoint with n0's address lookup and the given relay mode, and
/// a new random identity.
pub async fn bind(relay: &RelayMode) -> Result<Endpoint> {
    bind_with(relay, None, &[]).await
}

/// Like [`bind`], with a fixed identity (`secret_key`) and, if `allow` isn't
/// empty, only accepting blob downloads from those endpoints. Other
/// protocols (resets, which have their own token) are not affected.
pub async fn bind_with(
    relay: &RelayMode,
    secret_key: Option<SecretKey>,
    allow: &[EndpointId],
) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::N0);
    // Before the path reporter, so rejected clients don't get path lines.
    if !allow.is_empty() {
        builder = builder.hooks(AllowList(Arc::new(allow.iter().copied().collect())));
    }
    builder = builder.hooks(PathReporter);
    // The N0 preset already picks n0's relays (honouring iroh's own
    // staging override), so only touch it for the other modes.
    if *relay != RelayMode::Default {
        builder = builder.relay_mode(relay.clone());
    }
    if let Some(key) = secret_key {
        builder = builder.secret_key(key);
    }
    Ok(builder.bind().await?)
}

/// Rejects blob connections from endpoints that aren't on the list, right
/// after the TLS handshake (which proves the remote's id).
#[derive(Debug)]
struct AllowList(Arc<HashSet<EndpointId>>);

impl EndpointHooks for AllowList {
    fn after_handshake<'a>(
        &'a self,
        conn: &'a Connection,
    ) -> impl Future<Output = AfterHandshakeOutcome> + Send + 'a {
        let outcome = if conn.alpn() != iroh_blobs::ALPN || self.0.contains(&conn.remote_id()) {
            AfterHandshakeOutcome::accept()
        } else {
            say!(
                "turned away {}: not on the allow list",
                conn.remote_id().fmt_short()
            );
            AfterHandshakeOutcome::Reject {
                error_code: Refusal::NotAllowed.code(),
                reason: Refusal::NotAllowed.reason().as_bytes().to_vec(),
            }
        };
        async { outcome }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_codes_are_distinct() {
        let codes: HashSet<_> = Refusal::ALL.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), Refusal::ALL.len());
        // iroh-blobs uses 1-3; ours must never collide with them.
        assert!(codes.iter().all(|c| c.into_inner() > 3));
        assert!(Refusal::Busy.is_temporary());
        assert!(!Refusal::Downloaded.is_temporary());
    }
}
