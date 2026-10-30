# TermIRC

[![CI](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-DEA584?logo=rust)](https://www.rust-lang.org/)

A terminal IRC client written in Rust. One connection per configured
server, one config file, no CLI flags.

## Requirements

- [Rust](https://rustup.rs) 1.88 or newer
- A terminal with Unicode support; on Windows use **Windows Terminal**

## Quick start

1. Create `~/.config/termirc/config.toml`
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

   Or just run TermIRC with no config file: the built-in setup wizard
   opens on first launch and writes the file for you (`/config` reopens
   it anytime).

2. Run:

   ```bash
   cargo run --release
   ```

3. Pick a channel in the sidebar (`j`/`k`, `Enter`) and type. `Enter` sends.

## Essentials

- `Tab` cycles focus between the sidebar, the messages, and the composer;
  the focused pane receives your keys.
- `j`/`k` move the sidebar cursor or message selection, `PgUp`/`PgDn` scroll,
  `Esc` quits.
- The mouse works too: wheel scrolls, a click focuses a pane or opens a
  channel. Hold `Shift` for the terminal's own text selection.
- The bottom bar shows where your focus is.

### Slash commands

| Command | Behavior |
| --- | --- |
| `/join #channel` | Join or focus a channel on the current server. |
| `/part [#channel] [reason]` | Leave a channel; its buffer and history are kept. |
| `/query <nickname>` | Open a private conversation. |
| `/close` | Hide the current private conversation. |
| `/msg <target> <text>` | Send a message without switching conversation. |
| `/me <action>` | Send a CTCP ACTION. |
| `/nick <nickname>` | Request a nickname change. |
| `/topic` `/names` `/mode` `/whois` | Channel/user queries; replies appear in the buffer that issued the command. |
| `/away [reason]` / `/back` | Set or clear away status. |
| `/connect [server]` / `/reconnect [server]` | Connect or restart a server's connection. |
| `/disconnect [reason]` / `/quit [reason]` | Disconnect (Esc or Ctrl+C exits TermIRC). |

Invalid or unavailable commands keep the original input for editing.

## Configuration notes

- All `[servers.<name>]` keys are documented in `config.example.toml`;
  the config is read once at startup — restart to pick up changes.
- Optional SOCKS5 proxy per server via `[servers.<name>.proxy]`
  (Tor-ready: DNS also resolves through the proxy).
- Optional persistent history via `[history] persist = true` (SQLite,
  `retention_days`, `exclude`).
- UI language via `[ui] language` (built-in English + zh-CN).

## Troubleshooting

- **CJK characters render as boxes** — use Windows Terminal (or run
  `chcp 65001`) with a CJK font.
- **A server won't connect** — check the config and the log, then use
  `/connect` or `/reconnect`.
- **Where do the logs live?** `~/.config/termirc/logs/`, daily files,
  7 days kept.
