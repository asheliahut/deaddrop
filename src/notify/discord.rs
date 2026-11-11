//! Discord channel webhook (Server Settings → Integrations → Webhooks).

use anyhow::Result;
use serde_json::json;

use super::{BoxFuture, Event, Notifier, parse_http_url, post_json};
use crate::torrent::human;

pub struct Discord {
    url: reqwest::Url,
    http: reqwest::Client,
}

impl Discord {
    pub fn new(target: &str, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            url: parse_http_url(target)?,
            http,
        })
    }
}

impl Notifier for Discord {
    fn label(&self) -> String {
        "discord".into()
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = json!({
                "username": "deaddrop",
                "content": render(event),
                // Never ping @everyone/@here or roles, even if a torrent name contains them.
                "allowed_mentions": { "parse": [] },
            });
            post_json(&self.http, &self.url, &body).await
        })
    }
}

/// Discord message text. Discord limits messages to 2000 characters.
fn render(event: &Event) -> String {
    match event {
        Event::TicketReady(t) => {
            let mode = if t.single_use {
                "single use: the server shuts down after one download"
            } else {
                "multi use"
            };
            let reset = if t.reset {
                "\n🔁 This replaces the previous ticket, which no longer works."
            } else {
                ""
            };
            // A spoiler, so the key isn't readable at a glance.
            let key = match &t.key {
                Some(k) => format!("\n🔑 Decryption key (set as DEADDROP_KEY): ||`{k}`||"),
                None => String::new(),
            };
            let msg = format!(
                "📦 **{}** is ready ({} file(s), {}, {mode}){reset}\n```\n{}\n```{key}",
                // Shorten the name, not the message, so the ticket is never cut off.
                escape(&truncate(t.name.clone(), 300)),
                t.files,
                human(t.total_bytes),
                t.fetch_command,
            );
            truncate(msg, 2000)
        }
    }
}

/// Escape Discord markdown in names that come from the torrent.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '_' | '~' | '`' | '|' | '>' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max - 1).chain(['…']).collect()
    }
}
