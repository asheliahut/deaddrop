# Security Policy

deaddrop moves data that people want kept private, so security reports are
taken seriously. Thank you for helping keep it safe.

## Supported versions

deaddrop follows [semantic versioning](https://semver.org). Security fixes
land in the latest release of the current major version (`vX`); there are no
backports to older minor or patch releases, so upgrade to the newest `vX.Y.Z`
to pick up a fix.

| Version | Supported |
| --- | --- |
| Latest `vX.Y.Z` (current major) | ✅ |
| Older `vX.Y.Z` releases in the current major | ❌ (upgrade to the latest) |
| Previous major versions (`vX-1` and earlier) | ❌ |
| `main` | ✅ (best effort) |

When a new major version ships, the previous major stops receiving fixes.

## Reporting a vulnerability

**Please do not open a public issue, discussion or pull request for a
security problem.**

Report it privately through GitHub's
[private vulnerability reporting](https://github.com/asheliahut/deaddrop/security/advisories/new)
(the **Security** tab → **Report a vulnerability**).

Please include, as far as you can:

- The deaddrop version (`deaddrop --version`) or commit, and your platform.
- Which command and options are involved (`serve`, `fetch`, `reset`,
  notifiers, config file).
- What an attacker needs (the ticket, the key, network position, a reset
  token, local access to the server, ...) and what they gain.
- Steps to reproduce, or a proof of concept.

Leave real tickets, keys, reset tokens and webhook URLs out of the report, or
rotate them first.

## What to expect

deaddrop is maintained by volunteers, so these are goals rather than
guarantees:

- An acknowledgement within **7 days**.
- A first assessment (accepted, needs more info, or out of scope) within
  **14 days**.
- For accepted reports, a fix and a release as soon as practical, followed by
  a published GitHub security advisory. You'll be credited in the advisory
  unless you'd rather not be.

Please give us a reasonable chance to ship a fix before disclosing publicly.
If 90 days pass without a fix, or without an agreed timeline, you're free to
disclose.

## Scope

In scope are problems in deaddrop's own code that break what it promises.
For example:

- **Confidentiality:** a party without the ticket (a relay, a network
  observer) reading the data; with `--encrypt`, recovering plaintext file
  contents, file names or the torrent name without the key, or the decryption
  key being written to the server's disk, leaked to a notifier that doesn't
  have `include_key`, or, with `--hide-key`, showing up in any output or reset
  reply.
- **Integrity:** `fetch` accepting tampered or wrong-key data, or writing a
  partial file under its real name.
- **Client file system:** a malicious server making `fetch` write outside the
  output directory (path traversal, absolute paths, symlinks, ...).
- **Seeding:** deaddrop uploading torrent piece data to the swarm, or
  accepting incoming BitTorrent connections.
- **Single use:** a second client downloading from a single-use server, a
  size probe being counted as the one download, or an old ticket still
  serving data after a reset.
- **Reset protocol:** resetting a server without the right token, bypassing
  the wrong-token delay, or a reset source being read as a path on the
  server's disk.
- **Secrets in output:** webhook URLs or reset tokens showing up in logs,
  `deaddrop config check` or error messages.
- **`deaddrop skill`:** writing outside the chosen skills directory, or
  replacing or removing files it didn't write without `--force`.
- Crashes or resource exhaustion that a remote peer can trigger cheaply.

### Out of scope

These are known limits, documented in the [README](README.md#encryption):

- Without `--encrypt` (the default), anyone holding the ticket can download
  and read the data, and it is stored on the server's disk as plaintext while
  it is served.
- File sizes, file count, and the fact that a transfer happened are not
  hidden.
- Plaintext exists on the server's disk while the torrent downloads, and
  deletion is not secure erasure (especially on SSDs).
- With `--encrypt` (and without `--hide-key`), the key appears on the server's stderr/stdout,
  so a persistent log of that output (systemd journal, log files) contains it.
- With `--encrypt`, anyone holding the ticket can download the ciphertext;
  anyone holding both the ticket and the key can read it.
- The BitTorrent swarm, trackers and DHT can see the server's IP address
  while it leeches.
- Relay servers (n0's public ones by default) and the address lookup service
  see which endpoints connect and their IP addresses, but never the data:
  relays only forward end-to-end encrypted packets.
- What an AI agent does with the instructions in the `deaddrop` skill.
- Content legality, or what users choose to share.

Bugs in dependencies (iroh, iroh-blobs, librqbit, age/rage, ...) should go to
those projects. If deaddrop uses a dependency in a way that makes it
exploitable, or ships a version with a known advisory, report it here.

## Hardening tips for users

- Treat the ticket as a secret: without `--encrypt` it is all anyone needs.
- Use `--encrypt` when the server's disk or the channel the ticket travels
  over isn't trusted, and send the ticket and the key over different
  channels.
- Prefer the config file (`chmod 600`) or environment variables over
  command-line flags for webhook URLs and reset tokens, to keep them out of
  shell history and `ps`.
- Use a long random reset token, e.g. `openssl rand -hex 32`.
- Only give `include_key` to notifiers you trust with the data.
- With `--encrypt`, add `--hide-key` to keep the key out of the server's
  terminal, logs and stdout entirely, so only your key notifiers have it.
- Run your own iroh relay (`[relay] urls`) if connection metadata shouldn't
  go through n0's public relays.
- Verify release downloads against `SHA256SUMS`.
- Installed with Homebrew? Run `brew upgrade deaddrop` to pick up security
  releases.
