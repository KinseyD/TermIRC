//! termirc: a terminal IRC client.
//!
//! Core identities and histories are independent of the terminal frontend.
//! Protocol conversion, connection workers, application coordination, and TUI
//! presentation form separate boundaries; main only assembles resources.

pub mod command;
pub mod config;
pub mod connection;
pub mod logging;

pub mod application;
pub mod core;
pub mod history;
pub mod protocol;
pub mod tui;
