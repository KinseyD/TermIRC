# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.8.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.8.0
[0.7.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.7.0
[0.6.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.6.0
