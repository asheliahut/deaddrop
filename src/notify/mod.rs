//! Notifications: tell someone when a ticket is ready.
//!
//! Notifiers are configured as [`NotifierConfig`], either as TOML tables in
//! the config file:
//!
//! ```toml
//! [[serve.notify]]
//! type = "discord"
//! url = "https://discord.com/api/webhooks/..."
//! ```
//!
//! or with the `--notify <type>=<url>` shorthand on the command line.
//!
//! When the server encrypts the data (`--encrypt`), the decryption key is
//! only sent to notifiers with `include_key = true` (shorthand:
//! `<type>+key=<url>`). The others get the ticket alone.
//!
//! # Adding a new notifier
//!
//! 1. Create `src/notify/<type>.rs` with a type that implements [`Notifier`].
//! 2. Add a `mod` line below, a variant to [`NotifierConfig`] holding its
//!    settings, and an arm to the `match` in [`NotifierConfig::build`].
//!
//! If the new notifier's main setting is a field called `url`, the
//! `--notify <type>=<url>` shorthand works for it with no extra code.

mod discord;
mod webhook;

use std::time::Duration;

use anyhow::{Context, Result, bail};
/// A boxed, sendable future borrowing for `'a`.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;
use serde::{Deserialize, Serialize};

/// How long a single notifier gets before it is abandoned.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

/// Something worth telling the outside world about.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// The torrent finished downloading and the data is being served.
    TicketReady(TicketReady),
}

#[derive(Debug, Clone, Serialize)]
pub struct TicketReady {
    /// The torrent's name.
    pub name: String,
    /// The iroh ticket to pass to `deaddrop fetch`.
    pub ticket: String,
    /// The full command a client should run.
    pub fetch_command: String,
    pub files: usize,
    pub total_bytes: u64,
    /// True if the server shuts down after one download.
    pub single_use: bool,
    /// True if this replaces an earlier ticket after a reset. The old ticket
    /// no longer works.
    pub reset: bool,
    /// The decryption key. Only set for notifiers with `include_key`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Event {
    /// A copy of this event carrying the decryption key.
    fn with_key(&self, key: &str) -> Self {
        match self {
            Self::TicketReady(t) => Self::TicketReady(TicketReady {
                key: Some(key.to_string()),
                ..t.clone()
            }),
        }
    }
}

/// A destination for notifications.
///
/// This uses a boxed future instead of `async fn` so notifiers can be stored
/// as `Box<dyn Notifier>`.
pub trait Notifier: Send + Sync {
    /// A short label for logs, e.g. "discord". Must not include secrets such
    /// as webhook URLs.
    fn label(&self) -> String;

    /// Deliver one event.
    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>>;
}

/// Settings for one notifier. The `type` field picks the variant.
#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum NotifierConfig {
    /// Discord channel webhook.
    Discord {
        url: String,
        #[serde(default)]
        include_key: bool,
    },
    /// Generic JSON webhook.
    Webhook {
        url: String,
        #[serde(default)]
        include_key: bool,
    },
}

// Hand-written so secrets (webhook URLs) never show up in debug output.
impl std::fmt::Debug for NotifierConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Discord { .. } => "discord",
            Self::Webhook { .. } => "webhook",
        };
        let key = if self.include_key() { ", with key" } else { "" };
        write!(f, "NotifierConfig({kind}{key}, <redacted>)")
    }
}

impl NotifierConfig {
    /// Parse the CLI shorthand `<type>=<url>`, or `<type>+key=<url>` to also
    /// send the decryption key. Errors don't echo the input, since it may
    /// contain a secret.
    pub fn parse_shorthand(spec: &str) -> Result<Self> {
        let Some((kind, url)) = spec.split_once('=') else {
            bail!("expected <type>=<url>, e.g. discord=https://discord.com/api/webhooks/...");
        };
        let kind = kind.trim().to_ascii_lowercase();
        let (kind, include_key) = match kind.strip_suffix("+key") {
            Some(k) => (k.to_string(), true),
            None => (kind, false),
        };
        let value = serde_json::json!({
            "type": kind,
            "url": url.trim(),
            "include_key": include_key,
        });
        serde_json::from_value(value).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Whether this notifier should receive the decryption key.
    pub fn include_key(&self) -> bool {
        match self {
            Self::Discord { include_key, .. } | Self::Webhook { include_key, .. } => *include_key,
        }
    }

    fn build(&self, http: &reqwest::Client) -> Result<Box<dyn Notifier>> {
        Ok(match self {
            Self::Discord { url, .. } => Box::new(discord::Discord::new(url, http.clone())?),
            Self::Webhook { url, .. } => Box::new(webhook::Webhook::new(url, http.clone())?),
        })
    }
}

/// One configured notifier.
struct Entry {
    notifier: Box<dyn Notifier>,
    include_key: bool,
}

/// All configured notifiers.
pub struct Notifiers(Vec<Entry>);

impl Notifiers {
    /// Build every notifier up front, so a bad URL fails before the torrent
    /// download starts rather than after it.
    pub fn new(configs: &[NotifierConfig]) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("deaddrop/", env!("CARGO_PKG_VERSION")))
            .timeout(SEND_TIMEOUT)
            .build()?;
        configs
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let notifier = c
                    .build(&http)
                    .with_context(|| format!("bad notifier #{}", i + 1))?;
                Ok(Entry {
                    notifier,
                    include_key: c.include_key(),
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Self)
    }

    /// Log-safe labels (no secrets), one per notifier.
    pub fn labels(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|e| {
                let label = e.notifier.label();
                if e.include_key {
                    format!("{label} (with key)")
                } else {
                    label
                }
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether any notifier gets the decryption key.
    pub fn sends_key(&self) -> bool {
        self.0.iter().any(|e| e.include_key)
    }

    /// Send `event` to every notifier at the same time, adding `key` (if the
    /// data is encrypted) for the ones with `include_key`. Failures are
    /// logged but don't stop the server, since the ticket is also printed
    /// locally.
    pub async fn send_all(&self, event: &Event, key: Option<&str>) -> Sent {
        let with_key = key.map(|k| event.with_key(k));
        let results = n0_future::join_all(self.0.iter().map(|e| {
            let (event, got_key) = match &with_key {
                Some(with_key) if e.include_key => (with_key, true),
                _ => (event, false),
            };
            async move {
                let res = tokio::time::timeout(SEND_TIMEOUT, e.notifier.send(event))
                    .await
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("timed out")));
                (e.notifier.label(), got_key, res)
            }
        }))
        .await;
        let mut sent = Sent::default();
        for (label, with_key, res) in results {
            match res {
                Ok(()) => {
                    sent.ok += 1;
                    if with_key {
                        sent.with_key += 1;
                    }
                    say!("notify: sent to {label}");
                }
                Err(e) => say!("notify: warning: {label} failed: {e:#}"),
            }
        }
        sent
    }
}

/// What [`Notifiers::send_all`] managed to deliver.
#[derive(Debug, Default)]
pub struct Sent {
    /// Notifiers that succeeded.
    pub ok: usize,
    /// Of those, how many got the decryption key.
    pub with_key: usize,
}

/// Parse and check an http(s) URL. Errors don't include the URL.
fn parse_http_url(s: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(s).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => bail!("URL scheme must be http or https, got {other:?}"),
    }
}

/// POST JSON and turn non-2xx responses into errors. The URL is stripped
/// from errors so secrets don't end up in logs.
async fn post_json(
    http: &reqwest::Client,
    url: &reqwest::Url,
    body: &impl Serialize,
) -> Result<()> {
    let resp = http
        .post(url.clone())
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let text: String = text.chars().take(200).collect();
        bail!("HTTP {status}: {text}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_parsing() {
        let http = reqwest::Client::new();
        let ok = |s: &str| {
            NotifierConfig::parse_shorthand(s)
                .and_then(|c| c.build(&http))
                .is_ok()
        };
        assert!(ok("webhook=https://example.com/hook"));
        assert!(ok("discord+key=https://discord.com/api/webhooks/1/abc"));
        assert!(!ok("discord+nope=https://x"));
        let c = NotifierConfig::parse_shorthand("Webhook+Key=https://x/h").unwrap();
        assert!(c.include_key());
        assert!(
            !NotifierConfig::parse_shorthand("webhook=https://x/h")
                .unwrap()
                .include_key()
        );
        assert!(ok("DISCORD=https://discord.com/api/webhooks/1/abc"));
        assert!(!ok("webhook"));
        assert!(!ok("carrier-pigeon=https://x"));
        assert!(!ok("webhook=ftp://example.com"));
        assert!(!ok("webhook=not a url"));
    }

    #[test]
    fn debug_redacts_secrets() {
        let c = NotifierConfig::parse_shorthand("discord=https://x/SECRET").unwrap();
        assert!(!format!("{c:?}").contains("SECRET"));
    }

    #[test]
    fn event_json_shape() {
        let ev = Event::TicketReady(TicketReady {
            name: "n".into(),
            ticket: "t".into(),
            fetch_command: "deaddrop fetch t".into(),
            files: 2,
            total_bytes: 10,
            single_use: true,
            reset: false,
            key: None,
        });
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["event"], "ticket_ready");
        assert_eq!(v["ticket"], "t");
        assert_eq!(v["single_use"], true);
        assert_eq!(v["reset"], false);
        assert!(v.get("key").is_none());
        let v = serde_json::to_value(ev.with_key("K")).unwrap();
        assert_eq!(v["key"], "K");
    }
}
