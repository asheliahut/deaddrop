# Changelog

All notable changes to deaddrop. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **A single-use server deletes the data when its download is done**, also
  from a `--data-dir` and also without `--wait-for-reset`. Before, it only
  shut down, and the files (plaintext, or the encrypted copies with
  `--encrypt`) and blob store stayed in a `--data-dir`. A multi-use server
  still keeps its data until a reset replaces it or, with a `--data-dir`,
  after exit.
- A reset that is still downloading when the server exits is deleted instead
  of left half-done in the data dir.

### Added

- **`serve` can serve more than torrents.** `SOURCE` can now also be:
  - **local files and directories** (one or more): served where they are,
    never moved, changed or deleted, also with `--encrypt` and after a
    single-use download. A directory keeps its name as the top folder;
    symlinks inside it, special files and empty folders are skipped.
  - **`-` for stdin**, saved as one file named by `--name` (default
    `stdin`).
  - **any http(s) URL**, which the server downloads and serves as one file
    (named by `--name`, `Content-Disposition` or the URL). A response that is
    a `.torrent` file (by `Content-Type` or its contents) is still downloaded
    as a torrent, so tracker links without `.torrent` in the URL work as
    before.

  `--as-is` serves a `.torrent` file or URL as a plain file. Magnet URIs,
  local `.torrent` files and URLs ending in `.torrent` work as before. Resets
  still only take torrents, so a reset token can't make the server read its
  own files or fetch arbitrary URLs.
- **`serve --allow <ID>` lets only specific clients download.** A client
  runs the new `deaddrop id` command once to create a persistent identity
  and gives the printed id to the server's operator. Everyone else is turned
  away right after the handshake, so a leaked ticket is useless to them, and
  `fetch` stops at once with a clear message instead of retrying. Repeatable,
  `DEADDROP_ALLOW` (comma-separated) or `serve.allow`. `fetch` uses the
  identity once it exists (`--identity`, `DEADDROP_IDENTITY` or
  `fetch.identity` pick another file); without one it still uses a new random
  id each time.
- **`serve --expire-after <DURATION>` makes tickets expire.** Each ticket
  stops working that long after it is printed (`30m`, `24h`, `7d`, `1h30m`)
  if it hasn't been used up. Then the data is deleted and the server exits,
  or with `--wait-for-reset` waits for the next reset, whose ticket gets the
  full time again. A download already running may finish. Also
  `DEADDROP_EXPIRE_AFTER` and `serve.expire_after`. Notifications say when
  the ticket expires: `expires_at` (Unix seconds) in webhook JSON, a
  relative time in Discord.
- **The server says why it turns a client away**, and `fetch` explains it
  instead of a bare "permission" error: another client is downloading from
  this single-use server, the drop was already downloaded, the ticket
  expired or was replaced by a reset, or the client isn't on the allow
  list. Only a busy slot is retried, quietly (one line, not one per
  attempt); the others stop at once. A single-use server now refuses a
  second client as soon as it connects. If the server disappears while a
  client waits for a busy slot, `fetch` says the other download most likely
  finished. The server logs every refusal, and `fetch` no longer leaves an
  empty `.deaddrop-<hash>` folder behind, or keeps a partial download that
  can never be finished.
- **`fetch --list` shows what's in a drop without downloading it**: each
  file's path and size on stdout, and a total. Only the names and sizes are
  fetched, and the server doesn't count that as the single-use download, so
  the real `fetch` still works afterwards. With `--encrypt` data the names
  are shown only with the key. A server older than 2.1 does count a listing
  as its one download.
- **`fetch` resumes interrupted downloads.** If the connection drops, it
  reconnects and downloads only what is missing, for up to `--retry-for`
  seconds (`fetch.retry_for`, default 120) without progress. If it gives up
  or is stopped, the partial download is kept in `.deaddrop-<hash>`, and
  running the same command again picks up where it left off. Before, a
  failed download was thrown away. A single-use server already let a client
  that dropped out before finishing connect again.

## [2.0.0] - 2026-11-13

### Breaking

- **age encryption is now off by default.** In 1.0, every file was always
  age-encrypted and `serve` always printed a decryption key. In 2.0, `serve`
  sends the files as they are (iroh still encrypts the connection end to end)
  unless you pass `--encrypt` or set `serve.encrypt = true`. To keep the 1.0
  behaviour, add `encrypt = true` under `[serve]` in your config file.
- **stdout format depends on encryption.** `serve` and `reset` print one line
  per ticket on stdout: `<ticket>` without encryption, `<ticket> <key>` with
  `--encrypt` (and `<ticket>` again with `--hide-key`). Scripts that do
  `read -r TICKET KEY` still work, but `KEY` is empty when there is no key to
  show.
- **A 2.0 client can't download from a 1.0 single-use server.** Before
  downloading, the client now sends a small size-probe request so it can show
  a total and an ETA. A 1.0 single-use server counts that probe as the
  download and then refuses the real one. Upgrade servers and clients
  together. A 1.0 multi-use server still works.

### Added

- **`deaddrop skill`** installs an [Agent Skill](https://agentskills.io)
  that teaches AI coding agents to use deaddrop and to set up its config file
  step by step. It ships `SKILL.md` plus `references/config.md`, generated
  from this version's config template.
  - `install`, `uninstall`, `status` and `show` subcommands.
  - `--global` (your user) or `--project` (the enclosing git repository, or
    `--dir`).
  - `--harness claude,codex,copilot,agents` or `--all`, writing to
    `.claude/skills`, `.agents/skills`, `.copilot/skills` (global) and
    `.github/skills` (project).
  - Asks interactively on a terminal when the scope or agents aren't given,
    pre-selecting the agents it finds.
  - Never replaces or removes a `deaddrop` skill it didn't write unless you
    pass `--force`, and keeps other files in the skill folder on uninstall.
  - The skill is also committed at `skills/deaddrop/`, so tools that install
    skills straight from a GitHub repository can use it too.
- **`--encrypt` / `serve.encrypt`:** opt-in age encryption at rest. Files are
  encrypted with a new X25519 key per torrent, the plaintext is deleted, and
  file names are hidden in an encrypted manifest, as in 1.0.
- **`--hide-key` / `serve.hide_key`:** with `--encrypt`, the decryption key is
  never printed on stderr or stdout, and never sent back to `deaddrop reset`.
  Only notifiers with `include_key` (`+key`) receive it. The server refuses to
  start if no notifier would receive the key, and prints an error if every
  one of them failed to send. Without `--encrypt`, the setting has no effect.
- **`[relay]` config section** for iroh's relay servers, used by `serve`,
  `fetch` and `reset`:
  - `mode = "default"`: n0's public relays (same as 1.0).
  - `mode = "custom"` with `urls = [...]`: only your relays, for example your
    own `iroh-relay`. Setting `urls` alone implies `custom`.
  - `mode = "disabled"`: no relays, so a transfer only works if the two sides
    can reach each other directly or by hole punching.
  - Invalid combinations are errors at startup, and `deaddrop config check`
    shows the setting.
- **Connection path reporting:** every connection prints whether it uses a
  relay or a direct path, and prints again when it switches, for example:
  `connection to e86045b272 switched to a direct path (203.0.113.7:50687)`.
  This works on the server, the client and `reset`.
- **Progress bars (indicatif)** on stderr when it's a terminal:
  - `serve`: a bar for the torrent download (spinner while metadata resolves,
    then bytes, speed, ETA and peer count).
  - `serve`: one bar per client transfer, named by the client's short id.
    With `--multi-use`, several run at once.
  - `fetch`: a bar for the download, with total size and ETA.
  - Finished bars become one-line summaries, e.g.
    `✔ sent 500.00 MiB in 6.3s (79.42 MiB/s) to b2319767b8`.
  - Status lines (client connected, notifications, connection paths) print
    above the bars without breaking them.
- **`fetch` keeps the download when the key is missing.** If the data is
  encrypted and no key was given, the download stays in `.deaddrop-<hash>`
  in the output folder. Running the same command again with the key finishes
  without contacting the server, even after a single-use server has shut
  down.
- `fetch` works out by itself whether the data is encrypted. If you pass a
  key for data that isn't encrypted, it prints a note and ignores the key.

### Changed

- **Plain progress output when stderr isn't a terminal** (logs, systemd,
  pipes): no bars, just a progress line every few seconds and a summary at
  the end. The torrent progress line keeps its 1.0 format.
- **`--no-progress` / `DEADDROP_NO_PROGRESS` / `progress = false`** force
  plain progress lines even on a terminal, for recorded sessions (`script`,
  tmux logging, CI pseudo-terminals). `TERM=dumb` does the same.
- **Log lines have no color codes unless stderr is a terminal.** 1.0 colored
  `tracing` log lines even when written to a file or pipe. Colors now follow
  `NO_COLOR`, `CLICOLOR=0`, `CLICOLOR_FORCE` and `TERM=dumb`, like the
  progress bars.
- **Single-use gate:** size-probe requests are answered but never count as
  the one download.
- Notifiers send the decryption key only when the data is encrypted, and only
  to notifiers with `include_key`. Without `--encrypt`, every notifier gets
  just the ticket.
- **Docs:**
  - The package description, `--help` text, README, `SECURITY.md` and
    `config.example.toml` describe age encryption as an optional extra layer.
  - They document `--hide-key`, `[relay]`, connection paths, progress output,
    `--no-progress` and `deaddrop skill`, and `--help` has examples for them.
  - `SECURITY.md` now covers size probes, `deaddrop skill`, what relays can
    see, and hardening tips for `--hide-key` and running your own relay.
- **Releases:** GitHub release notes now come from this changelog (the tag's
  section must exist, or the release fails early). Release archives include
  `CHANGELOG.md`. The Homebrew formula's description no longer says the data
  is always age-encrypted.

### Dependencies

- Added `indicatif` 0.18.6.

## [1.0.0] - 2026-11-10

First release: leech a torrent on a server without seeding it, age-encrypt
it, and hand it to one client peer to peer over iroh. Includes single-use and
multi-use serving, resets with a reset token, `--wait-for-reset`, Discord and
webhook notifiers, a TOML config file and shell completions.

[2.0.0]: https://github.com/asheliahut/deaddrop/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/asheliahut/deaddrop/releases/tag/v1.0.0
