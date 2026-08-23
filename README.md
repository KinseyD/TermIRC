# termirc

A terminal TUI IRC client written in Rust.

It reads your server list from a TOML config file, opens one connection per
server (joining all of its channels), and displays incoming chat messages in a
scrollable pane beside a server/channel sidebar. Type in the composer and
press `Enter` to send to the viewed channel.

## Layout

```text
 osu_irc          │
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
  viewed channel is highlighted.
- **Message pane**: each message spans the pane; the nick and body each take
  their own horizontal space, and wrapped body lines are indented to the body
  column (never under the nick). Exactly one blank line separates two messages,
  and the list is framed by one more blank separator row above the first and
  below the last message (these scroll with the content; there is no title
  row and no static spacer above the composer). On startup a **welcome page**
  takes the whole main column - just the centered termirc logo, no composer -
  until you open a channel from the sidebar; focus starts on the sidebar.
- **Composer** (bottom): a shaded input box with a pale-green `┃` accent on its
  left. It keeps a 3-column gap from the screen's right edge and a 2-column
  margin inside the box. When the typed text exceeds one line it wraps, the
  composer grows taller, and the message pane shrinks to match.

## Keys

Focus cycles between the three panes with `Tab`:

| Key | Sidebar focused | Messages focused | Composer focused |
|-----|-----------------|------------------|------------------|
| `j` / `k` | move the cursor down / up | select the next / previous message | type `j` / `k` |
| `Enter` | collapse/expand a server row, or switch to the channel under the cursor | (no-op) | send the text to the viewed channel |
| printable chars | — | — | type into the composer |
| `Backspace`/`Delete`/arrows | — | — | edit the composer input |
| `PgUp` / `PgDn` | scroll the message pane by ⅓ of its height | same | same |
| `Esc` / `Ctrl+C` | quit | quit | quit |

While the sidebar has focus its cursor row is slightly highlighted (with a
block cursor) and the viewed channel row is brighter; moving focus into the
sidebar snaps the cursor onto the viewed channel's row (or the first row when
the welcome page is up). `Enter` on a channel switches the message pane and
returns focus to the composer. While the message pane has focus one message
is always selected: its rows are highlighted, the separator rows above/below
(including the framing rows at the very top and bottom of the list) render as
half blocks, and a pale green `┃` accent - tapered at both ends - marks its
front edge; `j`/`k` move the selection (auto scrolling minimally to reveal
it) and incoming messages do not disturb the view. The composer's accent dims
while another pane has focus.

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
