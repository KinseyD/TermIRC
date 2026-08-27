# TermIRC

[![CI](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml/badge.svg)](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)

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
   ```

   A filled-in sample (`test.toml`) ships in the repo root.

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

Two expectations worth setting: opening a channel jumps to its newest
messages and the view follows new ones while you stay at the bottom; and
TermIRC never auto-reconnects — after a drop, restart it.

## Roadmap

- [ ] Mouse support
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
- **Nothing arrives after a disconnect** — there is no auto-reconnect by
  design; restart TermIRC.
- **Where do the logs live?** Runtime logs (connections, errors; no chat
  content) rotate daily under `~/.config/termirc/logs/`, 7 days kept.
