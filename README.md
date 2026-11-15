<div align="center">

# 📦 deaddrop

**Leech a torrent on a server. Hand it to one client over an encrypted peer-to-peer link. Leave nothing behind.**

🌐 **[deaddrop.rip](https://deaddrop.rip)**

[![CI](https://github.com/asheliahut/deaddrop/actions/workflows/ci.yml/badge.svg)](https://github.com/asheliahut/deaddrop/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/asheliahut/deaddrop?include_prereleases&sort=semver&logo=github)](https://github.com/asheliahut/deaddrop/releases)
[![GitHub stars](https://img.shields.io/github/stars/asheliahut/deaddrop?style=flat&logo=github)](https://github.com/asheliahut/deaddrop/stargazers)
[![Rust 2024](https://img.shields.io/badge/rust-2024_edition-orange?logo=rust)](https://doc.rust-lang.org/edition-guide/rust-2024/)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macOS%20%7C%20windows-blue)](#-installation)
[![License: Unlicense](https://img.shields.io/badge/license-Unlicense-blue)](LICENSE)
[![Website](https://img.shields.io/badge/website-deaddrop.rip-111827)](https://deaddrop.rip)
[![Powered by iroh](https://img.shields.io/badge/powered_by-iroh-7c3aed)](https://iroh.computer)
[![Optional age encryption](https://img.shields.io/badge/optional_encryption-age-0f766e)](https://age-encryption.org)

[Website](https://deaddrop.rip) •
[Quick start](#-quick-start) •
[Installation](#-installation) •
[Usage](#usage) •
[Config](#config-file) •
[Notifications](#notifications) •
[Encryption](#encryption) •
[Agent skill](#agent-skill-for-ai-coding-agents) •
[Contributing](#-contributing)

</div>

---

A server downloads a torrent **without seeding it** (or takes local files,
stdin, or an http(s) download), then hands the files to a client over [iroh](https://iroh.computer) (end-to-end encrypted peer-to-peer
QUIC with NAT traversal and relay fallback). With `--encrypt`, the files are
also [age](https://age-encryption.org)-encrypted at rest.

```mermaid
flowchart LR
    swarm[("🌐 BitTorrent<br/>swarm")]
    server["🖥️ deaddrop serve<br/><sub>optionally age-encrypts</sub>"]
    client["💻 deaddrop fetch<br/><sub>verifies (+ decrypts)</sub>"]
    swarm -- "leech only<br/>(never uploads)" --> server
    server -- "iroh-blobs<br/>encrypted QUIC · BLAKE3-verified" --> client
```

## ✨ Features

- 🚫 **Zero seeding** – built on librqbit's `disable-upload`, no incoming BitTorrent listener, and the swarm session is torn down before serving starts.
- 🔐 **End-to-end encrypted** – iroh connections are QUIC with TLS 1.3, authenticated by the server's key in the ticket, relays included. Add `--encrypt` to also [age](https://age-encryption.org)-encrypt every file (and hide file names) with a fresh per-torrent key that never touches the server's disk.
- 🕳️ **NAT-proof transfers** – iroh punches through NATs and falls back to relays, so neither side needs an open port.
- ✅ **Verified delivery** – every chunk is checked against its BLAKE3 hash; tampering (or a wrong key) fails loudly.
- 💥 **Single use by default** – one client, one download, then the server shuts down (or wipes the data and waits).
- 🎟️ **Only the right client** – `--allow <ID>` turns away everyone but the clients you name, so a leaked ticket is useless.
- ⏳ **Expiring tickets** – `--expire-after 24h` wipes an unclaimed drop instead of leaving it on disk forever.
- 📁 **Not just torrents** – serve local files and directories in place, whatever is piped into stdin, or a file the server downloads over http(s).
- 🔄 **Hot swaps** – point a running server at a new torrent with `deaddrop reset`; the old ticket dies.
- 🔔 **Notifications** – push tickets to Discord, Slack, Mattermost, Rocket.Chat, Telegram, ntfy, email (SMTP), SMS (Twilio) or any webhook, with per-notifier control over who gets the key.
- 🧭 **See the path** – every connection reports whether it goes direct (hole-punched) or through a relay; use n0's relays, your own, or none.
- 📊 **Progress bars** – for the torrent, each transfer on the server, and the client download, with plain lines for logs, pipes and `--no-progress`.
- 🧩 **Script friendly** – the ticket (and key, with `--encrypt`, unless `--hide-key`) on stdout, everything else on stderr, TOML config, shell completions.
- 🤖 **Agent ready** – `deaddrop skill install` teaches Claude Code, Codex, GitHub Copilot and other AI coding agents how to use and configure deaddrop.

## 🚀 Quick start

```sh
# on the server
deaddrop serve 'magnet:?xt=urn:btih:...'
# ...prints:  <ticket>

# on the client
deaddrop fetch <ticket> -o ./downloads
```

That's it. The server exits once the client has everything.

Want the data encrypted at rest too, so the ticket alone isn't enough?

```sh
deaddrop serve --encrypt 'magnet:?xt=urn:btih:...'
# ...prints:  <ticket> AGE-SECRET-KEY-1...

DEADDROP_KEY=AGE-SECRET-KEY-1... deaddrop fetch <ticket> -o ./downloads
```

## 📥 Installation

**Homebrew** (macOS and Linux, Intel + ARM) is the easiest way to install:

```sh
brew install asheliahut/tap/deaddrop
```

That one command adds the [`asheliahut/tap`](https://github.com/asheliahut/homebrew-tap)
tap and installs the prebuilt release binary (the static musl build on Linux).
You also get:

- bash, zsh and fish completions, loaded automatically (no setup from
  [Shell completions](#shell-completions) needed)
- the example config at `$(brew --prefix)/share/deaddrop/config.example.toml`
- no Gatekeeper prompt on macOS. The release binaries aren't notarized, but
  Homebrew doesn't quarantine what it installs.

The tap updates on every stable release:

```sh
brew upgrade deaddrop                                  # update
brew uninstall deaddrop && brew untap asheliahut/tap   # remove
```

**Prebuilt binaries** for Linux (gnu + musl, x86_64 + aarch64), macOS (Intel +
Apple Silicon) and Windows (x86_64 + ARM) are attached to every
[GitHub release](https://github.com/asheliahut/deaddrop/releases), with
`SHA256SUMS`.

```sh
# example: Linux x86_64 (static musl build)
VERSION=v2.1.0
curl -LO "https://github.com/asheliahut/deaddrop/releases/download/$VERSION/deaddrop-$VERSION-x86_64-unknown-linux-musl.tar.gz"
tar -xzf "deaddrop-$VERSION-x86_64-unknown-linux-musl.tar.gz"
sudo install "deaddrop-$VERSION-x86_64-unknown-linux-musl/deaddrop" /usr/local/bin/
```

macOS and Linux tarballs carry signed [build provenance](https://docs.github.com/en/actions/security-for-github-actions/using-artifact-attestations)
proving they were built by this repo's release workflow. Verify one with the
GitHub CLI:

```sh
gh attestation verify "deaddrop-$VERSION-x86_64-unknown-linux-musl.tar.gz" --repo asheliahut/deaddrop
```

On macOS, a binary downloaded through a browser is quarantined and Gatekeeper
will block it. Use Homebrew or `curl` (which don't quarantine), or clear the flag:
`xattr -d com.apple.quarantine ./deaddrop`.

**Upgrading from 1.x:** 2.0 has breaking changes. Most notably, age
encryption is now opt-in (`--encrypt`), and a 2.0 client can't download from
a 1.0 single-use server. See the [changelog](CHANGELOG.md).

**From source** (needs Rust 1.95+):

```sh
cargo install --locked --git https://github.com/asheliahut/deaddrop
```

## Usage

On the server:

```sh
deaddrop serve 'magnet:?xt=urn:btih:...'          # single use: exits after one client finishes downloading
deaddrop serve https://example.com/x.torrent      # .torrent URLs and local .torrent paths work too
deaddrop serve file.torrent --multi-use           # keep serving to any number of clients until Ctrl-C
deaddrop serve file.torrent --wait-for-reset      # one download per torrent, then wait for the next (needs a reset token)
deaddrop serve file.torrent --encrypt             # also age-encrypt the files; prints a decryption key
deaddrop serve ./photos notes.txt                 # local files and directories, served where they are
tar c dir | deaddrop serve - --name dir.tar       # whatever is piped in, as one file
deaddrop serve https://example.com/x.iso          # the server downloads it, then serves it
```

When the data is ready, the server prints a ticket. On the client:

```sh
deaddrop fetch <ticket> -o ./downloads
export DEADDROP_KEY=AGE-SECRET-KEY-1...           # only for --encrypt: the key from `serve`
```

See [Encryption](#encryption) for what is protected and from whom.

### What `serve` can serve

| `SOURCE` | What happens |
| --- | --- |
| `magnet:?...` | Downloaded as a torrent, leech only, then the torrent session is shut down. |
| `x.torrent`, `https://.../x.torrent` | A local `.torrent` file, or an http(s) URL whose path ends in `.torrent`: a torrent too. |
| `FILE` or `DIR` (one or more) | Served where they are. deaddrop never moves, changes or deletes them, also not with `--encrypt` (the encrypted copies go in the data dir) or after a single-use download. A directory keeps its name as the top folder. Symlinks inside directories, special files and empty folders are skipped. Don't change the files while they're served: the client checks every chunk and would reject the download. They can't be inside the data dir. |
| `-` | Reads stdin into one file, named by `--name` (default `stdin`). |
| any other `https://...` / `http://...` | The server downloads it and serves it as one file, named by `--name`, the `Content-Disposition` file name, or the URL's last part. If the response is a `.torrent` file (by `Content-Type`, or by its contents), that torrent is downloaded instead, so tracker links like `download.php?id=…` keep working. |

`--as-is` turns the torrent detection off: a local `.torrent` file or a URL is
served as a plain file. Resets only ever take torrents (see
[Resetting a server](#resetting-a-server)).

### `serve` options

| Flag | Meaning |
| --- | --- |
| `--multi-use` | Keep serving to any number of clients until Ctrl-C. Without this flag the server is **single use**: it serves one client, then deletes the data and shuts down (or, with `--wait-for-reset`, waits) once that client has the whole collection and disconnects (or 30 s after the transfer completes). While that client is connected, other clients are turned away with a reason (see [When the server turns a client away](#when-the-server-turns-a-client-away)). If the client drops out before finishing, the next client can connect. `--multi-use=false` forces single use even if the config file turns multi-use on. |
| `--name <NAME>` | File name for stdin (`-`) or an http(s) download. |
| `--as-is` | Serve a `.torrent` file or URL as a plain file instead of downloading the torrent. |
| `--data-dir <DIR>` | Keep downloaded data and the blob store here. If you leave this out, deaddrop uses a temp directory and deletes it on exit. A single-use server deletes the data once its one download is done, even from a data dir; a multi-use server's data stays until a reset replaces it. |
| `--peer <IP:PORT>` | Extra BitTorrent peer to connect to. Repeatable. Useful for private swarms. |
| `--notify <TYPE=URL>` | Send the ticket somewhere when it's ready. Repeatable. See [Notifications](#notifications). |
| `--reset-token <TOKEN>` | Let `deaddrop reset` switch the server to a different torrent. See [Resetting a server](#resetting-a-server). Prefer `DEADDROP_RESET_TOKEN` or the config file. |
| `--wait-for-reset` | Single use, needs a reset token: after the one download, delete the data and keep running until a reset brings the next torrent. See [Single use, long running](#single-use-long-running). |
| `--encrypt` | Also age-encrypt the files before serving them, delete the plaintext, and print a decryption key. Off by default, since iroh already encrypts the connection. See [Encryption](#encryption). |
| `--hide-key` | Only with `--encrypt`: never print the decryption key; send it only to notifiers with `include_key` (at least one is required). See [Encryption](#encryption). |
| `--allow <ID>` | Only let this client download or list. Repeatable, or comma-separated in `DEADDROP_ALLOW`. See [Allowing only specific clients](#allowing-only-specific-clients). |
| `--expire-after <DURATION>` | Stop serving a ticket this long after it's printed (`30m`, `24h`, `7d`, `1h30m`), then delete the data. See [Expiring tickets](#expiring-tickets). |
| `-c, --config <FILE>` | TOML config file (works with every command). See [Config file](#config-file). |
| `--no-progress` | Plain progress lines instead of progress bars (works with every command). See [Progress](#progress). |

`fetch` takes `-o/--out <DIR>` (default: the current directory) and, for
data served with `--encrypt`, the key in `DEADDROP_KEY` or `--key`.

If the connection drops mid-download, `fetch` reconnects by itself and only
downloads what is still missing. It keeps trying for `--retry-for <SECS>`
(default 120, `0` to give up at once) without progress. If it gives up or you
stop it, the partial download stays in `.deaddrop-<hash>` in the output
folder, and running the same command again resumes from there. A single-use
server lets a client continue as long as its download hasn't finished. After
a hard disconnect (the client crashed or lost its network), the server can
take up to about 30 s to notice and free its slot; until then reconnects are
refused and `fetch` just keeps retrying.

### When the server turns a client away

A server says why it won't serve a client, and `fetch` (and `fetch --list`)
explains it instead of a bare "permission" error:

| Reason | What `fetch` does |
| --- | --- |
| Another client is downloading from this single-use server | Says so once, then waits quietly for up to `--retry-for`, in case that download fails and the slot frees up. `--list` stops at once. If the server then disappears, `fetch` says the other download most likely finished. |
| The single-use drop has already been downloaded | Stops at once. |
| The ticket expired ([`--expire-after`](#expiring-tickets)) | Stops at once. |
| The ticket was replaced by a reset | Stops at once; ask for the new ticket. |
| The server isn't serving that ticket | Stops at once. |
| Not on the server's allow list ([`--allow`](#allowing-only-specific-clients)) | Stops at once, and says which id to allow. |

The server logs each one (`turned away <id>: <reason>`). Whoever connects
first gets a single-use server's slot; everyone else is turned away while it
is taken, and no data or file list reaches them. When nothing was downloaded,
`fetch` leaves no `.deaddrop-<hash>` folder behind, and when the download can
never be finished (already downloaded, expired, replaced) it deletes the
partial one. Servers older than 2.1 only say "permission".

To see what's in a drop before downloading it, use `fetch --list` (`-l`):

```sh
$ deaddrop fetch --list <ticket>
    6.00 B  data/sub/b.txt
292.97 KiB  data/a.bin
2 files, 292.97 KiB
```

It fetches only the file names and sizes and prints one file per line on
stdout, so it doesn't use up a single-use server's download. That needs a
2.1 or newer server: an older one counts a listing as the download. With
`--encrypt` data the names are encrypted too, so pass the key to see them;
without it you get just the count and sizes.

### Resetting a server

A server started with a reset token can be told to serve a different
torrent, without restarting it:

```sh
# server
export DEADDROP_RESET_TOKEN=$(openssl rand -hex 32)
deaddrop serve first.torrent --multi-use

# anywhere that has the ticket and the token
export DEADDROP_RESET_TOKEN=<same token>
deaddrop reset <ticket> 'magnet:?xt=urn:btih:...'   # or an http(s) .torrent URL, or a local .torrent file
```

- The source can be a magnet URI, an http(s) URL (the server fetches it), or
  a local `.torrent` file (its contents are sent to the server). The server
  never treats a reset source as a path on its own disk, and a reset only
  ever brings in a torrent, even on a server that started with local files,
  stdin or an http(s) download: a reset token holder can't make the server
  read its files or fetch arbitrary URLs for them.
- The server keeps serving the current torrent while the new one downloads.
  If the download fails, nothing changes and `reset` reports the error.
- Once the new torrent is ready, the server prints the new ticket (and, with
  `--encrypt`, its new decryption key) as another line on stdout, sends them to the notifiers with
  `"reset": true`, and `deaddrop reset` prints them too. `--no-wait` returns as soon as the server
  accepts the request.
- **The old ticket stops working.** Transfers already in progress finish, then
  the old torrent's files are deleted, also from a `--data-dir`.
- One reset at a time; a second request is refused while one is downloading.
- Resets use their own iroh protocol (`deaddrop/reset/0`) on the same
  endpoint, which is only enabled when a token is set. The token must be at
  least 16 characters. A wrong token is refused after a short delay.
- Any ticket the server handed out works for `reset`, even one that no
  longer downloads anything: only its endpoint address is used.
- The `reset` command reads its token from `--token`, `DEADDROP_RESET_TOKEN`,
  or `[reset] token` in the config file.
- A single-use server allows one download of each torrent it switches to.
  Without `--wait-for-reset` it still exits after that download.

### Allowing only specific clients

Normally anyone who has the ticket can download. To let in only the people
you mean, each of them runs `deaddrop id` once and sends you the id it
prints:

```sh
# on the client: creates an identity the first time, prints its id
$ deaddrop id
aedfde36826e82dc8a3d4900dc5960d785374b4867b37c5e05e7b498b3b8414d

# on the server
deaddrop serve x.torrent --allow aedfde36826e82dc8a3d4900dc5960d785374b4867b37c5e05e7b498b3b8414d
```

The server checks the id right after the encrypted handshake, which proves
it, and turns everyone else away before they can list or download anything.
`fetch` then stops at once with a message saying it isn't on the allow
list, instead of retrying. Resets aren't affected; they have their own token.

- The id isn't secret. The identity file is: it holds the key that proves the
  id (`~/.local/share/deaddrop/identity.key` on Linux, mode 600). Anyone who
  has a copy can pass as that client.
- Once the file exists, every `fetch` uses it, so every server you fetch from
  sees the same id. Delete the file to go back to a new random id per fetch,
  or keep it elsewhere and point at it only when needed with
  `--identity <FILE>` (or `DEADDROP_IDENTITY`). `deaddrop id --new` replaces it.
- Several `--allow` flags (or `DEADDROP_ALLOW=id1,id2`, or `allow = [...]`
  under `[serve]`) allow several clients.

### Expiring tickets

A single-use drop that nobody picks up stays on the server's disk until
someone notices. `--expire-after <DURATION>` puts a deadline on each ticket:

```sh
deaddrop serve x.torrent --expire-after 24h
```

The time counts from when the ticket is printed, not while the torrent
downloads. When it runs out, the ticket stops working: no new download or
listing starts. A download that is already running may finish (Ctrl-C cuts
it off). Then the data is deleted and the server exits. With
`--wait-for-reset`, it deletes the data and waits for the next reset instead,
and every new ticket gets the full time again. Durations are like `90s`,
`30m`, `24h`, `7d` or `1h30m`. A bare number is seconds, and `0` or `never`
turns a config file's `expire_after` off.

Notifiers are told when the ticket expires: Discord shows it in each
reader's own time ("expires in 23 hours"), and webhook JSON gets
`expires_at` (Unix seconds).

### Single use, long running

`--wait-for-reset` turns a single-use server into a long-running dead drop
where every torrent can be downloaded exactly once:

```sh
export DEADDROP_RESET_TOKEN=$(openssl rand -hex 32)
deaddrop serve first.torrent --wait-for-reset

deaddrop fetch <ticket>                       # the one download
deaddrop reset <ticket> 'magnet:?xt=...'      # any ticket from this server works
deaddrop fetch <new ticket>
```

1. The server serves the torrent to one client, as in normal single use.
2. When that client has everything and disconnects (or 30 s after it has
   everything), the ticket stops working and the torrent's files are deleted.
   The blob store drops its copy within about 10 s. The server keeps running
   but serves nothing.
3. A reset downloads the next torrent and serves it once more, with a new
   ticket. A reset can also replace a torrent nobody has downloaded yet.

Nothing is deleted on Ctrl-C beyond what normal single use does: with
`--data-dir`, a torrent that wasn't downloaded yet stays on disk.

### Agent skill for AI coding agents

`deaddrop skill` installs an [Agent Skill](https://agentskills.io) that
teaches AI coding agents how to use deaddrop: serving, fetching, resets, and
setting up a config file step by step. The skill matches the installed
version, and includes the full config reference.

```sh
deaddrop skill install                                  # asks where, and for which agents
deaddrop skill install --global --harness claude,codex  # for your user, in every project
deaddrop skill install --project --all                  # only this repository; commit it to share
deaddrop skill status                                   # where it's installed, and if it's current
deaddrop skill uninstall --global --all
deaddrop skill show                                     # print SKILL.md
```

| `--harness` | Global | Project |
| --- | --- | --- |
| `claude` (Claude Code) | `~/.claude/skills/` | `.claude/skills/` |
| `codex` (OpenAI Codex) | `~/.agents/skills/` | `.agents/skills/` |
| `copilot` (GitHub Copilot) | `~/.copilot/skills/` | `.github/skills/` |
| `agents` (shared Agent Skills location, also read by the Copilot CLI and others) | `~/.agents/skills/` | `.agents/skills/` |

The project is the enclosing git repository, or the current directory
(`--dir` picks another). Run `install` again after upgrading deaddrop to
update the skill. deaddrop only replaces or removes a `deaddrop` skill that
it wrote, unless you pass `--force`, and `uninstall` keeps any other files
you added to the skill's folder.

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

Installed with Homebrew? bash, zsh and fish completions are already set up and
stay current with `brew upgrade`, as long as your shell loads Homebrew's
completions ([Homebrew docs](https://docs.brew.sh/Shell-Completion)).

### Help

`deaddrop --help` (or `deaddrop help <command>`) has the full reference:
examples, environment variables, config precedence and exit codes. `-h` gives
a shorter summary.

Each ticket goes to stdout as one line, `<ticket>` or, with `--encrypt`,
`<ticket> <key>`. Everything else goes to stderr. So
`deaddrop serve x.torrent > tickets.txt` and then
`read -r TICKET KEY < tickets.txt` works in scripts (`KEY` is empty without
`--encrypt`, or with `--hide-key`). After a [reset](#resetting-a-server), the new ticket is printed
as another line.

### Logging

Release builds (`cargo build --release`) log nothing by default. Debug builds
log warnings. Set `RUST_LOG` (e.g. `RUST_LOG=info`) or `log` in the
[config file](#config-file) to change that, or `off` to turn logging off in
any build. This only covers the log output from deaddrop and its libraries.
The status lines (progress, the ticket, reset messages) are always printed to
stderr. Log lines are only colored on a terminal; `NO_COLOR` or `TERM=dumb`
turn colors off there too, so log files never get escape codes.

### Progress

On a terminal, deaddrop shows progress bars on stderr:

- `serve`: one for the torrent download, then one per client transfer
  (several at once with `--multi-use`), each named by the client's short id.
- `fetch`: one for the download, with the total size and an ETA. The client
  first asks the server for the collection's size with a small probe request.
  This needs a 2.0 server: a 1.0 single-use server would count the probe as
  the download and refuse the real one.

When stderr isn't a terminal (a log file, systemd, a pipe), there are no bars,
just a plain progress line every few seconds and a summary at the end, with no
escape codes. That's also what you get with:

- `--no-progress` (any command), or `DEADDROP_NO_PROGRESS=true`, or
  `progress = false` in the config file. Use it when a terminal session is
  recorded anyway: `script`, tmux or screen logging, a CI runner that gives
  jobs a pseudo-terminal. `--no-progress=false` turns bars back on.
- `TERM=dumb`, which turns off colors too.

`NO_COLOR` (or `CLICOLOR=0`) turns off colors in progress bars and log lines;
`CLICOLOR_FORCE=1` forces them on.

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
log = "off"                       # same syntax as RUST_LOG; "off" = no logs (the release default)

[serve]
multi_use = false
data_dir = "/var/lib/deaddrop"
peers = ["192.168.1.10:6881"]
# reset_token = "<long random string>"     # see "Resetting a server"
# wait_for_reset = false                    # single use: stay up for resets
# encrypt = false                           # true: also age-encrypt the files
# hide_key = false                          # with encrypt: key goes only to include_key notifiers
# allow = ["<id from `deaddrop id`>"]       # only these clients may download
# expire_after = "24h"                      # each ticket stops working after this long

# Notifiers get the ticket only; with encrypt, include_key = true also sends the decryption key.
[[serve.notify]]
type = "discord"
url = "https://discord.com/api/webhooks/<id>/<token>"

[[serve.notify]]
type = "webhook"
url = "https://example.com/hooks/deaddrop"
include_key = true                # this one also gets the key (default: false)

[fetch]
out = "/home/me/Downloads"
# retry_for = 120                 # seconds to keep reconnecting without progress
# identity = "~/.local/share/deaddrop/identity.key"   # default; made by `deaddrop id`

[reset]
# token = "<long random string>"           # used by `deaddrop reset`

[relay]
# mode = "default"                          # "default" (n0), "custom", or "disabled"
# urls = ["https://relay.example.com"]      # your own relays; implies mode = "custom"
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
- **Relative paths** (`data_dir`, `out`, `identity`) are resolved against the config
  file's directory.
- **Typos are errors:** unknown keys or notifier types fail at startup with
  the line and column.
- The file holds secrets once it has webhook URLs or reset tokens. On Unix, deaddrop warns if
  other users can read it (`chmod 600` fixes that).
- The torrent source and the fetch ticket are per-run inputs, so they stay on
  the command line.

## Notifications

The server can send the ticket out when it's ready:

```sh
deaddrop serve x.torrent --notify discord=https://discord.com/api/webhooks/<id>/<token>
deaddrop serve x.torrent --notify 'email=smtps://user:pass@smtp.example.com?from=me@example.com&to=you@example.com'
deaddrop serve x.torrent --notify slack=https://hooks.slack.com/services/<T…>/<B…>/<secret>
deaddrop serve x.torrent --notify mattermost=https://mattermost.example.com/hooks/<key>
deaddrop serve x.torrent --notify rocketchat=https://chat.example.com/hooks/<id>/<token>
deaddrop serve x.torrent --notify webhook=https://example.com/hooks/deaddrop
deaddrop serve x.torrent --notify ntfy=https://ntfy.sh/<long-random-topic>
deaddrop serve x.torrent --notify telegram=<bot_token>@<chat_id>
deaddrop serve x.torrent --notify twilio=<account_sid>:<auth_token>@<from>/<to>
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

With `--encrypt`, a notifier gets the ticket but **not the decryption key** by
default. Choose per notifier which ones get the key: add `+key` to the type on the command
line (`webhook+key=https://...`) or set `include_key = true` in its
`[[serve.notify]]` table. For example, post the ticket to a Discord channel
and send the key to a private webhook, so a leaked channel alone isn't
enough to read the data. Without `--encrypt` there is no key, and the ticket
alone gives access, so only send it where you'd send the data.

| Type | What it sends |
| --- | --- |
| `discord` | A channel message with the torrent name, size, mode and a copyable `deaddrop fetch …` command. Mentions are turned off and markdown in the name is escaped. With the key, it is shown as a spoiler, to be set as `DEADDROP_KEY`. |
| `email` | A plain-text email over SMTP with the name, size, mode, expiry and the `deaddrop fetch …` command. The URL is the server: `smtps://` uses TLS from the start (port 465), and `smtp://` requires STARTTLS (port 587). Add `?tls=opportunistic` to allow a server without STARTTLS, or `?tls=none` for a local relay; deaddrop refuses to send a password without TLS unless the server is on localhost. Put the sender and recipients in the query string (`?from=me@example.com&to=a@example.com&to=b@example.com`), or as `from` and `to` (one address or a list) in its `[[serve.notify]]` table. Addresses can be `Name <a@example.com>`. Percent-encode `@`, `:` and `/` in the user name or password. With the key, it is added as `DEADDROP_KEY=…`, but email isn't end-to-end encrypted, so prefer another notifier for the key. |
| `slack` | A message to a Slack [incoming webhook](https://api.slack.com/messaging/webhooks) with the name, size, mode and the `deaddrop fetch …` command in a code block. The expiry is shown in each reader's time zone. Link and mention markup in the name is escaped, and link previews are off. With the key, it is added as inline code (Slack has no spoilers). |
| `mattermost` | A message to a Mattermost [incoming webhook](https://developers.mattermost.com/integrate/webhooks/incoming/) with the name, size, mode, expiry and the `deaddrop fetch …` command in a code block. Markdown in the name is escaped, and `@` mentions in it (like `@channel`) are broken up so they don't ping anyone. With the key, it is added as inline code. |
| `rocketchat` | A message to a Rocket.Chat [incoming webhook](https://docs.rocket.chat/docs/integrations) with the name as an attachment title (plain text, so it can't contain markup or mentions) and the size, mode, expiry and `deaddrop fetch …` command below it. `rocket.chat` works as the type too. With the key, it is added as inline code. |
| `ntfy` | A push notification to an [ntfy](https://ntfy.sh) topic (ntfy.sh or self-hosted): the name as the title, then the size, mode, expiry and the `deaddrop fetch …` command. The URL is the topic URL. For a protected topic, put `user:pass@` in the URL or set `token = "tk_…"` in its `[[serve.notify]]` table; `priority = 1`–`5` is optional. With the key, it is added to the message. On a public server anyone who knows the topic can read it, so use a long random topic name, and send the key only to a protected topic or your own server. |
| `telegram` | A message from your Telegram bot with the name, size, mode, expiry and the `deaddrop fetch …` command in a copyable block. The shorthand is `telegram=<bot_token>@<chat_id>`: the token from [@BotFather](https://t.me/BotFather), and the chat id as a number (`-1001234567890`) or `@channelname`. In a `[[serve.notify]]` table these are `bot_token` and `chat_id`, plus optional `thread_id` (a forum topic) and `api_url` (a self-hosted Bot API server). With the key, it is shown as a spoiler. |
| `twilio` | An SMS through [Twilio](https://www.twilio.com/docs/messaging) with the name, size, mode, expiry and the `deaddrop fetch …` command, in plain text so it fits in as few SMS segments as possible. The shorthand is `twilio=<account_sid>:<auth_token>@<from>/<to>`, where `from` is a Twilio number, an alphanumeric sender id (in countries that allow them) or a Messaging Service SID (`MG…`), and `to` is an E.164 number like `+15552223333`. Use one notifier per recipient. In a `[[serve.notify]]` table these are `account_sid`, `auth_token`, `from` and `to`, plus optional `api_key` (an `SK…` API key, with its secret as `auth_token`) and `api_url`. With the key, it is added as `DEADDROP_KEY=…`, but SMS isn't end-to-end encrypted, so prefer another notifier for the key. |
| `webhook` | `POST` with a JSON body: `{"event": "ticket_ready", "name", "ticket", "fetch_command", "files", "total_bytes", "single_use", "reset"}`, plus `"key"` for notifiers with the key. `reset` is `true` when the ticket replaces an old one after a reset. With `--expire-after`, `expires_at` is when the ticket stops working (Unix seconds). |

Bad notifier settings are rejected at startup, before the torrent download begins. If a
notification fails (an error status, unreachable host, or the 15 s timeout),
the server logs a warning and keeps serving, since the ticket is still printed
locally. Logs never include webhook URLs.

### Adding a notifier

Notifiers live in `src/notify/`. To add one, for example Matrix:

1. Create `src/notify/matrix.rs` with a struct that implements the `Notifier`
   trait (`label()` and `send(&Event)`). `slack.rs` is a short one to copy.
2. Register it in `src/notify/mod.rs`: add `mod matrix;`, add a
   `Matrix { ... }` variant to `NotifierConfig` with the settings it needs,
   and add an arm to the `match` in `NotifierConfig::build`.

The variant's fields are what users write in their `[[serve.notify]]` table
(`type = "matrix"` plus those fields). If its main setting is called `url`, the
`--notify matrix=<url>` shorthand works with no extra code. Otherwise, add a
case to `NotifierConfig::parse_shorthand`, as Telegram and Twilio do.

New kinds of events (e.g. "client finished downloading") are variants of the
`Event` enum, and each notifier decides how to render them.

## How "no seeding" works

- The torrent session is built with librqbit's `disable-upload` feature, so it
  never sends piece data to peers.
- There is no incoming BitTorrent listener.
- When the download finishes, the torrent is removed from the session and the
  session (DHT, trackers, peer connections) is stopped before iroh starts
  serving.

## Encryption

**In transit, always.** iroh connections are QUIC with TLS 1.3, end to end
between client and server. The ticket holds the server's public key, so the
client knows it's talking to the right server, and relays only ever forward
encrypted packets. Every chunk is also checked against its BLAKE3 hash.

**At rest, with `--encrypt`.** Without it, the downloaded files sit on the
server's disk as they are while they're served (deleted afterwards, as
described above), their names are in the collection, and **anyone with the
ticket can download them**. `--encrypt` (or `encrypt = true` in the config
file) adds a layer of [age](https://age-encryption.org) encryption, so the
data doesn't depend on keeping the ticket secret or on the server's disk:

1. When the torrent finishes, each file is encrypted with
   [age](https://age-encryption.org) to a new X25519 key made for that
   torrent. Each plaintext file is deleted as soon as it is encrypted (so
   you need about one extra file's worth of disk space), and the whole
   download folder is deleted afterwards, also if encryption fails.
2. Only the encrypted files go into the blob store and over iroh. Collection
   entries are named `0`, `1`, ...; the real file names and the torrent name
   are in an encrypted manifest (`deaddrop-manifest.age`).
3. The key lives only in the server's memory. It is printed (stderr and
   stdout) and sent to the notifiers that ask for it, never written to the
   server's disk. Each reset gets a new key. With `--hide-key` (or
   `hide_key = true`) it is not printed at all, and not sent back to
   `deaddrop reset` either: the notifiers with `include_key` get the only
   copies. The server refuses to start without one, and says so loudly if
   none of them accepted the key.
4. `fetch` downloads the ciphertext, then decrypts each file into place. A
   wrong key or tampered data fails (age authenticates every chunk), and no
   partial file is left under the real name.

What `--encrypt` doesn't hide: file sizes and file count, and that a transfer
happened. The plaintext does exist on the server's disk while the torrent
downloads, and deleting a file doesn't securely erase it (especially on
SSDs). If the server's stderr goes to a persistent log (systemd journal, a
log file), the key ends up there too, unless you use `--hide-key`.

## Connection paths and relays

iroh first reaches the other side through a relay server and any addresses
in the ticket, then tries to hole-punch a direct UDP path and moves the
traffic there if that works. Both `serve` and `fetch` print which path a
connection is using, and print again when it switches between the relay and
a direct path:

```text
connection to e86045b272 using the relay https://use1-1.relay.n0.iroh.link./
connection to e86045b272 switched to a direct path (203.0.113.7:50687)
```

If the relay line is the last one you see, no direct path could be made
(for example both sides are behind symmetric NATs, or UDP is blocked). The
data is still end-to-end encrypted, but n0's public relays are rate limited,
so large transfers will be slow.

The `[relay]` config section picks the relays:

- `mode = "default"` uses n0's public relays.
- `urls = [...]` (or `mode = "custom"`) uses only your relays, for example
  your own [`iroh-relay`](https://crates.io/crates/iroh-relay) server.
- `mode = "disabled"` turns relays off. Then a transfer only works if the
  two sides can reach each other directly or by hole punching.

The ticket carries the server's relay URL and direct addresses. If you use
your own relays, give the server and the client the same `urls`. With
relays disabled on the server, the ticket has only direct addresses.

## Notes

- Files (encrypted or not) are imported into the iroh blob store by
  reference, not copied, so disk usage isn't doubled.
- The client checks every received chunk against its BLAKE3 hash. It refuses
  file names that are absolute or contain `..`.
- Anyone who has the ticket can download the data. With `--encrypt` the key
  is also needed to read it. Treat both as secrets, and send them over
  different channels when you can.
- If `fetch` of encrypted data is run without the key, the download is kept
  in `.deaddrop-<hash>` in the output folder, so running it again with the
  key works even after a single-use server has shut down. An interrupted
  download is kept there too, and running `fetch` again resumes it.

## Testing locally

`examples/seed.rs` seeds a local path on 127.0.0.1, so you can test without
the public swarm:

```sh
cargo run --example seed -- ./some-dir       # prints magnet + peer address, writes ./some-dir.torrent
cargo run -- serve ./some-dir.torrent --peer 127.0.0.1:<port>
cargo run -- fetch <ticket> -o /tmp/out
```

## 🤝 Contributing

Issues and pull requests are welcome! Before opening a PR, make sure CI will be happy:

```sh
cargo fmt --all
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Want a new notifier? See [Adding a notifier](#adding-a-notifier). It's about
two files of work.

### Contributors

<a href="https://github.com/asheliahut/deaddrop/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=asheliahut/deaddrop" alt="Contributors" />
</a>

## 🙏 Built on

- [iroh](https://iroh.computer) and [iroh-blobs](https://github.com/n0-computer/iroh-blobs): P2P QUIC transport and verified blob transfer
- [librqbit](https://github.com/ikatson/rqbit): the BitTorrent engine
- [age](https://github.com/str4d/rage): optional file encryption
- [indicatif](https://github.com/console-rs/indicatif): progress bars
- [clap](https://github.com/clap-rs/clap), [tokio](https://tokio.rs), and the rest of the Rust ecosystem

## 📜 License

deaddrop is free and unencumbered software released into the public domain.
See [LICENSE](LICENSE) or <https://unlicense.org>.

## ⭐ Star history

<a href="https://star-history.com/#asheliahut/deaddrop&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=asheliahut/deaddrop&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=asheliahut/deaddrop&type=Date" />
    <img alt="Star history chart" src="https://api.star-history.com/svg?repos=asheliahut/deaddrop&type=Date" />
  </picture>
</a>

---

<div align="center">

If deaddrop is useful to you, consider giving it a ⭐. It helps other people find it.

<sub>Use it responsibly: only download and share content you have the right to.</sub>

</div>
