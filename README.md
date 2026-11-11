# deaddrop

A server downloads a torrent without seeding it, then hands the files to a
client over [iroh](https://iroh.computer) (peer-to-peer QUIC with NAT
traversal and relay fallback).

```
                 BitTorrent (leech only)            iroh-blobs (QUIC, verified)
  swarm  ───────────────────────────────▶  server  ─────────────────────────────▶  client
```

## Usage

On the server:

```sh
deaddrop serve 'magnet:?xt=urn:btih:...'          # single use: exits after one client finishes downloading
deaddrop serve https://example.com/x.torrent      # .torrent URLs and local .torrent paths work too
deaddrop serve file.torrent --multi-use           # keep serving to any number of clients until Ctrl-C
```

When the torrent finishes, the server prints a ticket. On the client:

```sh
deaddrop fetch <ticket> -o ./downloads
```

### `serve` options

| Flag | Meaning |
| --- | --- |
| `--multi-use` | Keep serving to any number of clients until Ctrl-C. Without this flag the server is **single use**: it serves one client, then shuts down once that client has the whole collection and disconnects (or 30 s after the transfer completes). While that client is connected, other clients are rejected. If the client drops out before finishing, the next client can connect. `--multi-use=false` forces single use even if the config file turns multi-use on. |
| `--data-dir <DIR>` | Keep the torrent data and blob store here. If you leave this out, deaddrop uses a temp directory and deletes it on exit. |
| `--peer <IP:PORT>` | Extra BitTorrent peer to connect to. Repeatable. Useful for private swarms. |
| `--notify <TYPE=URL>` | Send the ticket somewhere when it's ready. Repeatable. See [Notifications](#notifications). |
| `-c, --config <FILE>` | TOML config file (works with every command). See [Config file](#config-file). |

`fetch` takes `-o/--out <DIR>` (default: the current directory).

### Shell completions

```sh
deaddrop completions bash > ~/.local/share/bash-completion/completions/deaddrop
deaddrop completions zsh  > ~/.zfunc/_deaddrop        # needs ~/.zfunc in $fpath before compinit
deaddrop completions fish > ~/.config/fish/completions/deaddrop.fish
deaddrop completions powershell >> $PROFILE
source <(deaddrop completions zsh)                   # current session only
```

`deaddrop completions --help` has the full install steps for each shell.
Re-run it after upgrading so new flags complete too.

### Help

`deaddrop --help` (or `deaddrop help <command>`) has the full reference:
examples, environment variables, config precedence and exit codes. `-h` gives
a shorter summary.

The ticket goes to stdout and everything else goes to stderr, so
`TICKET=$(deaddrop serve x.torrent)` works in scripts. Set `RUST_LOG=info` for
more detail.

## Config file

Every setting can live in a TOML file. [`config.example.toml`](config.example.toml)
lists all of them, and the binary can print it too:

```sh
deaddrop config example > ~/.config/deaddrop/config.toml   # write a commented template
deaddrop config path                                       # which file is used, and does it exist?
deaddrop config check                                      # validate it and show the settings (secrets hidden)
```

For example:

```toml
log = "warn"                      # same syntax as RUST_LOG

[serve]
multi_use = false
data_dir = "/var/lib/deaddrop"
peers = ["192.168.1.10:6881"]

[[serve.notify]]
type = "discord"
url = "https://discord.com/api/webhooks/<id>/<token>"

[[serve.notify]]
type = "webhook"
url = "https://example.com/hooks/deaddrop"

[fetch]
out = "/home/me/Downloads"
```

- **Where it's read from:** `--config <file>` or `DEADDROP_CONFIG`. Otherwise
  deaddrop uses the platform config dir if the file exists:
  `~/.config/deaddrop/config.toml` on Linux (respects `$XDG_CONFIG_HOME`),
  `~/Library/Application Support/deaddrop/config.toml` on macOS,
  `%APPDATA%\deaddrop\config\config.toml` on Windows. A missing default file is
  fine. A missing `--config` file is an error.
- **Precedence:** command-line flag > environment variable > config file >
  default. For lists (`peers`, `notify`) the first source that has any entries
  wins; they aren't merged.
- **Relative paths** (`data_dir`, `out`) are resolved against the config
  file's directory.
- **Typos are errors:** unknown keys or notifier types fail at startup with
  the line and column.
- The file holds secrets once it has webhook URLs. On Unix, deaddrop warns if
  other users can read it (`chmod 600` fixes that).
- The torrent source and the fetch ticket are per-run inputs, so they stay on
  the command line.

## Notifications

The server can send the ticket out when it's ready:

```sh
deaddrop serve x.torrent --notify discord=https://discord.com/api/webhooks/<id>/<token>
deaddrop serve x.torrent --notify webhook=https://example.com/hooks/deaddrop
```

Webhook URLs usually contain a secret. To keep them out of shell history and
`ps`, put them in the [config file](#config-file) as `[[serve.notify]]`
tables, or in `DEADDROP_NOTIFY`, comma-separated:

```sh
export DEADDROP_NOTIFY='discord=https://discord.com/api/webhooks/...,webhook=https://example.com/hook'
deaddrop serve x.torrent
```

Sources aren't merged: `--notify` flags replace `DEADDROP_NOTIFY`, which
replaces the config file's list.

| Type | What it sends |
| --- | --- |
| `discord` | A channel message with the torrent name, size, mode and a copyable `deaddrop fetch …` command. Mentions are turned off and markdown in the name is escaped. |
| `webhook` | `POST` with a JSON body: `{"event": "ticket_ready", "name", "ticket", "fetch_command", "files", "total_bytes", "single_use"}` |

Bad notifier settings are rejected at startup, before the torrent download begins. If a
notification fails (an error status, unreachable host, or the 15 s timeout),
the server logs a warning and keeps serving, since the ticket is still printed
locally. Logs never include webhook URLs.

### Adding a notifier

Notifiers live in `src/notify/`. To add one, for example Slack:

1. Create `src/notify/slack.rs` with a struct that implements the `Notifier`
   trait (`label()` and `send(&Event)`).
2. Register it in `src/notify/mod.rs`: add `mod slack;`, add a
   `Slack { ... }` variant to `NotifierConfig` with the settings it needs,
   and add an arm to the `match` in `NotifierConfig::build`.

The variant's fields are what users write in their `[[serve.notify]]` table
(`type = "slack"` plus those fields). If its main setting is called `url`, the
`--notify slack=<url>` shorthand works with no extra code.

New kinds of events (e.g. "client finished downloading") are variants of the
`Event` enum, and each notifier decides how to render them.

## How "no seeding" works

- The torrent session is built with librqbit's `disable-upload` feature, so it
  never sends piece data to peers.
- There is no incoming BitTorrent listener.
- When the download finishes, the torrent is removed from the session and the
  session (DHT, trackers, peer connections) is stopped before iroh starts
  serving.

## Notes

- Files are imported into the iroh blob store by reference, not copied, so
  disk usage isn't doubled.
- The client checks every received chunk against its BLAKE3 hash. It refuses
  file names that are absolute or contain `..`.
- Anyone who has the ticket can download the data. Treat the ticket as a
  secret.

## Testing locally

`examples/seed.rs` seeds a local path on 127.0.0.1, so you can test without
the public swarm:

```sh
cargo run --example seed -- ./some-dir       # prints magnet + peer address, writes ./some-dir.torrent
cargo run -- serve ./some-dir.torrent --peer 127.0.0.1:<port>
cargo run -- fetch <ticket> -o /tmp/out
```
