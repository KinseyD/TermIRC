# TermIRC

[![CI](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)](https://github.com/KinseyD/TermIRC/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-DEA584?logo=rust)](https://www.rust-lang.org/)

A terminal IRC client written in Rust. One connection per configured
server, one config file, an optional flag for read-only monitoring,
a bidirectional `--stdio` mode for automation, and a one-shot `send`
subcommand for scripts.

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

   Pass `--config <PATH>` (short `-c`) to start with a different
   configuration file; logs always stay under `~/.config/termirc/logs/`.

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

## Listen mode

Run headless with `--listen` (combine with `-c <PATH>` for another
config file):

```bash
termirc --listen
```

TermIRC connects to every configured server, joins the configured
channels, and prints incoming chat to stdout — one line per event,
timestamps always UTC `HH:MM:SS`:

```
[14:32:05][libera/#termirc][alice] hello
[14:32:41][libera/#termirc][bob] * waves
[14:33:02][libera][connected]
```

- **Read-only**: nothing is ever sent — no messages, no replies. The
  configured JOINs still happen (that is what makes channel traffic
  visible); no terminal setup, no stdin.
- Nothing is written to the history database, even with
  `[history] persist = true`.
- Chat, actions (`* text`), query messages, and connection status
  lines are printed; console chatter (MOTD, numerics) is not.
- Ctrl+C terminates. `--listen` with no configured servers is an error
  (there is no setup wizard to fall back on headlessly).

## Send mode

Run a one-shot send headlessly: connect to one server, deliver a single
message, and exit — handy from scripts and CI:

```bash
termirc send libera/#termirc "deploy done"
termirc send libera/alice "hi"
echo "build 42 passed" | termirc send libera/#ci -
termirc -c ~/.config/termirc/ci.toml send libera/#ci "done"
```

- `SERVER/TARGET` is the `[servers.<name>]` key (case-insensitive), a
  `/`, then a `#channel` or a nickname. `--config`/`-c` works before or
  after `send`. It may also be just a server name, in which case
  MESSAGE is one raw IRC command line sent to that server, e.g.
  `termirc send libera "WHOIS alice"` or
  `termirc send libera "MODE #chan +o alice"`. Replies are not printed
  by `send` — it only reports whether the command went out; use
  `--stdio` when you need to see the replies.
- A message of `-` reads the text from stdin (one trailing newline is
  stripped).
- Only the named server is connected, and only the target channel is
  joined.
- Text is sent verbatim — no slash-command parsing, no line splitting.
  Empty text, control characters, or more than 512 bytes are rejected
  before connecting.
- Exactly one PRIVMSG is sent per run, and nothing is written to the
  history database.
- Success is silent with exit code 0. Exit code 2 means the message was
  locally rejected (bad arguments, unknown server, invalid text); exit
  code 3 means it was not sent (connecting or the `JOIN` failed or
  timed out, or the message was dropped). Note that `JOIN` must be
  spelled `JOIN`.
- "Sent" means handed to the connection, not confirmed by the server.
- A channel target shows up as a JOIN followed by a QUIT on the
  network.
- `--listen` and `send` cannot be combined.
- A nickname already connected from a running TermIRC is refused by
  the server (433) — add a second `[servers.<name>]` entry with another
  nickname for scripts.

## Stdio mode

Run fully headless and bidirectional with `--stdio` (`-c <PATH>` works
too): TermIRC connects to every configured server and joins the
configured channels like `--listen`, then reads commands from stdin
and prints traffic to stdout until stdin is closed.

```bash
termirc --stdio
```

Input is one command per line; the text before the first space is the
conversation, everything after it is sent verbatim:

```
libera/#termirc deploy done
libera/alice hi
libera WHOIS alice
libera JOIN #newchan
```

A message to a channel, a message to a nickname, a raw command to the
server, and joining a channel that is not in the config — its traffic
is then printed too. Blank lines are ignored. Raw commands have no
leading slash.

Output is one line per event, UTC `HH:MM:SS`:

```
[14:32:05][libera/#termirc][alice] hello
[14:32:41][libera/#termirc][bob] * waves
[14:33:02][libera] connected
[14:33:10][libera] 311 alice is alice@example.com ...
[14:33:10][libera] error 401: bob :No such nick
```

Chat and actions look exactly like listen mode. Numeric replies
(WHOIS, NAMES, TOPIC ...), server errors, and local notices (join
timeouts, kicks ...) are printed as `[time][server] text` (or
`[time][server/#channel] text` when they belong to a channel). MOTD
and other console chatter are not printed.

- A line waits until its server is connected and, for a channel, the
  channel is joined, so piped input written immediately still goes out
  once the connection is ready. A line is refused with a message on
  stderr when the server stops trying or the channel is not joined.
  Refused lines make the exit code 3 when stdin has already closed, 0
  otherwise. stdout carries traffic only; errors go to stderr.
- Closing stdin ends the session (QUIT is sent; exit code 0, or 3 when
  a refused line went undelivered); Ctrl+C also works. `--stdio`
  cannot be combined with `--listen` or with `send`.
- Nothing is written to the history database. A nickname already
  connected from a running TermIRC (or `--listen`) is refused by the
  server.

For simple automation:

```bash
printf 'libera/#ci deploy done\n'; sleep 2 | termirc --stdio
```

A driving program can use a coprocess (`subprocess.Popen`, `coproc`)
to keep it running, write lines to its stdin and read lines from its
stdout.

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
