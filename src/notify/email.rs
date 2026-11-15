//! Email over SMTP.
//!
//! The server is a URL:
//!
//! - `smtps://user:pass@smtp.example.com` – TLS from the start (port 465).
//! - `smtp://user:pass@smtp.example.com` – STARTTLS, required (port 587).
//!   `?tls=opportunistic` uses it only if the server offers it.
//! - `smtp://localhost?tls=none` – no TLS (port 25), e.g. a local relay.
//!   Not allowed with a password unless the host is local.
//!
//! The sender and recipients go in `from` and `to`, either as settings or in
//! the URL's query string (`?from=a@x.org&to=b@y.org&to=c@z.org`), so the
//! `--notify email=<url>` shorthand works too. A user name or password with
//! `@`, `:` or `/` in it must be percent-encoded (`%40`, `%3A`, `%2F`).

use std::net::IpAddr;

use anyhow::{Context, Result, bail};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use super::{BoxFuture, Event, Notifier, SEND_TIMEOUT, now, time_left};
use crate::torrent::human;

pub struct Email {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    host: String,
    from: Mailbox,
    to: Vec<Mailbox>,
}

impl Email {
    pub fn new(target: &str, from: Option<&str>, to: &[String]) -> Result<Self> {
        let url = reqwest::Url::parse(target).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
        let host = match url.host_str() {
            // IPv6 hosts come in brackets, which the SMTP client doesn't want.
            Some(h) if !h.is_empty() => h.trim_start_matches('[').trim_end_matches(']').to_string(),
            _ => bail!("email URL needs a server, e.g. smtps://user:pass@smtp.example.com"),
        };
        if !matches!(url.path(), "" | "/") || url.fragment().is_some() {
            bail!("email URL can't have a path or fragment");
        }

        let (mut tls_mode, mut from_q, mut to_q) = (None, None, Vec::new());
        for (k, v) in url.query_pairs() {
            match &*k {
                "tls" if tls_mode.is_none() => tls_mode = Some(v.into_owned()),
                "from" if from_q.is_none() => from_q = Some(v.into_owned()),
                "to" => to_q.push(v.into_owned()),
                "tls" | "from" => bail!("email URL has {k} twice"),
                _ => bail!("email URL has an unknown setting {k:?}; known: tls, from, to"),
            }
        }

        let params = || {
            TlsParameters::new(host.clone()).map_err(|e| anyhow::anyhow!("TLS setup failed: {e}"))
        };
        let (tls, default_port) = match (url.scheme(), tls_mode.as_deref()) {
            ("smtps", None | Some("wrapper")) => (Tls::Wrapper(params()?), 465),
            ("smtp", None | Some("required")) => (Tls::Required(params()?), 587),
            ("smtp", Some("opportunistic")) => (Tls::Opportunistic(params()?), 587),
            ("smtp", Some("none")) => (Tls::None, 25),
            ("smtp" | "smtps", Some(other)) => bail!(
                "email URL tls must be required, opportunistic or none (smtp://), got {other:?}"
            ),
            (other, _) => bail!("email URL scheme must be smtp or smtps, got {other:?}"),
        };

        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
            .port(url.port().unwrap_or(default_port))
            .tls(tls.clone())
            .timeout(Some(SEND_TIMEOUT));
        if !url.username().is_empty() || url.password().is_some() {
            if matches!(tls, Tls::None) && !is_local(&host) {
                bail!(
                    "email: refusing to send a password without TLS; use smtps://, or smtp:// without tls=none"
                );
            }
            let user = percent_decode(url.username()).context("email URL user name")?;
            let pass =
                percent_decode(url.password().unwrap_or_default()).context("email URL password")?;
            builder = builder.credentials(Credentials::new(user, pass));
        }

        let from = match (from, from_q.as_deref()) {
            (Some(_), Some(_)) => bail!("email from is set both in the URL and as a setting"),
            (Some(f), None) | (None, Some(f)) => f,
            (None, None) => bail!("email needs a from address"),
        };
        let from = mailbox(from).context("email from")?;
        let to = to
            .iter()
            .chain(&to_q)
            .map(|t| mailbox(t).context("email to"))
            .collect::<Result<Vec<_>>>()?;
        if to.is_empty() {
            bail!("email needs at least one to address");
        }

        Ok(Self {
            transport: builder.build(),
            host,
            from,
            to,
        })
    }
}

/// Whether the server is this machine, where a password without TLS never
/// crosses the network.
fn is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `a@x.org` or `Name <a@x.org>`.
fn mailbox(s: &str) -> Result<Mailbox> {
    s.trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid address {:?}: {e}", s.trim()))
}

/// Decode `%XX` escapes from a URL's user name or password.
fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            let Some(b) = hex else {
                bail!("bad percent escape");
            };
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| anyhow::anyhow!("not valid UTF-8"))
}

impl Notifier for Email {
    fn label(&self) -> String {
        // Never the URL: it holds the password.
        let n = self.to.len();
        let s = if n == 1 { "" } else { "s" };
        format!("email (via {}, {n} recipient{s})", self.host)
    }

    fn send<'a>(&'a self, event: &'a Event) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (subject, body) = render(event, now());
            let mut msg = Message::builder()
                .from(self.from.clone())
                .subject(subject)
                .header(ContentType::TEXT_PLAIN);
            for to in &self.to {
                msg = msg.to(to.clone());
            }
            let msg = msg.body(body)?;
            self.transport
                .send(msg)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(())
        })
    }
}

/// Subject and plain-text body.
fn render(event: &Event, now: u64) -> (String, String) {
    match event {
        Event::TicketReady(t) => {
            // Names come from the torrent: no line breaks in the subject.
            let name: String = t
                .name
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let name: String = if name.chars().count() > 150 {
                name.chars().take(149).chain(['…']).collect()
            } else {
                name
            };
            let subject = format!("deaddrop: {name} is ready");
            let mode = if t.single_use {
                "single use: the server shuts down after one download"
            } else {
                "multi use"
            };
            let mut body = format!(
                "{name} is ready to download.\n\n{} file(s), {}, {mode}.\n",
                t.files,
                human(t.total_bytes),
            );
            if t.reset {
                body.push_str("This replaces the previous ticket, which no longer works.\n");
            }
            if let Some(at) = t.expires_at {
                body.push_str(&format!("The ticket expires in {}.\n", time_left(at, now)));
            }
            body.push_str(&format!(
                "\nTo download it, run:\n\n    {}\n",
                t.fetch_command
            ));
            if let Some(k) = &t.key {
                body.push_str(&format!(
                    "\nThe data is encrypted. Set this key first:\n\n    DEADDROP_KEY={k}\n"
                ));
            }
            body.push_str("\n-- \nSent by deaddrop\n");
            (subject, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::TicketReady;

    fn email(url: &str) -> Result<Email> {
        Email::new(url, None, &[])
    }

    #[test]
    fn settings() {
        let to = ["b@y.org".to_string()];
        let e = Email::new("smtps://u:p@smtp.example.com", Some("a@x.org"), &to).unwrap();
        assert_eq!(e.label(), "email (via smtp.example.com, 1 recipient)");

        let e = email("smtp://u%40x.org:p%3Aw@mail.x.org:2525?from=Me%20%3Ca%40x.org%3E&to=b@y.org&to=c@z.org").unwrap();
        assert_eq!(e.to.len(), 2);
        assert_eq!(e.from.email.to_string(), "a@x.org");
        assert_eq!(e.label(), "email (via mail.x.org, 2 recipients)");
        assert!(email("smtp://localhost?tls=none&from=a@x.org&to=b@y.org").is_ok());
        assert!(email("smtp://u:p@127.0.0.1?tls=none&from=a@x.org&to=b@y.org").is_ok());
        assert!(email("smtp://u:p@[::1]:25?tls=none&from=a@x.org&to=b@y.org").is_ok());
        assert!(email("smtp://h?tls=opportunistic&from=a@x.org&to=b@y.org").is_ok());

        // Never a password in the clear over the network.
        assert!(email("smtp://u:p@mail.x.org?tls=none&from=a@x.org&to=b@y.org").is_err());
        assert!(email("https://mail.x.org?from=a@x.org&to=b@y.org").is_err());
        assert!(email("smtps://h?tls=none&from=a@x.org&to=b@y.org").is_err());
        assert!(email("smtp://h?tls=maybe&from=a@x.org&to=b@y.org").is_err());
        assert!(email("smtp://h?from=a@x.org").is_err());
        assert!(email("smtp://h?to=b@y.org").is_err());
        assert!(email("smtp://h?from=nope&to=b@y.org").is_err());
        assert!(email("smtp://h?from=a@x.org&to=b@y.org&cc=c@z.org").is_err());
        assert!(email("smtp://h/path?from=a@x.org&to=b@y.org").is_err());
        assert!(email("smtp://u:%zz@h?from=a@x.org&to=b@y.org").is_err());
        assert!(Email::new("smtp://h?from=a@x.org&to=b@y.org", Some("c@z.org"), &[]).is_err());
    }

    /// A real send through lettre to a minimal SMTP server on localhost.
    #[tokio::test]
    async fn sends_over_smtp() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (r, mut w) = sock.into_split();
            let mut lines = BufReader::new(r).lines();
            let (mut rcpts, mut data, mut in_data) = (Vec::new(), String::new(), false);
            w.write_all(b"220 test ESMTP\r\n").await.unwrap();
            while let Some(line) = lines.next_line().await.unwrap() {
                if in_data {
                    if line == "." {
                        in_data = false;
                        w.write_all(b"250 queued\r\n").await.unwrap();
                    } else {
                        data.push_str(&line);
                        data.push('\n');
                    }
                    continue;
                }
                let cmd = line.to_ascii_uppercase();
                let reply: &[u8] = if cmd.starts_with("EHLO") {
                    b"250 test\r\n"
                } else if cmd.starts_with("RCPT TO:") {
                    rcpts.push(line[8..].to_string());
                    b"250 ok\r\n"
                } else if cmd == "DATA" {
                    in_data = true;
                    b"354 go\r\n"
                } else if cmd == "QUIT" {
                    w.write_all(b"221 bye\r\n").await.unwrap();
                    break;
                } else {
                    b"250 ok\r\n"
                };
                w.write_all(reply).await.unwrap();
            }
            (rcpts, data)
        });

        let to = ["b@y.org".to_string(), "c@z.org".to_string()];
        let e = Email::new(
            &format!("smtp://127.0.0.1:{port}?tls=none"),
            Some("deaddrop <a@x.org>"),
            &to,
        )
        .unwrap();
        let ev = Event::TicketReady(TicketReady {
            name: "ubuntu.iso".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: true,
            reset: false,
            expires_at: None,
            key: None,
        });
        e.send(&ev).await.unwrap();

        let (rcpts, data) = server.await.unwrap();
        assert_eq!(rcpts, ["<b@y.org>", "<c@z.org>"]);
        assert!(data.contains("Subject: deaddrop: ubuntu.iso is ready"));
        assert!(data.contains("From: deaddrop <a@x.org>"));
        assert!(data.contains("deaddrop fetch blobT"));
    }

    #[test]
    fn decode() {
        assert_eq!(percent_decode("a%40b%3Ac").unwrap(), "a@b:c");
        assert_eq!(percent_decode("plain").unwrap(), "plain");
        assert!(percent_decode("a%4").is_err());
    }

    #[test]
    fn message() {
        let ev = Event::TicketReady(TicketReady {
            name: "bad\r\nBcc: x@y.org".into(),
            ticket: "blobT".into(),
            fetch_command: "deaddrop fetch blobT".into(),
            files: 1,
            total_bytes: 2048,
            single_use: true,
            reset: false,
            expires_at: Some(10 + 86_400),
            key: None,
        });
        let (subject, body) = render(&ev, 10);
        assert_eq!(subject, "deaddrop: bad  Bcc: x@y.org is ready");
        assert!(body.contains("    deaddrop fetch blobT\n"));
        assert!(body.contains("expires in 1d."));
        assert!(!body.contains("DEADDROP_KEY"));
        let (_, body) = render(&ev.with_key("AGE-SECRET-KEY-1X"), 10);
        assert!(body.contains("DEADDROP_KEY=AGE-SECRET-KEY-1X"));
    }
}
