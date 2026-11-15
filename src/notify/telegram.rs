//! Telegram bot message (Bot API `sendMessage`).
//!
//! Create a bot with @BotFather to get its token, add it to the chat (or
//! start a private chat with it), and use the chat's id: a number like
//! `123456789` or `-1001234567890`, or `@channelname` for a public channel.

use anyhow::{Result, bail};
use serde_json::json;

use super::{BoxFuture, Event, Notifier, now, parse_http_url, post_json, time_left};
use crate::torrent::human;

/// Where the official Bot API lives. A self-hosted Bot API server can be
/// used instead with `api_url`.
const DEFAULT_API: &str = "https://api.telegram.org";

pub struct Telegram {
    /// `<api>/bot<token>/sendMessage`. Contains the token: never log it.
    url: reqwest::Url,
    chat_id: serde_json::Value,
    thread_id: Option<i64>,
    http: reqwest::Client,
}

impl Telegram {
    pub fn new(
        bot_token: &str,
        chat_id: &str,
        thread_id: Option<i64>,
        api_url: Option<&str>,
        http: reqwest::Client,
    ) -> Result<Self> {
        let bot_token = bot_token.trim();
        if !valid_token(bot_token) {
            bail!("telegram bot_token should look like 123456789:AAE..., as given by @BotFather");
        }
        let chat_id = parse_chat_id(chat_id.trim())?;
        let mut url = parse_http_url(api_url.unwrap_or(DEFAULT_API))?;
        if url.query().is_some() || url.fragment().is_some() {
            bail!("telegram api_url can't have a query string or fragment");
        }
        let base = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!("{base}/bot{bot_token}/sendMessage"));
        Ok(Self {
            url,
            chat_id,
            thread_id,
            http,
        })
    }
}

/// `<digits>:<letters, digits, '-' or '_'>`. Checked so the token can't
/// change the request path.
fn valid_token(t: &str) -> bool {
    let Some((id, secret)) = t.split_once(':') else {
        return false;
    };
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && !secret.is_empty()
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A numeric chat id is sent as a JSON number, `@channel` as a string.
fn parse_chat_id(s: &str) -> Result<serde_json::Value> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(json!(n));
    }
    if let Some(name) = s.strip_prefix('@')
        && name.len() >= 4
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Ok(json!(s));
    }
    bail!("telegram chat_id must be a number like -1001234567890 or @channelname");
}

impl Notifier for Telegram {
    fn label(&self) -> String {
        // The URL holds the bot token, so only say what kind it is.
        "telegram".into()
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut body = json!({
                "chat_id": self.chat_id,
                "text": render(event, now()),
                "parse_mode": "HTML",
                "link_preview_options": { "is_disabled": true },
            });
            if let Some(t) = self.thread_id {
                body["message_thread_id"] = json!(t);
            }
            post_json(&self.http, &self.url, &body).await
        })
    }
}

/// Message text in Telegram's HTML subset. Telegram limits messages to 4096
/// characters, so only the name is shortened and the ticket is never cut.
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
                "📦 <b>{}</b> is ready ({} file(s), {}, {mode})",
                escape(&name),
                t.files,
                human(t.total_bytes),
            );
            if t.reset {
                msg.push_str("\n🔁 This replaces the previous ticket, which no longer works.");
            }
            if let Some(at) = t.expires_at {
                msg.push_str(&format!(
                    "\n⏳ The ticket expires in {}.",
                    time_left(at, now)
                ));
            }
            // <pre> blocks get a copy button in Telegram's apps.
            msg.push_str(&format!("\n<pre>{}</pre>", escape(&t.fetch_command)));
            if let Some(k) = &t.key {
                // A spoiler, so the key isn't readable at a glance.
                msg.push_str(&format!(
                    "\n🔑 Decryption key (set as DEADDROP_KEY): <tg-spoiler><code>{}</code></tg-spoiler>",
                    escape(k)
                ));
            }
            msg
        }
    }
}

/// Escape text for Telegram's HTML parse mode.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    const TOKEN: &str = "123456:AAE-x_y";

    fn tg(token: &str, chat: &str, api: Option<&str>) -> Result<Telegram> {
        Telegram::new(token, chat, None, api, reqwest::Client::new())
    }

    #[test]
    fn settings() {
        let t = tg(TOKEN, "-1001234", None).unwrap();
        assert_eq!(
            t.url.as_str(),
            "https://api.telegram.org/bot123456:AAE-x_y/sendMessage"
        );
        assert_eq!(t.chat_id, json!(-1001234));
        assert_eq!(t.label(), "telegram");
        let t = tg(TOKEN, "@my_channel", Some("http://localhost:8081/tg/")).unwrap();
        assert_eq!(
            t.url.as_str(),
            "http://localhost:8081/tg/bot123456:AAE-x_y/sendMessage"
        );
        assert_eq!(t.chat_id, json!("@my_channel"));

        assert!(tg("nocolon", "1", None).is_err());
        assert!(tg("abc:def", "1", None).is_err());
        assert!(tg("123:", "1", None).is_err());
        assert!(tg("123:a/../b", "1", None).is_err());
        assert!(tg(TOKEN, "", None).is_err());
        assert!(tg(TOKEN, "channel", None).is_err());
        assert!(tg(TOKEN, "@a b", None).is_err());
        assert!(tg(TOKEN, "1", Some("ftp://x")).is_err());
        assert!(tg(TOKEN, "1", Some("https://x/?a=b")).is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "<b>x</b> & y".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: false,
            reset: false,
            expires_at: None,
            key: None,
        });
        let msg = render(&ev, 0);
        assert!(msg.contains("<b>&lt;b&gt;x&lt;/b&gt; &amp; y</b>"));
        assert!(msg.contains("<pre>deaddrop fetch blobT</pre>"));
        assert!(!msg.contains("DEADDROP_KEY") && !msg.contains("expires"));
        let Event::TicketReady(t) = ev.with_key("AGE-SECRET-KEY-1X");
        let msg = render(
            &Event::TicketReady(TicketReady {
                expires_at: Some(100 + 3600),
                ..t
            }),
            100,
        );
        assert!(msg.contains("expires in 1h."));
        assert!(msg.contains("<tg-spoiler><code>AGE-SECRET-KEY-1X</code></tg-spoiler>"));
    }
}
