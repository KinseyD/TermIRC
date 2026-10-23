# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Focus-aware status bar on the bottom row: `server <name> ▶ channel <name> ▶
  type in` blocks nested arrow-to-tail (powerline style), with the block matching
  the current focus (composer, message pane, or sidebar server/buffer navigation)
  highlighted; clicking a block focuses that zone, and blocks without a value
  (nothing connected, server console) hide entirely. New `statusbar.*` i18n keys
  and `status_bar_highlight` theme role.

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

[0.7.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.7.0
[0.6.0]: https://github.com/KinseyD/TermIRC/releases/tag/v0.6.0
