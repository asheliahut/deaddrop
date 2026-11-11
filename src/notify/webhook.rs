//! Generic webhook: POSTs the event as JSON.
//!
//! ```json
//! {
//!   "event": "ticket_ready",
//!   "name": "ubuntu-24.04.iso",
//!   "ticket": "blob...",
//!   "fetch_command": "deaddrop fetch blob...",
//!   "files": 1,
//!   "total_bytes": 6114656256,
//!   "single_use": true
//! }
//! ```

use anyhow::Result;

use super::{BoxFuture, Event, Notifier, parse_http_url, post_json};

pub struct Webhook {
    url: reqwest::Url,
    http: reqwest::Client,
}

impl Webhook {
    pub fn new(target: &str, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            url: parse_http_url(target)?,
            http,
        })
    }
}

impl Notifier for Webhook {
    fn label(&self) -> String {
        // Host only: the path or query string may carry a token.
        format!("webhook ({})", self.url.host_str().unwrap_or("?"))
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(post_json(&self.http, &self.url, event))
    }
}
