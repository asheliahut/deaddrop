//! Command-line interface definition and help text.

use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand, ValueHint};
use clap_complete::Shell;
use iroh_blobs::ticket::BlobTicket;

const ABOUT: &str =
    "Download a torrent on a server without seeding it, then hand the files to a client over iroh.";

const LONG_ABOUT: &str = "\
Download a torrent on a server without seeding it, then hand the files to a client over iroh.

A dead drop in two steps:
  1. On the server, `deaddrop serve <magnet|.torrent>` downloads the torrent in
     leech-only mode (nothing is uploaded to the swarm), stops the torrent
     session, and prints a ticket.
  2. On the client, `deaddrop fetch <ticket>` connects to the server peer to
     peer over iroh (QUIC with NAT traversal and relay fallback), downloads
     the files, and checks every chunk against its BLAKE3 hash.

By default the server is single use: it shuts down once one client has the data.";

const MAIN_AFTER_HELP: &str = "\
Examples:
  deaddrop serve 'magnet:?xt=urn:btih:...'     Download, serve once, then exit
  deaddrop serve ./file.torrent --multi-use    Keep serving until Ctrl-C
  deaddrop fetch <ticket> -o ./downloads       Get the files from a server
  deaddrop config example > ~/.config/deaddrop/config.toml
  source <(deaddrop completions zsh)           Tab completion for this shell

Run `deaddrop help <command>` or `deaddrop <command> --help` for details.";

const MAIN_AFTER_LONG_HELP: &str = "\
Examples:
  deaddrop serve 'magnet:?xt=urn:btih:...'     Download, serve once, then exit
  deaddrop serve ./file.torrent --multi-use    Keep serving until Ctrl-C
  deaddrop fetch <ticket> -o ./downloads       Get the files from a server
  deaddrop config example > ~/.config/deaddrop/config.toml
  source <(deaddrop completions zsh)           Tab completion for this shell

Environment variables:
  DEADDROP_CONFIG   Path to the config file (same as --config)
  DEADDROP_NOTIFY   Comma-separated notifiers for `serve` (same as --notify)
  RUST_LOG          Log filter, e.g. `info` or `deaddrop=debug,librqbit=info`
                    (overrides `log` in the config file; default: warn)

Config file:
  Every setting can be put in a TOML file. Run `deaddrop config example` for
  a commented template and `deaddrop config path` to see where it's read from.
  Precedence: command-line flag > environment variable > config file > default.

Output:
  `serve` prints the ticket alone on stdout and everything else on stderr, so
  TICKET=$(deaddrop serve x.torrent) works in scripts.

Exit status:
  0 on success (including a single-use server shutting down after its
  download, and Ctrl-C), 1 on error, 2 on bad command-line usage.";

const SERVE_LONG_ABOUT: &str = "\
Download a torrent (leech only, no seeding), then serve its contents over iroh.

What happens:
  1. The torrent is downloaded with uploading disabled and no incoming
     BitTorrent listener, so nothing is seeded.
  2. When it finishes, the torrent session is shut down (DHT, trackers, peers).
  3. The files are added to an iroh blob store (by reference, not copied) and
     served as one collection.
  4. A ticket is printed (and sent to any configured notifiers). Give it to
     the client: `deaddrop fetch <ticket>`.

Single use (the default): one client at a time can connect. Once a client has
everything and disconnects, the server exits. If the client drops out partway,
another client can connect. --multi-use keeps serving until Ctrl-C.

Anyone with the ticket can download the data. Treat it as a secret.";

const SERVE_AFTER_HELP: &str = "\
Examples:
  deaddrop serve 'magnet:?xt=urn:btih:...'
  deaddrop serve https://example.com/file.torrent --multi-use
  deaddrop serve ./file.torrent --data-dir /srv/deaddrop
  deaddrop serve ./file.torrent --notify discord=https://discord.com/api/webhooks/<id>/<token>
  TICKET=$(deaddrop serve ./file.torrent)";

const NOTIFY_LONG_HELP: &str = "\
Send the ticket somewhere once it's ready. Repeatable.

Types:
  discord=<URL>   Discord channel webhook. Posts the name, size, mode and a
                  copyable `deaddrop fetch` command.
  webhook=<URL>   POSTs JSON: {\"event\":\"ticket_ready\", \"name\", \"ticket\",
                  \"fetch_command\", \"files\", \"total_bytes\", \"single_use\"}

Webhook URLs are secrets. To keep them out of shell history and `ps`, set
DEADDROP_NOTIFY (comma-separated) or use [[serve.notify]] tables in the config
file instead. Sources aren't merged: flags replace DEADDROP_NOTIFY, which
replaces the config file's list.

A failed notification is logged as a warning and the server keeps serving.";

const FETCH_LONG_ABOUT: &str = "\
Download the data that a deaddrop server is serving.

The client connects to the server named in the ticket, downloads the whole
collection, and checks every chunk against its BLAKE3 hash. The data is first
downloaded into a temporary folder (.deaddrop-<hash>) inside the output folder.
The files are then written out under their original names and the temporary
folder is deleted. File names that are absolute or contain `..` are refused.

When the download finishes the client disconnects, which lets a single-use
server shut down.";

const FETCH_AFTER_HELP: &str = "\
Examples:
  deaddrop fetch blobaa...            Write files into the current directory
  deaddrop fetch blobaa... -o ~/dl    Write files into ~/dl";

const COMPLETIONS_LONG_ABOUT: &str = "\
Print a shell completion script to stdout.

Install it once, then open a new shell:
  bash         deaddrop completions bash > ~/.local/share/bash-completion/completions/deaddrop
  zsh          deaddrop completions zsh > ~/.zfunc/_deaddrop
               (and add `fpath=(~/.zfunc $fpath); autoload -U compinit; compinit`
               to ~/.zshrc before any plugin manager that calls compinit)
  fish         deaddrop completions fish > ~/.config/fish/completions/deaddrop.fish
  powershell   deaddrop completions powershell >> $PROFILE
  elvish       deaddrop completions elvish >> ~/.config/elvish/rc.elv

Or load it for the current session only:
  bash         source <(deaddrop completions bash)
  zsh          source <(deaddrop completions zsh)

Re-run this after upgrading deaddrop so new flags complete too.";

const CONFIG_LONG_ABOUT: &str = "\
Show, create and check the TOML config file.

The config file is read from --config <FILE> or DEADDROP_CONFIG. If neither is
set, deaddrop uses the platform config dir if the file exists
(~/.config/deaddrop/config.toml on Linux). Relative paths in the file are
resolved against the file's directory, and unknown keys are errors.";

#[derive(Debug, Parser)]
#[command(
    name = "deaddrop",
    version,
    about = ABOUT,
    long_about = LONG_ABOUT,
    after_help = MAIN_AFTER_HELP,
    after_long_help = MAIN_AFTER_LONG_HELP,
    propagate_version = true,
)]
pub struct Cli {
    /// TOML config file [default: ~/.config/deaddrop/config.toml, if it exists]
    ///
    /// Settings in the file are used when the matching flag isn't given. Run
    /// `deaddrop config example` for a template.
    #[arg(
        long,
        short = 'c',
        global = true,
        env = "DEADDROP_CONFIG",
        hide_env_values = true,
        value_name = "FILE",
        value_hint = ValueHint::FilePath
    )]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Download a torrent without seeding it, then serve the files over iroh
    #[command(long_about = SERVE_LONG_ABOUT, after_help = SERVE_AFTER_HELP)]
    Serve {
        /// Magnet URI, http(s) URL of a .torrent file, or path to a local .torrent file
        #[arg(value_name = "SOURCE", value_hint = ValueHint::FilePath)]
        source: String,

        /// Keep serving to any number of clients until Ctrl-C [config: serve.multi_use]
        ///
        /// Without this the server is single use and exits after one client has
        /// downloaded everything. `--multi-use=false` forces single use even if
        /// the config file turns multi-use on.
        #[arg(
            long,
            num_args = 0..=1,
            require_equals = true,
            default_missing_value = "true",
            value_name = "BOOL"
        )]
        multi_use: Option<bool>,

        /// Keep torrent data and the blob store here [config: serve.data_dir]
        ///
        /// If you leave this out, deaddrop uses a temporary directory and deletes
        /// it on exit. With a data dir, the data stays after exit.
        #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        data_dir: Option<PathBuf>,

        /// Extra BitTorrent peer to connect to; repeatable [config: serve.peers]
        ///
        /// Useful for private swarms or when DHT and trackers can't find peers.
        #[arg(long = "peer", value_name = "IP:PORT")]
        peers: Vec<SocketAddr>,

        /// Send the ticket to discord=<URL> or webhook=<URL>; repeatable [config: serve.notify]
        #[arg(
            long,
            value_name = "TYPE=URL",
            value_hint = ValueHint::Other,
            env = "DEADDROP_NOTIFY",
            value_delimiter = ',',
            hide_env_values = true,
            long_help = NOTIFY_LONG_HELP
        )]
        notify: Vec<String>,
    },

    /// Download the files a deaddrop server is serving
    #[command(long_about = FETCH_LONG_ABOUT, after_help = FETCH_AFTER_HELP)]
    Fetch {
        /// The ticket printed by `deaddrop serve` (starts with "blob")
        #[arg(value_name = "TICKET")]
        ticket: BlobTicket,

        /// Directory to write the files into [default: .] [config: fetch.out]
        #[arg(short, long, value_name = "DIR", value_hint = ValueHint::DirPath)]
        out: Option<PathBuf>,
    },

    /// Show, create or check the config file
    #[command(long_about = CONFIG_LONG_ABOUT, subcommand_required = true)]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Print a shell completion script (bash, zsh, fish, powershell, elvish)
    #[command(
        long_about = COMPLETIONS_LONG_ABOUT,
        after_help = "Example:\n  deaddrop completions zsh > ~/.zfunc/_deaddrop"
    )]
    Completions {
        /// The shell to generate completions for
        #[arg(value_name = "SHELL")]
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print which config file would be used and whether it exists
    Path,
    /// Print a commented example config with every setting
    #[command(after_help = "Example:\n  deaddrop config example > ~/.config/deaddrop/config.toml")]
    Example,
    /// Check the config file and print the settings it gives (secrets hidden)
    Check,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
