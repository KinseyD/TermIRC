# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

termirc is a **receive-only** terminal TUI IRC client in Rust (edition 2024). It reads servers from a TOML config, connects to the **first server's first channel**, and renders incoming chat in a scrollable pane. Sending messages is intentionally not implemented.

## Commands

```bash
cargo run                                      # run (needs ~/.config/termirc/config.toml)
cargo test                                     # unit + integration tests
cargo test <name_substring>                    # one test (matches against test names)
cargo test --test mock_irc_integration         # integration tests only
cargo clippy --all-targets -- -D warnings      # lint; warnings are errors
cargo fmt --check                              # format check (run `cargo fmt` to fix)
```

The app requires a config at `~/.config/termirc/config.toml`; `test.toml` in the repo root is the local sample — copy it there to run.

## Architecture

A library (`src/lib.rs` re-exports `app`, `config`, `irc`, `layout`, `message`, `ui`) plus a thin binary (`src/main.rs`) that wires them together. All logic lives in the library so unit and integration tests can reach it.

### Two threads, one channel — the central design

- **Main thread** runs a synchronous ratatui event loop. It blocks on `event::poll(POLL_INTERVAL)` (50 ms), handles one key/resize event, then drains IRC events with `rx.try_iter()`. Chat only appears when the poll returns — this is the ~50 ms latency ceiling on message display.
- **IRC thread** (spawned by `spawn_irc` in [src/irc.rs](src/irc.rs)) owns a dedicated `current_thread` tokio runtime and runs the async `irc` crate client. It is deliberately **not joined** — it blocks on network I/O and is reaped at process exit.
- The bridge is a `std::sync::mpsc` channel carrying [`IrcEvent`](src/irc.rs) (`Message` / `Status` / `Error`). The IRC thread always emits a terminal event before stopping (`Status("disconnected …")` on a clean close, `Error` otherwise), so the UI never shows "connected" to a dead feed. **There is no auto-reconnect** — quit and restart.

Constraint to respect when touching this boundary: the async `irc` crate cannot run inside the sync UI loop, and ratatui must stay on the main thread. The single-thread tokio runtime stays confined to the IRC thread.

### Pre-wrapped layout, not ratatui `Wrap`

[src/layout.rs](src/layout.rs) turns each `ChatMessage` into one or more `LayoutLine`s: a nick column (`nick: `), a body column, and one blank separator between messages (never trailing). Continuation lines are indented to the body column so wrapped text never runs under the nick. [src/ui.rs](src/ui.rs) renders these as a `Paragraph` **without** `Wrap`, borrowing row bodies to keep per-frame allocations flat. Widths are measured with `unicode-width`; a 2-column (CJK/full-width) char that doesn't fit moves to the next line rather than overflowing.

### Scroll model and the `u16` cap

[src/app.rs](src/app.rs) holds the message list and the laid-out rows. Auto-scroll rule: a new message moves the viewport only when its last line was already visible (`is_at_bottom`). `was_at_bottom` is captured **before** mutating; `relayout` then either snaps to `max_offset` (follow) or clamps the existing offset. Because ratatui's scroll offset is a `u16`, the row count must stay below `u16::MAX` — enforced by `MAX_LINES` (50_000), which evicts the oldest messages. `MAX_MESSAGES` (5_000) is a separate cap on stored messages.

### Message filtering

[`ChatMessage::from_proto`](src/message.rs) accepts **only** channel PRIVMSGs targeted at the viewed channel (case-insensitive), with a real user nick as source. It drops DMs to our own nick, other channels, NOTICEs, server/numeric messages, and non-ACTION CTCP. `/me` actions (`\x01ACTION …\x01`) render as `* text`.

## Conventions and gotchas

- **Only the first server's first channel is used.** Multi-server/channel support exists in config parsing only; the rest is single-channel.
- **Config file order matters.** `Config::servers` is an `IndexMap` (with toml `preserve_order`), so `first_server()` returns the first server *written*, not alphabetical.
- **Credentials.** `ServerConfig` has a manual `Debug` impl that redacts `password`. `test.toml` holds a real-looking osu! IRC token and is in `.gitignore` — never commit it.
- **Terminal safety.** `main.rs` installs a panic hook that restores the terminal before the backtrace prints, so a panic never strands the shell in raw mode / the alternate screen.
- **Platform.** Developed on Windows. Key handling filters to `KeyEventKind::Press` (Windows also emits Release/Repeat) and treats `Ctrl+C` as a key event because raw mode disables SIGINT. Use Windows Terminal (or `chcp 65001` + a CJK font) for CJK channels like `#chinese`.
- **TDD throughout.** Tests follow Arrange/Act/Assert with those comments. UI is tested headlessly via ratatui's `TestBackend` — the terminal is 2 wider and 2 taller than the app viewport because of the `BORDER = 2` block. Integration tests in [tests/mock_irc_integration.rs](tests/mock_irc_integration.rs) spin up a real mock TCP IRC server on an ephemeral port and drive the actual `irc` client through connect → identify → join → receive → PING/PONG.
