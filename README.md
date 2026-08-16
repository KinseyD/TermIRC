# termirc

A receive-only terminal TUI IRC client written in Rust.

It reads your server list from a TOML config file, connects to the **first
server's first channel**, and displays incoming chat messages in a scrollable
pane beside a server/channel sidebar, with a composer along the bottom.
Sending messages is intentionally not implemented (yet) — the composer accepts
and edits text but `Enter` does not send.

## Layout

```text
 osu_irc          │  #osu
   ▶ #osu        │  alice:  hello world
     #chinese    │  bob:    a longer message that wraps
                  │           under the body column, never
                  │           under the nick
                  │
                  │  ┃ hi_                      <- composer (grows as you type)
                  │  ┃
                  │  ┃ connected to irc.ppy.sh · Esc quit · PgUp/PgDn scroll
                  │  ╹▀▀▀▀▀▀▀▀▀▀
```

- **Sidebar** (fixed width): the configured servers and their channels; the
  connected channel is highlighted.
- **Message pane**: each message spans the pane; the nick and body each take
  their own horizontal space, and wrapped body lines are indented to the body
  column (never under the nick). Exactly one blank line separates two messages.
- **Composer** (bottom): a shaded input box with a pale-green `┃` accent on its
  left. It keeps a 3-column gap from the screen's right edge and a 2-column
  margin inside the box. When the typed text exceeds one line it wraps, the
  composer grows taller, and the message pane shrinks to match. A single blank
  row (global background) sits between the messages and the composer.

## Keys

Focus cycles between the three panes with `Tab`:

| Key | Sidebar focused | Messages focused | Composer focused |
|-----|-----------------|------------------|------------------|
| `j` / `k` | move the cursor down / up | select the next / previous message | type `j` / `k` |
| `Enter` | collapse/expand a server row, or switch to the channel under the cursor | (no-op) | (no send yet) |
| printable chars | — | — | type into the composer |
| `Backspace`/`Delete`/arrows | — | — | edit the composer input |
| `PgUp` / `PgDn` | scroll the message pane by ⅓ of its height | same | same |
| `Esc` / `Ctrl+C` | quit | quit | quit |

While the sidebar has focus its cursor row is slightly highlighted (with a
block cursor) and the viewed channel row is brighter; `Enter` on a channel
switches the message pane and returns focus to the composer. While the
message pane has focus one message is always selected: its rows are
highlighted, the blank rows above/below render as half blocks, and a pale
green `┃` accent marks its front edge; `j`/`k` move the selection (auto
scrolling minimally to reveal it) and incoming messages do not disturb the
view. The composer's accent dims while another pane has focus.

Connection status is shown on the composer's bottom (tips) row. If the server
drops the connection, the status changes to `disconnected from …` (or an error
message) — termirc does not auto-reconnect; quit and restart to rejoin.

Auto-scroll: when a new message arrives, the view follows it **only if the
newest message's last line is currently visible**. If you have scrolled up so
that line has left the window, the view stays where it is until you scroll
back to the bottom.

## Configuration

The config file lives at `~/.config/termirc/config.toml` (on Windows that is
`C:\Users\<you>\.config\termirc\config.toml`). Copy the sample:

```bash
cp test.toml ~/.config/termirc/config.toml
chmod 600 ~/.config/termirc/config.toml   # Unix: it holds a credential
```

Format (multiple servers and channels are allowed; termirc currently uses the
first of each):

```toml
[servers.osu_irc]
username = "YourName"
nickname = "YourName"
password = "your-irc-token"
server = "irc.ppy.sh"
use_tls = false
port = 6667
channels = ["#osu", "#chinese"]
```

The connection defaults to plaintext (`use_tls = false`, port 6667 — what
osu! Bancho historically uses). If your network offers TLS, prefer it so the
token does not cross the wire in clear: set `use_tls = true` and the TLS port
(the system trust store is used; certificate and hostname are verified).

## Running

```bash
cargo run
```

Run inside **Windows Terminal** (or any modern terminal) so CJK channels like
`#chinese` render correctly — legacy conhost needs `chcp 65001` plus a
CJK-capable font.

## Development

Test-driven throughout; the suite includes unit tests for every pure module
(config, message, layout, app, ui via `TestBackend`) plus integration tests
that run the real `irc` client against a local mock TCP server:

```bash
cargo test                       # 80 unit + 5 integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
