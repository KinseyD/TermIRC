//! termirc: a receive-only terminal IRC client.
//!
//! The library holds all logic (config, IRC adapter, layout, app state, UI)
//! so it can be exercised by both unit tests and integration tests; the binary
//! in `main.rs` is a thin wiring layer.

pub mod app;
pub mod config;
pub mod irc;
pub mod layout;
pub mod message;
pub mod ui;
