//! Mattermost incoming webhook (Integrations → Incoming Webhooks).

use anyhow::Result;
use serde_json::json;

use super::{BoxFuture, Event, Notifier, now, parse_http_url, post_json, time_left};
use crate::torrent::human;

pub struct Mattermost {
    url: reqwest::Url,
    http: reqwest::Client,
}

impl Mattermost {
    pub fn new(target: &str, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            url: parse_http_url(target)?,
            http,
        })
    }
}

impl Notifier for Mattermost {
    fn label(&self) -> String {
        // Host only: the path holds the webhook's secret key.
        format!("mattermost ({})", self.url.host_str().unwrap_or("?"))
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = json!({
                // Ignored unless the server lets integrations override it.
                "username": "deaddrop",
                "text": render(event, now()),
            });
            post_json(&self.http, &self.url, &body).await
        })
    }
}

/// Message text in Mattermost's Markdown. Only the name is shortened, so
/// the ticket is never cut.
fn render(event: &Event, now: u64) -> String {
    match event {
        Event::TicketReady(t) => {
            let mode = if t.single_use {
                "single use: the server shuts down after one download"
            } else {
                "multi use"
            };
            let name: String = if t.name.chars().count() > 300 {
                t.name.chars().take(299).chain(['…']).collect()
            } else {
                t.name.clone()
            };
            let mut msg = format!(
                ":package: **{}** is ready ({} file(s), {}, {mode})",
                escape(&name),
                t.files,
                human(t.total_bytes),
            );
            if t.reset {
                msg.push_str(
                    "\n:repeat: This replaces the previous ticket, which no longer works.",
                );
            }
            if let Some(at) = t.expires_at {
                msg.push_str(&format!(
                    "\n:hourglass_flowing_sand: The ticket expires in {}.",
                    time_left(at, now)
                ));
            }
            msg.push_str(&format!("\n```\n{}\n```", t.fetch_command));
            if let Some(k) = &t.key {
                // Mattermost has no spoilers; at least keep it easy to copy.
                msg.push_str(&format!(
                    "\n:key: Decryption key (set as DEADDROP_KEY): `{k}`"
                ));
            }
            msg
        }
    }
}

/// Escape Markdown in names that come from the torrent, and break up
/// `@mentions` (`@channel`, `@all`, `@here`, users) with a zero-width space
/// so a name can't ping anyone.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '~' | '[' | ']' | '(' | ')' | '<' | '>' | '#' | '|' | '!' => {
                out.push('\\');
                out.push(c);
            }
            '@' => out.push_str("@\u{200B}"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    #[test]
    fn settings() {
        let http = reqwest::Client::new;
        let m = Mattermost::new("https://mm.example.com/hooks/SECRET", http()).unwrap();
        assert_eq!(m.label(), "mattermost (mm.example.com)");
        assert!(Mattermost::new("ftp://mm.example.com/hooks/x", http()).is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "@channel *x* [y](z)".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: true,
            reset: false,
            expires_at: Some(100 + 1800),
            key: None,
        });
        let msg = render(&ev, 100);
        assert!(msg.contains("**@\u{200B}channel \\*x\\* \\[y\\]\\(z\\)**"));
        assert!(!msg.contains("@channel"));
        assert!(msg.contains("```\ndeaddrop fetch blobT\n```"));
        assert!(msg.contains("expires in 30m."));
        assert!(!msg.contains("DEADDROP_KEY"));
        let msg = render(&ev.with_key("AGE-SECRET-KEY-1X"), 100);
        assert!(msg.contains("`AGE-SECRET-KEY-1X`"));
    }
}
