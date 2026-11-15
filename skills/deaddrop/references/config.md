# deaddrop config file reference

Every setting deaddrop reads from its TOML config file, as printed by `deaddrop config example`. Every key is optional. Precedence: command-line flag > environment variable > config file > default. Validate with `deaddrop config check`.

<!-- generated-by: deaddrop -->

```toml
# deaddrop config file.
#
# Default location: ~/.config/deaddrop/config.toml (Linux), or pass --config <file>
# (or set DEADDROP_CONFIG). Every setting is optional.
#
# Precedence: command-line flag > environment variable > this file > default.
# Relative paths are resolved against this file's directory.
#
# This file may hold webhook URLs, which are secrets: chmod 600 it.

# Log filter, same syntax as RUST_LOG. RUST_LOG overrides it.
# "off" disables logging entirely. Default: "off" in release builds,
# "warn" in debug builds.
# log = "warn"

# Progress bars on stderr. They're only drawn when stderr is a terminal (and
# TERM isn't "dumb"); logs, pipes and files always get a plain progress line
# every few seconds. false: always plain lines, e.g. when a terminal session
# is recorded. --no-progress and DEADDROP_NO_PROGRESS override it.
# progress = true

[serve]
# false (default): shut down after one client has downloaded the data.
# true: keep serving to any number of clients until Ctrl-C.
multi_use = false

# Where the torrent data and blob store go. If unset, a temp dir is used and
# deleted on exit. A single-use server deletes the data from here too once its
# one download is done; a multi-use server leaves it until a reset.
# data_dir = "/var/lib/deaddrop"

# Extra BitTorrent peers to connect to.
# peers = ["192.168.1.10:6881"]

# Let `deaddrop reset` replace the torrent being served with a new magnet or
# .torrent. Anyone with the ticket AND this token can do it. At least 16
# characters; `openssl rand -hex 32` makes a good one. Unset (the default)
# turns resets off. DEADDROP_RESET_TOKEN overrides it.
# reset_token = "<long random string>"

# Single use only, needs reset_token: instead of exiting after the one
# download, delete the data and wait for a reset to bring the next torrent.
# wait_for_reset = false

# Also age-encrypt the files before serving them, delete the plaintext, and
# print a decryption key the client needs. Off by default: iroh already
# encrypts the connection. Turn it on if the ticket alone shouldn't be enough
# to read the data, or the plaintext shouldn't stay on disk while serving.
# encrypt = false

# Only with encrypt = true: never print the decryption key (stdout gets just
# the ticket) and never send it back to `deaddrop reset`. The only copies go
# to notifiers with include_key = true, and at least one is required. Keeps
# the key out of terminals and logs. No effect without encrypt.
# hide_key = false

# Only let these clients download (or list). Each client gets its id from
# `deaddrop id`; everyone else is turned away, so a leaked ticket is useless
# to them. Empty (the default): anyone with the ticket. --allow and
# DEADDROP_ALLOW replace this list. Resets aren't affected.
# allow = ["<id from `deaddrop id` on the client>"]

# Stop serving each ticket this long after it's printed, if it hasn't been
# used up: then the data is deleted and the server exits (or, with
# wait_for_reset, waits for a reset, whose ticket gets the full time again).
# A download already running may finish. Like "30m", "24h", "7d" or "1h30m";
# a bare number is seconds. Unset or "never" (the default): no limit.
# expire_after = "24h"

# Notifiers, sent when the ticket is ready. Add as many as you like.
# Any --notify flag or DEADDROP_NOTIFY replaces this whole list.
# With encrypt = true, include_key = true also sends the decryption key
# (default: ticket only).
[[serve.notify]]
type = "discord"
url = "https://discord.com/api/webhooks/<id>/<token>"

# Email over SMTP. smtps:// is TLS from the start (port 465); smtp:// requires
# STARTTLS (port 587), add ?tls=opportunistic to allow without, or ?tls=none
# for a local relay (no password then, unless it's localhost). Percent-encode
# @ : / in the user name or password. Email isn't end-to-end encrypted, so
# think twice before include_key = true.
[[serve.notify]]
type = "email"
url = "smtps://user:password@smtp.example.com"
from = "deaddrop <deaddrop@example.com>"
to = ["me@example.com"]              # one address, or a list

# Slack incoming webhook.
[[serve.notify]]
type = "slack"
url = "https://hooks.slack.com/services/<T...>/<B...>/<secret>"

# Mattermost incoming webhook.
[[serve.notify]]
type = "mattermost"
url = "https://mattermost.example.com/hooks/<key>"

# Rocket.Chat incoming webhook.
[[serve.notify]]
type = "rocketchat"
url = "https://chat.example.com/hooks/<id>/<token>"

[[serve.notify]]
type = "webhook"
url = "https://example.com/hooks/deaddrop"
include_key = true

# ntfy push notification: the URL is the topic URL (ntfy.sh or self-hosted).
# On a public server the topic name works like a password: make it long and
# random, and don't send the key there unless the topic is protected.
[[serve.notify]]
type = "ntfy"
url = "https://ntfy.sh/<long-random-topic>"
# token = "tk_..."   # access token for a protected topic (or user:pass@ in the URL)
# priority = 4       # 1 (min) to 5 (max); ntfy's default is 3

# Telegram bot message. Make a bot with @BotFather, add it to the chat, and
# use the chat's id (a number, or "@channelname" for a public channel).
[[serve.notify]]
type = "telegram"
bot_token = "123456789:AAE_token_from_BotFather"
chat_id = -1001234567890
# thread_id = 42                          # post in this forum topic
# api_url = "http://localhost:8081"       # self-hosted Bot API server

# SMS through Twilio. One table per phone number to text. SMS isn't
# end-to-end encrypted: carriers can read it, so think twice before
# include_key = true.
[[serve.notify]]
type = "twilio"
account_sid = "AC0123456789abcdef0123456789abcdef"
auth_token = "<auth token from the Twilio console>"
from = "+15550001111"      # Twilio number, sender id like "Deaddrop", or "MG..." Messaging Service
to = "+15552223333"        # E.164
# api_key = "SK..."        # log in with an API key; auth_token is then its secret
# api_url = "https://api.dublin.ie1.twilio.com"   # another Twilio API host

[fetch]
# Where `deaddrop fetch` writes files. Default: the current directory.
# out = "/home/me/Downloads"

# If the connection drops, keep reconnecting (and resuming) for this many
# seconds without progress. 0 gives up on the first failure. Default: 120.
# retry_for = 120

# Identity file for `fetch` (and `deaddrop id`), so a server can --allow this
# client. Default: ~/.local/share/deaddrop/identity.key on Linux, used only
# once `deaddrop id` has created it; without one, each fetch uses a new
# random id. DEADDROP_IDENTITY and --identity override it.
# identity = "/home/me/.local/share/deaddrop/identity.key"

[reset]
# The server's reset token, used by `deaddrop reset`. --token and
# DEADDROP_RESET_TOKEN override it.
# token = "<long random string>"

[relay]
# Relay servers, used by serve, fetch and reset. iroh always tries a direct
# (hole-punched) path; relays get the connection started and carry the data
# if no direct path works. Each connection prints which path it uses.
#   "default"  n0's public relays (the default)
#   "custom"   only the relays in `urls`, e.g. your own iroh-relay
#   "disabled" no relays: both sides must reach each other directly
# mode = "default"
# urls = ["https://relay.example.com"]   # setting urls implies mode = "custom"
```
