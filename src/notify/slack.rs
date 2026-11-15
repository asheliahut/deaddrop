//! Slack incoming webhook (a Slack app with Incoming Webhooks turned on).

use anyhow::Result;
use serde_json::json;

use super::{BoxFuture, Event, Notifier, now, parse_http_url, post_json, time_left};
use crate::torrent::human;

pub struct Slack {
    url: reqwest::Url,
    http: reqwest::Client,
}

impl Slack {
    pub fn new(target: &str, http: reqwest::Client) -> Result<Self> {
        Ok(Self {
            url: parse_http_url(target)?,
            http,
        })
    }
}

impl Notifier for Slack {
    fn label(&self) -> String {
        // The webhook URL is the secret, so only say what kind it is.
        "slack".into()
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = json!({
                "text": render(event, now()),
                "unfurl_links": false,
                "unfurl_media": false,
            });
            post_json(&self.http, &self.url, &body).await
        })
    }
}

/// Message text in Slack's mrkdwn. Only the name is shortened, so the
/// ticket is never cut.
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
                ":package: *{}* is ready ({} file(s), {}, {mode})",
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
                // Slack shows this in each reader's own time zone; the text
                // after `|` is for clients that can't.
                msg.push_str(&format!(
                    "\n:hourglass_flowing_sand: The ticket expires <!date^{at}^{{date_short_pretty}} at {{time}}|in {}>.",
                    time_left(at, now)
                ));
            }
            msg.push_str(&format!("\n```{}```", escape(&t.fetch_command)));
            if let Some(k) = &t.key {
                // Slack has no spoilers; at least keep it easy to copy.
                msg.push_str(&format!(
                    "\n:key: Decryption key (set as DEADDROP_KEY): `{}`",
                    escape(k)
                ));
            }
            msg
        }
    }
}

/// Escape the characters Slack treats as markup (links and mentions such
/// as `<!channel>`).
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    #[test]
    fn settings() {
        let http = reqwest::Client::new;
        let s = Slack::new("https://hooks.slack.com/services/T0/B0/SECRET", http()).unwrap();
        assert!(!s.label().contains("SECRET"));
        assert!(Slack::new("ftp://hooks.slack.com/x", http()).is_err());
        assert!(Slack::new("not a url", http()).is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "<!channel> & co".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: true,
            reset: true,
            expires_at: Some(100 + 1800),
            key: None,
        });
        let msg = render(&ev, 100);
        assert!(msg.contains("*&lt;!channel&gt; &amp; co*"));
        assert!(!msg.contains("<!channel>"));
        assert!(msg.contains("```deaddrop fetch blobT```"));
        assert!(msg.contains("<!date^1900^{date_short_pretty} at {time}|in 30m>"));
        assert!(msg.contains("replaces the previous ticket"));
        assert!(!msg.contains("DEADDROP_KEY"));
        let msg = render(&ev.with_key("AGE-SECRET-KEY-1X"), 100);
        assert!(msg.contains("`AGE-SECRET-KEY-1X`"));
    }
}
