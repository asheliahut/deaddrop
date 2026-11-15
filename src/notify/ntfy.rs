//! [ntfy](https://ntfy.sh) push notifications, on ntfy.sh or a self-hosted
//! server.
//!
//! The URL is the topic URL, e.g. `https://ntfy.sh/my-secret-topic` or
//! `https://ntfy.example.com/deaddrop`. The message is published as JSON to
//! the server root, so names from the torrent never have to fit in HTTP
//! headers. Protected topics take an access token (`token`, sent as
//! `Authorization: Bearer`) or `user:pass@` in the URL (basic auth).

use anyhow::{Result, bail};
use serde_json::json;

use super::{BoxFuture, Event, Notifier, now, parse_http_url, send_checked, time_left};
use crate::torrent::human;

pub struct Ntfy {
    /// The server root that JSON messages are published to.
    base: reqwest::Url,
    topic: String,
    token: Option<String>,
    priority: Option<u8>,
    http: reqwest::Client,
}

impl Ntfy {
    pub fn new(
        target: &str,
        token: Option<&str>,
        priority: Option<u8>,
        http: reqwest::Client,
    ) -> Result<Self> {
        let url = parse_http_url(target)?;
        let (base, topic) = split_topic(&url)?;
        if let Some(p) = priority
            && !(1..=5).contains(&p)
        {
            bail!("ntfy priority must be 1 (min) to 5 (max), got {p}");
        }
        let token = match token.map(str::trim) {
            Some("") => bail!("ntfy token is empty"),
            Some(_) if !url.username().is_empty() => {
                bail!("ntfy: use either a token or user:pass@ in the URL, not both")
            }
            t => t.map(str::to_string),
        };
        Ok(Self {
            base,
            topic,
            token,
            priority,
            http,
        })
    }
}

/// Split `https://host/prefix/topic` into the server root
/// (`https://host/prefix/`) and the topic. Errors don't include the URL.
fn split_topic(url: &reqwest::Url) -> Result<(reqwest::Url, String)> {
    let path = url.path().trim_end_matches('/');
    let Some((prefix, topic)) = path.rsplit_once('/') else {
        bail!("ntfy URL needs a topic, e.g. https://ntfy.sh/<topic>");
    };
    // ntfy's own rule for topic names.
    if topic.is_empty()
        || topic.len() > 64
        || !topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!(
            "ntfy URL needs a topic of 1-64 letters, digits, '-' or '_', e.g. https://ntfy.sh/<topic>"
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!(
            "ntfy URL can't have a query string or fragment; set the token in the config instead"
        );
    }
    let mut base = url.clone();
    base.set_path(&format!("{prefix}/"));
    Ok((base, topic.to_string()))
}

impl Notifier for Ntfy {
    fn label(&self) -> String {
        // Host only: on a public server the topic name is the secret.
        format!("ntfy ({})", self.base.host_str().unwrap_or("?"))
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut body = render(event, now());
            body["topic"] = json!(self.topic);
            if let Some(p) = self.priority {
                body["priority"] = json!(p);
            }
            let mut req = self.http.post(self.base.clone()).json(&body);
            if let Some(token) = &self.token {
                req = req.bearer_auth(token);
            }
            send_checked(req).await
        })
    }
}

/// The ntfy JSON message, without the topic. Plain text, since not every
/// ntfy client renders markdown.
fn render(event: &Event, now: u64) -> serde_json::Value {
    match event {
        Event::TicketReady(t) => {
            let mode = if t.single_use {
                "single use: the server shuts down after one download"
            } else {
                "multi use"
            };
            let mut msg = format!("{} file(s), {}, {mode}", t.files, human(t.total_bytes));
            if t.reset {
                msg.push_str("\nThis replaces the previous ticket, which no longer works.");
            }
            if let Some(at) = t.expires_at {
                msg.push_str(&format!("\nThe ticket expires in {}.", time_left(at, now)));
            }
            msg.push_str(&format!("\n\n{}", t.fetch_command));
            if let Some(k) = &t.key {
                msg.push_str(&format!("\n\nDecryption key (set as DEADDROP_KEY):\n{k}"));
            }
            let title: String = if t.name.chars().count() > 200 {
                t.name.chars().take(199).chain(['…']).collect()
            } else {
                t.name.clone()
            };
            let mut tags = vec!["package"];
            if t.reset {
                tags.push("repeat");
            }
            if t.key.is_some() {
                tags.push("key");
            }
            json!({
                "title": format!("{title} is ready"),
                "message": msg,
                "tags": tags,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    fn ntfy(url: &str) -> Result<Ntfy> {
        Ntfy::new(url, None, None, reqwest::Client::new())
    }

    #[test]
    fn topic_urls() {
        let n = ntfy("https://ntfy.sh/my-Topic_1").unwrap();
        assert_eq!(n.base.as_str(), "https://ntfy.sh/");
        assert_eq!(n.topic, "my-Topic_1");
        let n = ntfy("https://u:p@example.com/ntfy/drops/").unwrap();
        assert_eq!(n.base.as_str(), "https://u:p@example.com/ntfy/");
        assert_eq!(n.topic, "drops");
        assert_eq!(n.label(), "ntfy (example.com)");

        assert!(ntfy("https://ntfy.sh").is_err());
        assert!(ntfy("https://ntfy.sh/").is_err());
        assert!(ntfy("https://ntfy.sh/bad.topic").is_err());
        assert!(ntfy("https://ntfy.sh/topic?auth=x").is_err());
        assert!(ntfy(&format!("https://ntfy.sh/{}", "a".repeat(65))).is_err());
        assert!(ntfy("ftp://ntfy.sh/topic").is_err());
    }

    #[test]
    fn options() {
        let http = reqwest::Client::new;
        assert!(Ntfy::new("https://ntfy.sh/t", Some("tk_x"), Some(5), http()).is_ok());
        assert!(Ntfy::new("https://ntfy.sh/t", None, Some(0), http()).is_err());
        assert!(Ntfy::new("https://ntfy.sh/t", None, Some(6), http()).is_err());
        assert!(Ntfy::new("https://ntfy.sh/t", Some(" "), None, http()).is_err());
        assert!(Ntfy::new("https://u:p@ntfy.sh/t", Some("tk_x"), None, http()).is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "ubuntu.iso".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: true,
            reset: true,
            expires_at: Some(1_000 + 90 * 60),
            key: None,
        });
        let v = render(&ev, 1_000);
        assert_eq!(v["title"], "ubuntu.iso is ready");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.contains("deaddrop fetch blobT"));
        assert!(msg.contains("expires in 1h30m"));
        assert!(msg.contains("replaces the previous ticket"));
        assert!(!msg.contains("DEADDROP_KEY"));
        assert_eq!(v["tags"], json!(["package", "repeat"]));

        let v = render(&ev.with_key("AGE-SECRET-KEY-1X"), 1_000);
        assert!(v["message"].as_str().unwrap().contains("AGE-SECRET-KEY-1X"));
        assert_eq!(v["tags"], json!(["package", "repeat", "key"]));
    }
}
