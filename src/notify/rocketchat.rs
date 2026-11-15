//! Rocket.Chat incoming webhook (Administration → Integrations → New →
//! Incoming).
//!
//! The torrent's name goes in an attachment title, which Rocket.Chat shows
//! as plain text: no markup to escape, and no `@all` mentions from a name.

use anyhow::Result;
use serde_json::json;

use super::{BoxFuture, Event, Notifier, now, parse_http_url, post_json, time_left};
use crate::torrent::human;

pub struct RocketChat {
    url: reqwest::Url,
    http: reqwest::Client,
}

impl RocketChat {
    pub fn new(target: &str, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            url: parse_http_url(target)?,
            http,
        })
    }
}

impl Notifier for RocketChat {
    fn label(&self) -> String {
        // Host only: the path holds the webhook's token.
        format!("rocketchat ({})", self.url.host_str().unwrap_or("?"))
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut body = render(event, now());
            body["alias"] = json!("deaddrop");
            body["emoji"] = json!(":package:");
            post_json(&self.http, &self.url, &body).await
        })
    }
}

/// The webhook body: a fixed message line, with everything from the torrent
/// in an attachment.
fn render(event: &Event, now: u64) -> serde_json::Value {
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
            let mut details = format!("{} file(s), {}, {mode}", t.files, human(t.total_bytes));
            if t.reset {
                details.push_str(
                    "\n:repeat: This replaces the previous ticket, which no longer works.",
                );
            }
            if let Some(at) = t.expires_at {
                details.push_str(&format!(
                    "\n:hourglass_flowing_sand: The ticket expires in {}.",
                    time_left(at, now)
                ));
            }
            details.push_str(&format!("\n```\n{}\n```", t.fetch_command));
            if let Some(k) = &t.key {
                // Rocket.Chat has no spoilers; at least keep it easy to copy.
                details.push_str(&format!(
                    "\n:key: Decryption key (set as DEADDROP_KEY): `{k}`"
                ));
            }
            let text = if t.reset {
                "A deaddrop was replaced"
            } else {
                "A deaddrop is ready"
            };
            json!({
                "text": text,
                "attachments": [{
                    "title": name,
                    "text": details,
                    "color": "#7c3aed",
                }],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    #[test]
    fn settings() {
        let http = reqwest::Client::new;
        let r = RocketChat::new("https://chat.example.com/hooks/ID/SECRET", http()).unwrap();
        assert_eq!(r.label(), "rocketchat (chat.example.com)");
        assert!(RocketChat::new("not a url", http()).is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "@all *x*".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: false,
            reset: true,
            expires_at: None,
            key: None,
        });
        let v = render(&ev, 0);
        assert_eq!(v["text"], "A deaddrop was replaced");
        assert!(!v["text"].as_str().unwrap().contains('@'));
        let a = &v["attachments"][0];
        assert_eq!(a["title"], "@all *x*");
        let details = a["text"].as_str().unwrap();
        assert!(details.contains("```\ndeaddrop fetch blobT\n```"));
        assert!(details.contains("replaces the previous ticket"));
        assert!(!details.contains("DEADDROP_KEY"));
        let v = render(&ev.with_key("AGE-SECRET-KEY-1X"), 0);
        assert!(
            v["attachments"][0]["text"]
                .as_str()
                .unwrap()
                .contains("`AGE-SECRET-KEY-1X`")
        );
    }
}
