---
name: deaddrop
description: Use the deaddrop CLI to hand files from a server to another machine peer to peer. It downloads a torrent on a server without seeding it, serves the files over iroh (end-to-end encrypted QUIC), and fetches them on a client with a ticket. Use when the user mentions deaddrop, has a deaddrop ticket (a long string starting with "blob"), wants to get a torrent's files onto another machine without seeding, or wants to set up, reset or configure a deaddrop server (notifiers, age encryption, relays).
metadata:
  generated-by: deaddrop
  version: "2.0.0"
---

# deaddrop

deaddrop moves a torrent's files from a server to one client:

1. On the server, `deaddrop serve <source>` downloads the torrent in
   leech-only mode (nothing is uploaded to the swarm), shuts the torrent
   session down, and prints a **ticket**.
2. On the client, `deaddrop fetch <ticket>` connects to the server peer to
   peer over iroh, downloads the files and checks every chunk against its
   BLAKE3 hash.

`<source>` is a magnet URI, an http(s) URL of a `.torrent` file, or a path to
a local `.torrent` file. Run `deaddrop --version` first: this skill is for
deaddrop 2.0.0. Run `deaddrop <command> --help` for the full details of
any command.

Only transfer content the user has the right to download and share.

## Secrets: handle with care

These all grant access. Never commit them, paste them into issues or logs, or
repeat them back unless the user asks:

- **Tickets** (`blob...`): anyone with one can download the data.
- **Decryption keys** (`AGE-SECRET-KEY-1...`): needed to read data served
  with `--encrypt`.
- **Reset tokens**: let anyone with a ticket switch the server's torrent.
- **Webhook URLs** (Discord or other): anyone with one can post there.

Pass secrets through environment variables instead of flags, so they stay out
of shell history and `ps`: `DEADDROP_KEY`, `DEADDROP_RESET_TOKEN`,
`DEADDROP_NOTIFY`. Config files that hold secrets should be `chmod 600`.

## Serving

```sh
deaddrop serve 'magnet:?xt=urn:btih:...' > ticket.txt
```

- **It's long running.** Start it in the background and read the ticket from
  stdout. Don't wait for it to exit: it runs until a client has finished
  downloading.
- **stdout holds one line per ticket:** `<ticket>`, or `<ticket> <key>` with
  `--encrypt`. Everything else (status, progress, errors) goes to stderr.
  In scripts: `read -r TICKET KEY < ticket.txt`.
- **Single use is the default:** the server exits after one client has
  downloaded everything. `--multi-use` keeps it serving until Ctrl-C.
- `--data-dir <DIR>` keeps the data; without it a temporary dir is used and
  deleted on exit.
- `--peer <IP:PORT>` (repeatable) adds BitTorrent peers when DHT and trackers
  can't find any.

### Encryption

iroh always encrypts the connection. `--encrypt` adds age encryption at
rest: each file is encrypted with a fresh key, the plaintext is deleted, file
names are hidden, and a decryption key is printed. The client then needs the
ticket **and** the key.

`--hide-key` (with `--encrypt`) never prints the key at all. It goes only to
notifiers with `+key` / `include_key = true`, and the server refuses to start
without one. Don't try to recover a hidden key; ask the user to get it from
their notifier.

### Notifiers

Send the ticket somewhere when it's ready:

```sh
DEADDROP_NOTIFY='discord=https://discord.com/api/webhooks/<id>/<token>' deaddrop serve ...
```

The types are `discord=<URL>` and `webhook=<URL>` (JSON POST). Add `+key`
(`webhook+key=<URL>`) to also send the decryption key. The list is
comma-separated. A failed notification is a warning; the server keeps
serving.

### Resets

With `DEADDROP_RESET_TOKEN` set on the server (16 or more characters, e.g.
`openssl rand -hex 32`), anyone with a ticket and the token can switch it to
another torrent:

```sh
DEADDROP_RESET_TOKEN=<token> deaddrop reset <ticket> <new-source>
```

This prints the new ticket (and key) on stdout. The old ticket stops working.
`--wait-for-reset` makes a single-use server stay up after each download and
wait for the next reset.

## Fetching

```sh
deaddrop fetch "$TICKET" -o ./downloads
DEADDROP_KEY="$KEY" deaddrop fetch "$TICKET" -o ./downloads   # data served with --encrypt
```

- `fetch` checks the size first, then downloads into
  `.deaddrop-<hash>/` inside the output dir, writes the files, and deletes the
  temp folder.
- If the data is encrypted and no key was given, it stops with an error but
  **keeps the download**. Rerunning with the key finishes without the server,
  even after a single-use server has exited.
- A key passed for data that isn't encrypted is ignored, with a note.

## Output and exit codes

- Exit 0 on success, including a single-use server shutting down after its
  download and Ctrl-C. Exit 1 on error, 2 on bad usage.
- When stderr isn't a terminal (as for most agents), progress is a plain line
  every few seconds instead of progress bars.
- Lines like `connection to <id> using a direct path (...)` or `... the relay
  <url>` show how the two sides are connected. Relay-only transfers work but
  are slower.

## Configuration

Every flag has a config file equivalent. Precedence: flag > environment
variable > config file > default. Every key, with its default and meaning, is
in [references/config.md](references/config.md). Read it before writing a
config file.

- `deaddrop config path` shows which file is used and whether it exists
  (`~/.config/deaddrop/config.toml` on Linux; `--config <FILE>` or
  `DEADDROP_CONFIG` picks another).
- `deaddrop config example` prints the commented template.
- `deaddrop config check` validates the file and prints the settings, with
  secrets hidden.

### Setting up a config file

Follow these steps when the user asks to configure deaddrop:

1. **Find the file.** Run `deaddrop config path`. If it exists, run
   `deaddrop config check` and edit that file. Don't replace it, and never
   overwrite an existing config without asking.
2. **Ask what's needed.** Only ask about what applies:
   - Is this machine a server (`serve`), a client (`fetch`), or both?
   - Server: single use (default) or `multi_use`? Keep the data
     (`data_dir`) or use a temp dir? Should resets be allowed
     (`reset_token`), and should it `wait_for_reset`?
   - Server: is age encryption wanted (`encrypt`)? If so, should the key be
     printed or only sent to notifiers (`hide_key`)?
   - Server: where should tickets go (Discord or webhook notifiers), and which
     of them should get the key (`include_key`)?
   - Client: where should downloads go (`fetch.out`)?
   - Relays: n0's public relays (default), the user's own (`[relay] urls`),
     or none (`mode = "disabled"`)?
3. **Write only what differs from the defaults.** Every setting is optional.
   Start from the relevant parts of `deaddrop config example`, and leave out
   keys that would just repeat a default.
4. **Handle secrets.**
   - Generate reset tokens with `openssl rand -hex 32` and don't echo them back.
   - Write webhook URLs exactly as the user gave them, and don't repeat them
     in your reply.
   - If the file holds any secret, run `chmod 600` on it.
   - Never put a config file with secrets inside a git repository.
5. **Validate.** Run `deaddrop config check`. It catches unknown keys, bad
   types, invalid notifier URLs, short reset tokens and invalid `[relay]`
   settings. Fix any error it reports, then tell the user what you set,
   without the secrets.

Example: a single-use server that encrypts, sends the ticket to Discord, and
sends the key only to a private webhook, with resets allowed:

```toml
[serve]
data_dir = "/var/lib/deaddrop"
encrypt = true
hide_key = true
reset_token = "<output of openssl rand -hex 32>"

[[serve.notify]]
type = "discord"
url = "https://discord.com/api/webhooks/<id>/<token>"

[[serve.notify]]
type = "webhook"
url = "https://example.com/hooks/deaddrop"
include_key = true
```

Example: a client that saves downloads to one folder and uses its own relay:

```toml
[fetch]
out = "/home/me/Downloads/deaddrop"

[relay]
urls = ["https://relay.example.com"]
```

## Troubleshooting

- **"failed to connect to the server":** the server isn't running, the
  single-use download already happened, or the ticket is stale after a reset.
  Ask for a fresh ticket.
- **"the data is encrypted: pass the decryption key":** set `DEADDROP_KEY` and
  rerun the same command.
- **Torrent stuck at "waiting for metadata":** there are no peers. Check the
  magnet link, or add `--peer`.
- **Only relay paths:** both sides are likely behind strict NATs or UDP is
  blocked. It still works, just slower. Custom relays go in `[relay]`.
- **"--hide-key needs a notifier that gets the key":** add `+key` to a
  notifier, or drop `--hide-key`.
