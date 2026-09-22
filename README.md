# TermIRC

[![CI](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml/badge.svg)](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-DEA584?logo=rust)](https://www.rust-lang.org/)
[![Built With Ratatui](https://img.shields.io/badge/Built_With_Ratatui-000?logo=ratatui&logoColor=fff)](https://ratatui.rs/)

A terminal IRC client written in Rust. TermIRC reads your servers from a
TOML config file, opens one connection per server — joining every
configured channel — and gives you a scrollable chat pane, a
server/channel sidebar, and a composer to talk with. `Enter` sends.

## Requirements

- [Rust](https://rustup.rs) 1.85 or newer
- A terminal with Unicode support; on Windows use **Windows Terminal**
  (a CJK-capable font is needed if you read CJK channels)

## Quick start

1. Create the config file at `~/.config/termirc/config.toml`
   (Windows: `C:\Users\<you>\.config\termirc\config.toml`):

   ```toml
   [servers.osu_irc]
   username = "YourName"
   nickname = "YourName"
   password = "your-password-or-token"
   server = "irc.ppy.sh"
   use_tls = false
   port = 6667
   channels = ["#osu"]
   queries = ["alice", "bob"]
   ```

   A sample (`config.example.toml`) ships in the repo root; replace its connection settings before use.

2. Run:

   ```bash
   cargo run --release
   ```

3. You land on the welcome page — just the TermIRC logo. Focus starts on
   the sidebar: move with `j`/`k`, press `Enter` on a channel, and type.

## Configuration

Each `[servers.<name>]` table is one connection; TermIRC connects to all
of them in parallel and joins all of their channels. The config is read
once at startup — restart to pick up changes.

| Key        | Required | Default | Meaning                              |
|------------|----------|---------|--------------------------------------|
| `username` | yes      | —       | IRC username                         |
| `nickname` | yes      | —       | the nick others see                  |
| `password` | yes      | —       | server password / auth token         |
| `server`   | yes      | —       | hostname                             |
| `port`     | yes      | —       | usually 6667 (plain) or 6697 (TLS)   |
| `use_tls`  | no       | `false` | connect over TLS                     |
| `channels` | yes      | —       | channels to join, in order           |
| `queries`  | no       | `[]`    | private conversations to show at startup, in order |

`queries` contains individual nicknames, not channels or comma-separated targets.
Repeated names are deduplicated within each server using ASCII case-insensitive
matching; the same nickname on different servers is a separate conversation.
These entries only create sidebar conversations: they do not send JOIN or any
private message, and do not indicate whether the other user is online.

Security notes:

- The file holds a credential — on Unix, `chmod 600` it.
- With `use_tls = false` the password crosses the network unencrypted.
  If your network offers TLS, set `use_tls = true` and the TLS port.

## Using TermIRC

The gist: `Tab` cycles focus between the sidebar, the messages, and the
composer, and the focused pane receives your keys. `j`/`k` move the
sidebar cursor or the message selection, `Enter` opens the highlighted
channel (sidebar) or sends your line (composer), `PgUp`/`PgDn` scroll the
messages, and `Esc` quits — the composer's bottom row always repeats the
essentials. The rest is there to explore.

The mouse works too: the wheel scrolls the messages, resting the pointer
on a message pre-highlights it, and a click focuses a pane — clicking a
channel in the sidebar opens it. While TermIRC runs, hold `Shift` for the
terminal's own text selection.

Inputs beginning with `/` after trimming surrounding whitespace are parsed
as slash commands in both channels and the server console. Supported
identity and connection commands are:

| Command | Behavior |
| --- | --- |
| `/query <nickname>` | Create or reopen a private conversation on the current server and focus its composer; works while disconnected. |
| `/close` | Hide the current private conversation and return to its server console; not available on channels or server consoles. |
| `/nick <nickname>` | Request a nickname change on the current server; local echoes use the new nickname after the server confirms it. |
| `/away [reason]` | Set an away message. With no argument, toggle between away (default reason `Away`) and back, using the confirmed away state. |
| `/back` | Clear away status on the current server. |
| `/connect [server]` | Connect a stopped server using its configuration key; defaults to the current server. |
| `/reconnect [server]` | Restart that server's connection, or connect if stopped; defaults to the current server. |
| `/disconnect [reason]` | Disconnect the current server and cancel automatic retries. |
| `/quit [reason]` | Alias for `/disconnect`; the application remains open. Use Esc or Ctrl+C to exit TermIRC. |

Server keys are matched case-insensitively; keys that differ only in ASCII
letter case cannot coexist in one configuration. Connection commands use the
existing configuration; they do not add servers. Successful command
submission clears the composer. Invalid, unsupported or unavailable
commands retain the original input and cursor for editing. Slash input
never becomes chat or a raw console line; `//` is not an escape for sending
a literal slash. Local command feedback is only recorded at DEBUG level,
without command contents or arguments, and never adds a status-bar prompt
or history line. Normal server messages still appear in the server console.

Each server and channel has a status dot (private conversations do not):

- **Green:** server registration confirmed, or our JOIN confirmed for a channel.
- **Blinking grey:** connecting, waiting to retry, or waiting for a channel JOIN.
- **Red:** stopped, including manual disconnection, exhausted retries or a failed channel JOIN.

Unexpected disconnections automatically retry up to three times, waiting
1, 2 and 4 seconds. A connection/registration attempt times out after
30 seconds; a missing channel JOIN confirmation also times out after
30 seconds. Registration nickname/password rejection or a server ban stops
automatic retries immediately. A connection stable for 30 seconds resets
the retry budget. `/connect` or `/reconnect` starts a fresh retry budget
after stopping. Reconnection retains the confirmed nickname in memory,
rejoins configured channels and clears away status. Queued messages from a
previous connection are discarded, never replayed on the new connection.

Opening a channel jumps to its newest messages, and the view follows new
ones while you stay at the bottom. Returning to a previously opened conversation
restores its reading position. Each server console, channel and private conversation keeps its own
draft and editing cursor. Histories remain available across disconnections,
with up to 5,000 messages per conversation; resizing never deletes messages.

Outgoing IRC lines are limited to 512 UTF-8 bytes including the command,
target and terminating CRLF. Oversized lines retain the full draft and cursor
for editing; they are not split or queued. Raw console commands preserve the
spaces within their trailing parameter. Local echoes mean the message was
queued, not acknowledged by the server. Server errors for configured channels
appear there once as system messages; other errors appear in the server console.

### Private conversations

The sidebar groups each server's channels followed by private conversations,
shown as `@nickname`. Incoming private messages automatically create or reopen a
conversation without changing your current page or draft. A trailing `*` marks
unread messages; it clears only when that conversation is visible at the bottom
of a nonempty message pane. Scrolling through older messages keeps it unread.
The sidebar scrolls with keyboard navigation or its mouse wheel.

Use `/query alice` to open a conversation, then type normally and press Enter.
Sending only requires a registered server connection, not a channel JOIN.
The first visit opens at the newest message; later visits restore the reading
position. Private ACTION messages are displayed; ordinary NOTICE messages still
go to the server console. Replies 401 (no such nickname) and 301 (away) go to an
existing visible private conversation, otherwise to the server console.

`/close` hides rather than deletes: history, drafts and reading positions remain
in memory until exit. Opening it again or receiving another private message
restores it. Closing sends no PART or QUIT and does not block the sender.
`/query`, incoming messages and `/close` never write the configuration file.
Configured `queries` appear again on restart; runtime-only conversations do not.
There is no new close shortcut, persistent history, `/msg` or `/me` command.

Confirmed peer nickname changes update the conversation's name and send target
without changing its identity or configuration. If the new nickname already has
a conversation, both histories and drafts are preserved separately: the old
conversation explains the change and blocks sending to the old nickname. Use
`/query <new-nickname>` to continue, or explicitly `/query <old-nickname>` to
select that old nickname again and unblock it. Identity changes while offline
are not inferred; server-specific CASEMAPPING and IRCv3 echo negotiation remain
unsupported. Sending to yourself displays the matching local echo only once.

## Architecture and development

TermIRC remains a single crate. Its internal boundaries are:

| Module | Responsibility |
| --- | --- |
| `core` | Stable server, buffer and message identities; typed conversations, message metadata, drafts and outbound requests. Uses only the standard library. |
| `history` | Bounded message queues per buffer; returns the IDs removed by eviction. |
| `protocol` | IRC decoding, tags, structured server errors, wire encoding and byte limits. |
| `connection` | One worker per server, bounded queues, confirmed connection state, retries and cancellation. |
| `application` | Session registration, input submission, command execution, event routing and unconfirmed local echoes. |
| `tui` | Focus, editing gestures, mouse input, layout, view anchors and rendering. |

`config` and `logging` remain independent services; `main` assembles resources
and restores the terminal. Core and history have no terminal or IRC-library
dependencies. Layout uses message IDs and `usize` row coordinates. Measured row
ranges survive layout-text cache eviction; the 50,000-row cache budget does not
limit history. Rendering constructs text only for visible rows. The composer
uses the same display-width geometry for wrapping, sizing and mouse regions.

Dynamic channels, persistent history, SASL and IRCv3 capability negotiation
are not implemented in this refactor.

Run local validation with cached dependencies:

```sh
cargo test --locked --offline --all-targets
cargo fmt --all --check
cargo clippy --all-targets --locked --offline -- -D warnings
git diff --check
cargo run --release --locked --offline --example layout_benchmark
```

Network tests use mock servers bound to `127.0.0.1`; the benchmark reads no
user configuration and opens no connections. See the
[refactor record](docs/refactor-2026-09-17.md) for regression coverage and measurements.

## Roadmap

- [x] Mouse support
- [ ] Adaptive, flexible layout
- [ ] Dynamic shortcut hints
- [x] Logging
- [ ] Persistent chat history
- [ ] Show system messages (joins, parts, notices)
- [ ] More complete IRC protocol support
- [ ] Web previews / images in the terminal
- [ ] Polished keybinding behavior

## Troubleshooting

- **`failed to load config from …`** — create
  `~/.config/termirc/config.toml` (see [Configuration](#configuration)).
- **CJK characters render as boxes** — use Windows Terminal (or run
  `chcp 65001`) with a CJK font.
- **A server has a red dot** — check the configuration and runtime log, then
  use `/connect` or `/reconnect` in that server's console or a channel.
  A red channel under a green server indicates a failed JOIN, PART or KICK.
- **Where do the logs live?** Runtime logs (connections, errors; no chat
  content) rotate daily under `~/.config/termirc/logs/`, 7 days kept.
