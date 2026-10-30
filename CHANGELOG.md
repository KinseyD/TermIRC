# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The setup wizard is now a fullscreen hub modeled 1:1 on the
  `/model` picker's chrome: the whole terminal becomes a
  `╭─ Server configuration ─┬─╮` frame while it is open (no chat,
  sidebar, or composer underneath), with the server list left of the
  `│` divider — `❯` cursor lane, a `●`/`+` icon lane, bold-accent
  selection, full-width band, separator rule — and the paged form on
  the right under a muted status row (`label · host`, or the pending
  validation error) and a powerline segment track: the active page is
  a filled chip in its own color, the others carry their position
  colors joined by a thin separator. Field rows carry the `❯ ` focus
  prefix instead of the old `┃` cursor, `→`/`←` switch panes (← from
  the form always returns to the sidebar), `PageUp`/`PageDown`
  turn pages from any pane, and a dim footer hint (`Enter edit · ↑/↓
  fields · ←/→ panes · PgUp/PgDn page · Esc cancel`) replaces the
  in-form footer buttons.
- Editing the hub's form is WYSIWYG: what the screen shows is the
  saved configuration. Every commit (`Enter`, or `Tab` which also
  advances) and every toggle flip writes `config.toml` immediately —
  there is no save action and no unsaved marker anymore, and the
  dialog stays open for further browsing/editing; only `Esc` (outside
  an open field) closes it. A validation failure keeps the dialog and
  the committed value in place and shows the reason on the status
  row. The footer actions pane is gone: `Tab` walks the fields and
  turns pages at a field edge, and together with `→`/`←` cycles the
  sidebar and the form. A worker respawn happens only when the label
  or a connection-affecting field (host, port, TLS, username,
  password, SASL pair, proxy) changed — a nickname or channels edit
  keeps the live connection. Renaming an entry replaces its
  `[servers.<old>]` section (proxy included) in place instead of
  appending a duplicate, preserves the file's entry order, and moves
  the worker to the new label.
- Fields edit through an explicit edit mode, like the chat composer:
  `Enter` opens the focused text field with the terminal's real
  cursor at the value's caret (the TLS toggle flips in place
  instead), typing lands only while the field is open, `Enter`
  commits, `Tab` commits and advances, and `Esc` discards — the dim
  helper row under the fields swaps to `Enter apply · Esc discard`
  meanwhile.

### Added

- In-TUI server configuration wizard: on first run (no
  `~/.config/termirc/config.toml`, or one without servers) the client
  boots to a modal setup wizard instead of exiting. The wizard is a
  two-pane form modeled after the `/model` picker style: the left
  column lists every configured server plus a `＋ new server` row, the
  right column edits the selection across three sequential pages —
  connection (name/host/port/TLS), identity (nickname, username,
  server password, SASL pair), and channels (pre-join channels and
  query nicknames). `Tab` walks the form's fields (turning a page at
  a field edge) and, with `→`/`←`, cycles the list and the form,
  `PageUp`/`PageDown` turn pages from any pane, `n` starts a new
  entry, and the panel paints over the content beneath it instead of
  letting it bleed through. Committing validates with the same rules
  `Config::parse` enforces and writes the file atomically; only
  non-server tables (`[theme]`, `[history]`, ...) still need a
  restart. The
  wizard's writer (`config::save_server`) owns every key the form
  shows — `password` and `queries` included — and preserves `proxy`
  and unrelated keys verbatim; a missing `password` key now defaults
  to empty instead of failing the parse. `/config` reopens the wizard,
  and cancelling the first-run wizard keeps the client usable: the
  welcome page shows a dim hint and `c` reopens the wizard there
  (that page has no composer to type `/config` into), while
  `Esc`/`Ctrl+C` still quit.

## [0.9.0] — 2026-10-28

### Changed

- The composer now uses the terminal's real cursor at the caret (positioned
  every frame while the composer has focus, hidden otherwise) instead of a
  simulated reverse-video block; the sidebar cursor stays a highlighted row.
- The `[theme]` roles `input_cursor_fg` and `input_cursor_bg` are deprecated
  and ignored: they are still parsed (malformed values still fail) and log a
  single `termirc::theme` warning, but no longer exist in the theme.
- Every sidebar row now opens with a 2-column pointer lane: an accent Nerd
  Font chevron (`\u{f054}`) marks the focused navigation-cursor row, while
  the active (viewed) row keeps its brighter background with its name
  accent-colored and bold.

## [0.8.0] — 2026-10-28

### Added

- Focus-aware status bar on the bottom row: the server and channel names
  plus a localized `type in` label, nested arrow-to-tail (powerline
  style), with the block matching the current focus (composer, message
  pane, or sidebar server/buffer navigation) highlighted as the last one
  shown — blocks to its right hide, as do blocks without a value (nothing
  connected, server console). While the sidebar holds focus its cursor
  row supplies the names (server header → the server block alone;
  channel/query row → that server plus the channel's name, `@nick` for
  queries), so the bar tracks the sidebar's own navigation from the
  entry frame on. Clicking a block focuses the matching sidebar row. The
  separator is the full-height powerline arrow `` (Nerd Fonts U+E0B0; a
  fallback glyph shows without one). New `statusbar.input` i18n key and
  `status_bar_highlight` theme role.
- Startup focus now rests on the server/channel sidebar (cursor on the
  first server header row) instead of the composer; `Tab` or a click moves
  focus as before.

### Fixed

- The status bar now syncs with the sidebar's focus: while the sidebar
  holds focus, its cursor row supplies the server/channel blocks, so the
  entry frame (cursor on the first server header) shows a highlighted
  server block. Previously the blocks came only from the viewed buffer,
  which left startup — and a sidebar cursor on a buffer row while a
  server console was viewed — with nothing on the bar highlighted; the
  fully dim rendering remains only when no block has a value (nothing
  connected). Clicking a server/channel block while the sidebar already
  holds focus now targets the row that block names instead of jumping to
  the viewed buffer's row.

## [0.7.0] — 2026-10-23

### Added

- Channel commands `/topic`, `/names`, `/mode` and `/whois` with numeric reply routing.
- Optional SOCKS5 proxy per server (Tor-ready), configured via `[servers.<name>] proxy`.
- Configurable color themes via the `[theme]` config section.
- Inline slash-command hint bar with Tab completion.
- Full i18n extraction: embedded locale files, a shipped `zh-CN` locale,
  localization of status messages, system notes, send/config errors, the hint
  bar and startup strings, and a `[ui] language` setting.
- CI `msrv` job pinning the minimum supported Rust version (1.88).

### Fixed

- Register configured servers at startup so every per-server console receives
  its welcome message; names and topic replies are now deferred (not parsed for
  now) instead of being misrouted.
- Declared MSRV aligned with the locked dependency set (1.88); `rust-version`
  previously claimed 1.85 while locked crates (e.g. `time` 0.3.54, `ratatui`
  0.30.2) already require 1.88.
- Deflake parallel test runs: the redacted-feedback log test no longer misses
  captured lines when sibling tests poison the shared tracing callsites, and
  the macOS mock-poll deadline in `dynamic_channels` was widened.

### Changed

- MSRV bumped from 1.85 to 1.88.

## [0.6.0] — 2026-10-22

### Added

- SQLite-backed persistent chat history configured via the `[history]`
  section; restored history obeys the same in-memory cutoffs as live buffers.

[0.9.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.9.0
[0.8.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.8.0
[0.7.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.7.0
[0.6.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.6.0
