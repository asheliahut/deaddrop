//! SMS through Twilio's Messages API.
//!
//! Needs the Account SID and Auth Token from the Twilio console (or an API
//! key's SID and secret), a sending number, alphanumeric sender id or
//! Messaging Service, and the phone number to text.

use anyhow::{Result, bail};

use super::{BoxFuture, Event, Notifier, now, parse_http_url, send_checked, time_left};
use crate::torrent::human;

const DEFAULT_API: &str = "https://api.twilio.com";

/// Twilio's limit for one message body (split into SMS segments).
const MAX_BODY: usize = 1600;

pub struct Twilio {
    /// `<api>/2010-04-01/Accounts/<sid>/Messages.json`.
    url: reqwest::Url,
    user: String,
    secret: String,
    from: From,
    to: String,
    http: reqwest::Client,
}

/// Who the SMS comes from.
enum From {
    /// A phone number or alphanumeric sender id.
    Sender(String),
    /// A Messaging Service (`MG...`), which picks the number itself.
    Service(String),
}

/// The settings, as written in the config.
pub struct Settings<'a> {
    pub account_sid: &'a str,
    pub auth_token: &'a str,
    pub api_key: Option<&'a str>,
    pub from: &'a str,
    pub to: &'a str,
    pub api_url: Option<&'a str>,
}

impl Twilio {
    pub fn new(s: Settings<'_>, http: reqwest::Client) -> Result<Self> {
        let account_sid = s.account_sid.trim();
        if !is_sid(account_sid, "AC") {
            bail!("twilio account_sid should be AC followed by 32 hex characters");
        }
        let user = match s.api_key.map(str::trim) {
            Some(k) if !is_sid(k, "SK") => {
                bail!("twilio api_key should be SK followed by 32 hex characters")
            }
            Some(k) => k.to_string(),
            None => account_sid.to_string(),
        };
        let secret = s.auth_token.trim();
        if secret.is_empty() {
            bail!("twilio auth_token is empty");
        }
        let from = s.from.trim();
        let from = if is_sid(from, "MG") {
            From::Service(from.to_string())
        } else if is_phone(from) || is_sender_id(from) {
            From::Sender(from.to_string())
        } else {
            bail!(
                "twilio from should be a phone number like +15551234567, an alphanumeric sender id, or a Messaging Service SID (MG...)"
            );
        };
        let to = s.to.trim();
        if !is_phone(to) {
            bail!("twilio to should be a phone number in E.164 form, like +15551234567");
        }
        let mut url = parse_http_url(s.api_url.unwrap_or(DEFAULT_API))?;
        if url.query().is_some() || url.fragment().is_some() {
            bail!("twilio api_url can't have a query string or fragment");
        }
        let base = url.path().trim_end_matches('/').to_string();
        url.set_path(&format!(
            "{base}/2010-04-01/Accounts/{account_sid}/Messages.json"
        ));
        Ok(Self {
            url,
            user,
            secret: secret.to_string(),
            from,
            to: to.to_string(),
            http,
        })
    }
}

/// `<prefix>` followed by 32 hex digits, like Twilio's SIDs. Also keeps the
/// account SID from changing the request path.
fn is_sid(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix)
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// E.164: `+`, then 7 to 15 digits, not starting with 0.
fn is_phone(s: &str) -> bool {
    s.strip_prefix('+').is_some_and(|d| {
        (7..=15).contains(&d.len()) && !d.starts_with('0') && d.bytes().all(|b| b.is_ascii_digit())
    })
}

/// An alphanumeric sender id: up to 11 letters, digits and spaces, with at
/// least one letter. Only some countries allow these.
fn is_sender_id(s: &str) -> bool {
    (1..=11).contains(&s.len())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' ')
}

impl Notifier for Twilio {
    fn label(&self) -> String {
        // Enough of the number to tell recipients apart in the logs.
        let tail = &self.to[self.to.len() - 4..];
        format!("twilio (sms to …{tail})")
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let body = render(event, now())?;
            let from = match &self.from {
                From::Sender(s) => ("From", s.as_str()),
                From::Service(s) => ("MessagingServiceSid", s.as_str()),
            };
            let form = [("To", self.to.as_str()), from, ("Body", body.as_str())];
            let req = self
                .http
                .post(self.url.clone())
                .basic_auth(&self.user, Some(&self.secret))
                .form(&form);
            send_checked(req).await
        })
    }
}

/// The SMS text. Plain ASCII where it can be (emoji make every SMS segment
/// hold far fewer characters), and only the name is shortened, so the
/// ticket is never cut.
fn render(event: &Event, now: u64) -> Result<String> {
    match event {
        Event::TicketReady(t) => {
            let mode = if t.single_use {
                "single use"
            } else {
                "multi use"
            };
            let name: String = if t.name.chars().count() > 60 {
                t.name.chars().take(59).chain(['…']).collect()
            } else {
                t.name.clone()
            };
            let mut msg = format!(
                "deaddrop: {name} is ready ({} file(s), {}, {mode}).",
                t.files,
                human(t.total_bytes),
            );
            if t.reset {
                msg.push_str(" It replaces the previous ticket.");
            }
            if let Some(at) = t.expires_at {
                msg.push_str(&format!(" Expires in {}.", time_left(at, now)));
            }
            msg.push_str(&format!("\n\n{}", t.fetch_command));
            if let Some(k) = &t.key {
                msg.push_str(&format!("\n\nDEADDROP_KEY={k}"));
            }
            if msg.chars().count() > MAX_BODY {
                bail!("message is over Twilio's {MAX_BODY}-character limit");
            }
            Ok(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    const AC: &str = "AC0123456789abcdef0123456789abcdef";
    const SK: &str = "SK0123456789abcdef0123456789abcdef";
    const MG: &str = "MG0123456789abcdef0123456789abcdef";

    fn settings<'a>() -> Settings<'a> {
        Settings {
            account_sid: AC,
            auth_token: "secret",
            api_key: None,
            from: "+15550001111",
            to: "+15552223333",
            api_url: None,
        }
    }

    fn tw(s: Settings<'_>) -> Result<Twilio> {
        Twilio::new(s, reqwest::Client::new())
    }

    #[test]
    fn settings_check() {
        let t = tw(settings()).unwrap();
        assert_eq!(
            t.url.as_str(),
            format!("https://api.twilio.com/2010-04-01/Accounts/{AC}/Messages.json")
        );
        assert_eq!(t.user, AC);
        assert_eq!(t.label(), "twilio (sms to …3333)");
        assert!(matches!(t.from, From::Sender(_)));

        let t = tw(Settings {
            api_key: Some(SK),
            from: MG,
            ..settings()
        })
        .unwrap();
        assert_eq!(t.user, SK);
        assert!(matches!(t.from, From::Service(_)));
        assert!(
            tw(Settings {
                from: "Deaddrop",
                ..settings()
            })
            .is_ok()
        );

        let bad = |s: Settings<'_>| assert!(tw(s).is_err());
        bad(Settings {
            account_sid: "AC/../x",
            ..settings()
        });
        bad(Settings {
            account_sid: SK,
            ..settings()
        });
        bad(Settings {
            api_key: Some(AC),
            ..settings()
        });
        bad(Settings {
            auth_token: " ",
            ..settings()
        });
        bad(Settings {
            to: "5552223333",
            ..settings()
        });
        bad(Settings {
            to: "+0123456789",
            ..settings()
        });
        bad(Settings {
            from: "12345",
            ..settings()
        });
        bad(Settings {
            from: "TooLongSenderId",
            ..settings()
        });
        bad(Settings {
            api_url: Some("ftp://x"),
            ..settings()
        });
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "x".repeat(100),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 2,
            total_bytes: 2048,
            single_use: true,
            reset: false,
            expires_at: Some(60 + 7200),
            key: None,
        });
        let msg = render(&ev, 60).unwrap();
        assert!(msg.contains(&format!("{}… is ready", "x".repeat(59))));
        assert!(msg.contains("Expires in 2h."));
        assert!(msg.ends_with("\n\ndeaddrop fetch blobT"));
        let msg = render(&ev.with_key("AGE-SECRET-KEY-1X"), 60).unwrap();
        assert!(msg.ends_with("DEADDROP_KEY=AGE-SECRET-KEY-1X"));

        let Event::TicketReady(t) = ev;
        let huge = Event::TicketReady(TicketReady {
            fetch_command: "y".repeat(MAX_BODY),
            ..t
        });
        assert!(render(&huge, 0).is_err());
    }
}
