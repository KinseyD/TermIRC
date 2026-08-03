# termirc

A receive-only terminal TUI IRC client written in Rust.

It reads your server list from a TOML config file, connects to the **first
server's first channel**, and displays incoming chat messages in a scrollable
full-width pane. Sending messages is intentionally not implemented (yet).

## Message layout

Each message spans the full window width. The nick and the body each take
their own horizontal space; wrapped body lines are indented to the body
column and never run underneath the nick:

```text
username:  messages messages
                  messages .....
```

There is exactly one blank line between two messages (never a trailing one).

## Scrolling

| Key | Action |
|-----|--------|
| `PgUp` | scroll up by ⅓ of the window height |
| `PgDn` | scroll down by ⅓ of the window height |
| `Esc` / `Ctrl+C` | quit (typing `q` inserts `q` into the composer) |

Connection status is shown on the pane's bottom border. If the server drops
the connection, the status changes to `disconnected from …` (or an error
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
cargo test                       # 44 unit + 3 integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
