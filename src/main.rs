//! deaddrop: fetch a torrent on a server (without seeding it), then hand the
//! data to a client over iroh.

#[macro_use]
mod ui;
mod cli;
mod client;
mod config;
mod crypt;
mod gate;
mod net;
mod notify;
mod reset;
mod server;
mod skill;
mod torrent;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use crate::{
    cli::{Cli, Command, ConfigAction, SkillAction},
    notify::{NotifierConfig, Notifiers},
};

/// Log filter used when neither RUST_LOG nor the config file sets one:
/// nothing at all in release builds, warnings in debug builds.
const DEFAULT_LOG: &str = if cfg!(debug_assertions) {
    "warn"
} else {
    "off"
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
    if let Command::Skill { action } = cli.command {
        return match action {
            SkillAction::Install { target, force } => skill::install(target.into_target(), force),
            SkillAction::Uninstall { target, force } => {
                skill::uninstall(target.into_target(), force)
            }
            SkillAction::Status { dir } => skill::status(dir),
            SkillAction::Show => skill::show(),
        };
    }

    let (config, _) = config::load(cli.config.as_deref())?;
    let relay = config
        .relay
        .to_mode()
        .context("invalid [relay] settings in the config file")?;

    // RUST_LOG > config `log` > DEFAULT_LOG.
    let filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_) => {
            let level = config.log.as_deref().unwrap_or(DEFAULT_LOG);
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
            reset_token,
            wait_for_reset,
            encrypt,
            hide_key,
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
                source: torrent::Source::from_arg(&source)?,
                single_use: !multi_use.or(file.multi_use).unwrap_or(false),
                data_dir: data_dir.or(file.data_dir),
                peers: if peers.is_empty() { file.peers } else { peers },
                notify,
                reset_token: reset_token.filter(|t| !t.is_empty()).or(file.reset_token),
                wait_for_reset: wait_for_reset.or(file.wait_for_reset).unwrap_or(false),
                encrypt: encrypt.or(file.encrypt).unwrap_or(false),
                hide_key: hide_key.or(file.hide_key).unwrap_or(false),
                relay,
            })
            .await
        }
        Command::Reset {
            ticket,
            source,
            token,
            no_wait,
        } => {
            let token = token.filter(|t| !t.is_empty()).or(config.reset.token).context(
                "no reset token: pass --token, set DEADDROP_RESET_TOKEN, or set reset.token in the config file",
            )?;
            reset::request(
                ticket,
                torrent::Source::from_arg(&source)?,
                token,
                !no_wait,
                &relay,
            )
            .await
        }
        Command::Fetch { ticket, out, key } => {
            let key = key
                .filter(|k| !k.is_empty())
                .map(|k| crypt::Key::parse(&k))
                .transpose()?;
            let out = out.or(config.fetch.out).unwrap_or_else(|| ".".into());
            client::run(ticket, out, key, &relay).await
        }
        Command::Config { .. } | Command::Completions { .. } | Command::Skill { .. } => {
            unreachable!("handled above")
        }
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
            for (key, token) in [
                ("serve.reset_token", &c.serve.reset_token),
                ("reset.token", &c.reset.token),
            ] {
                if let Some(t) = token {
                    reset::check_token(t).with_context(|| format!("invalid {key}"))?;
                }
            }
            // Build the notifiers too, which checks their URLs.
            let notifiers = Notifiers::new(&c.serve.notify)?;
            let relay = c.relay.to_mode().context("invalid [relay] settings")?;
            let or_default =
                |v: Option<String>, d: &str| v.unwrap_or_else(|| format!("{d} (default)"));
            writeln!(o, "{} is valid\n", path.display())?;
            writeln!(o, "log                  {}", or_default(c.log, DEFAULT_LOG))?;
            writeln!(
                o,
                "serve.multi_use      {}",
                or_default(c.serve.multi_use.map(|b| b.to_string()), "false")
            )?;
            writeln!(
                o,
                "serve.data_dir       {}",
                or_default(
                    c.serve.data_dir.map(|d| d.display().to_string()),
                    "temp dir, deleted on exit"
                )
            )?;
            let peers: Vec<String> = c.serve.peers.iter().map(|p| p.to_string()).collect();
            writeln!(
                o,
                "serve.peers          {}",
                if peers.is_empty() {
                    "none".into()
                } else {
                    peers.join(", ")
                }
            )?;
            let labels = notifiers.labels();
            writeln!(
                o,
                "serve.notify         {}",
                if labels.is_empty() {
                    "none".into()
                } else {
                    format!("{} (URLs hidden)", labels.join(", "))
                }
            )?;
            let set = |v: &Option<String>| {
                if v.is_some() {
                    "set (hidden)"
                } else {
                    "not set"
                }
            };
            writeln!(o, "serve.reset_token    {}", set(&c.serve.reset_token))?;
            writeln!(
                o,
                "serve.wait_for_reset {}",
                or_default(c.serve.wait_for_reset.map(|b| b.to_string()), "false")
            )?;
            writeln!(
                o,
                "serve.encrypt        {}",
                or_default(c.serve.encrypt.map(|b| b.to_string()), "false")
            )?;
            writeln!(
                o,
                "serve.hide_key       {}",
                or_default(c.serve.hide_key.map(|b| b.to_string()), "false")
            )?;
            writeln!(
                o,
                "fetch.out            {}",
                or_default(c.fetch.out.map(|d| d.display().to_string()), ".")
            )?;
            let relay = match relay {
                iroh::RelayMode::Custom(_) => format!("custom: {}", c.relay.urls.join(", ")),
                iroh::RelayMode::Disabled => "disabled (direct connections only)".into(),
                _ if c.relay.mode.is_some() => "n0's public relays".into(),
                _ => "n0's public relays (default)".into(),
            };
            writeln!(o, "relay                {relay}")?;
            writeln!(o, "reset.token          {}", set(&c.reset.token))?;
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

/// Show a ticket and, if the data is encrypted, its decryption key:
/// readable on stderr, and as one `<ticket>` or `<ticket> <key>` line on
/// stdout for scripts.
pub fn print_ticket(ticket: &str, key: Option<&str>) -> Result<()> {
    ui::suspend(|| print_ticket_inner(ticket, key))
}

fn print_ticket_inner(ticket: &str, key: Option<&str>) -> Result<()> {
    eprintln!("on the client, run:");
    eprintln!();
    eprintln!("    deaddrop fetch {ticket}");
    eprintln!();
    match key {
        Some(key) => {
            eprintln!("with this decryption key in DEADDROP_KEY (or --key):");
            eprintln!();
            eprintln!("    {key}");
            eprintln!();
            write_stdout(&format!("{ticket} {key}\n"))
        }
        None => write_stdout(&format!("{ticket}\n")),
    }
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
