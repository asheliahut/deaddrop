# Changelog

All notable changes to deaddrop. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

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
