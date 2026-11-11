//! deaddrop: fetch a torrent on a server (without seeding it), then hand the
//! data to a client over iroh.

mod cli;
mod client;
mod config;
mod notify;
mod server;
mod single_use;
mod torrent;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::{
    cli::{Cli, Command, ConfigAction},
    notify::{NotifierConfig, Notifiers},
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // These run before loading the config file, so `config path` and
    // `completions` still work when the file is broken.
    match &cli.command {
        Command::Config { action } => return config_command(action, cli.config.as_deref()),
        Command::Completions { shell } => return completions(*shell),
        _ => {}
    }

    let (config, _) = config::load(cli.config.as_deref())?;

    // RUST_LOG > config `log` > "warn".
    let filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => {
            let level = config.log.as_deref().unwrap_or("warn");
            EnvFilter::try_new(level).with_context(|| format!("invalid log filter {level:?}"))?
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::Serve {
            source,
            multi_use,
            data_dir,
            peers,
            notify,
        } => {
            let file = config.serve;
            // Flags and DEADDROP_NOTIFY arrive here together (clap handles
            // flag-over-env); fall back to the config file if both are empty.
            let notify = if notify.is_empty() {
                file.notify
            } else {
                parse_notify_flags(&notify)?
            };
            server::run(server::ServeOptions {
                source,
                single_use: !multi_use.or(file.multi_use).unwrap_or(false),
                data_dir: data_dir.or(file.data_dir),
                peers: if peers.is_empty() { file.peers } else { peers },
                notify,
            })
            .await
        }
        Command::Fetch { ticket, out } => {
            let out = out.or(config.fetch.out).unwrap_or_else(|| ".".into());
            client::run(ticket, out).await
        }
        Command::Config { .. } | Command::Completions { .. } => unreachable!("handled above"),
    }
}

fn parse_notify_flags(specs: &[String]) -> Result<Vec<NotifierConfig>> {
    specs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            NotifierConfig::parse_shorthand(s).with_context(|| format!("bad --notify #{}", i + 1))
        })
        .collect()
}

fn config_command(action: &ConfigAction, explicit: Option<&std::path::Path>) -> Result<()> {
    use std::fmt::Write as _;
    let mut o = String::new();
    match action {
        ConfigAction::Example => o.push_str(config::EXAMPLE),
        ConfigAction::Path => match config::locate(explicit) {
            config::Location::Explicit(p) => {
                let state = if p.is_file() { "exists" } else { "MISSING" };
                writeln!(
                    o,
                    "{} ({state}, from --config / DEADDROP_CONFIG)",
                    p.display()
                )?;
            }
            config::Location::Default(p) => {
                let state = if p.is_file() {
                    "exists"
                } else {
                    "not created; defaults are used"
                };
                writeln!(o, "{} ({state})", p.display())?;
            }
            config::Location::None => {
                writeln!(o, "(no config directory found on this platform)")?;
            }
        },
        ConfigAction::Check => {
            let (c, path) = config::load(explicit)?;
            let Some(path) = path else {
                writeln!(o, "no config file; all settings are defaults")?;
                return Ok(());
            };
            if let Some(level) = &c.log {
                EnvFilter::try_new(level)
                    .with_context(|| format!("invalid log filter {level:?}"))?;
            }
            // Build the notifiers too, which checks their URLs.
            let notifiers = Notifiers::new(&c.serve.notify)?;
            let or_default =
                |v: Option<String>, d: &str| v.unwrap_or_else(|| format!("{d} (default)"));
            writeln!(o, "{} is valid\n", path.display())?;
            writeln!(o, "log               {}", or_default(c.log, "warn"))?;
            writeln!(
                o,
                "serve.multi_use   {}",
                or_default(c.serve.multi_use.map(|b| b.to_string()), "false")
            )?;
            writeln!(
                o,
                "serve.data_dir    {}",
                or_default(
                    c.serve.data_dir.map(|d| d.display().to_string()),
                    "temp dir, deleted on exit"
                )
            )?;
            let peers: Vec<String> = c.serve.peers.iter().map(|p| p.to_string()).collect();
            writeln!(
                o,
                "serve.peers       {}",
                if peers.is_empty() {
                    "none".into()
                } else {
                    peers.join(", ")
                }
            )?;
            let labels = notifiers.labels();
            writeln!(
                o,
                "serve.notify      {}",
                if labels.is_empty() {
                    "none".into()
                } else {
                    format!("{} (URLs hidden)", labels.join(", "))
                }
            )?;
            writeln!(
                o,
                "fetch.out         {}",
                or_default(c.fetch.out.map(|d| d.display().to_string()), ".")
            )?;
        }
    }
    write_stdout(&o)
}

fn completions(shell: clap_complete::Shell) -> Result<()> {
    use clap::CommandFactory;
    let mut buf = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "deaddrop", &mut buf);
    write_stdout(&String::from_utf8(buf)?)
}

/// Write to stdout, treating a closed pipe (`deaddrop ... | head`) as success
/// instead of panicking like `println!` does.
pub fn write_stdout(s: &str) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    match out.write_all(s.as_bytes()).and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}
