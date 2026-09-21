//! IRC client adapter: one controllable worker thread per configured server.
//!
//! The async `irc` crate needs a tokio runtime; we confine it to a single
//! background thread with a cheap current-thread runtime and forward events
//! to the synchronous UI loop over an `mpsc` channel. Sending goes the other
//! way through a bounded tokio channel (`OutgoingMessage`). One thread is
//! spawned per configured server, each joining all of that server's channels.

mod session;

use std::sync::mpsc;
use std::thread;

use irc::client::prelude::Config as IrcClientConfig;

use crate::config::ServerConfig;
use crate::core::{ConnectionCommand, OutgoingMessage, RoutedMessage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionState {
    Connecting,
    Connected,
    #[default]
    Stopped,
}

pub struct ConnectionHandle {
    pub outgoing: tokio::sync::mpsc::Sender<OutgoingMessage>,
    pub control: tokio::sync::mpsc::Sender<ConnectionCommand>,
}

#[derive(Clone, Copy)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub delay: std::time::Duration,
    pub timeout: std::time::Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            delay: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(30),
        }
    }
}

/// Events produced by the IRC adapter thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcEvent {
    /// Ordered barrier: the worker has consumed a manual disconnect/reconnect.
    ControlApplied(String),
    Connection(String, ConnectionState),
    Channel(String, String, ConnectionState),
    Nickname(String, String),
    PeerNickname(String, String, String),
    Away(String, bool),
    /// A routed message received from the server, a channel, or a peer.
    Message(RoutedMessage),
    /// Informational status for `server` (the config key), e.g. "connected
    /// to irc.ppy.sh".
    Status(String, String),
    /// A failed attempt for `server`; a subsequent state event indicates
    /// whether the worker is retrying or stopped.
    Error(String, String),
}

/// How many messages may wait in flight to a connection before `try_send`
/// starts failing (the UI never blocks on a send).
const OUTGOING_CAPACITY: usize = 64;

/// Map our server settings onto the irc crate's client configuration,
/// joining every channel in `channels`.
pub fn build_client_config(server: &ServerConfig, channels: &[String]) -> IrcClientConfig {
    IrcClientConfig {
        nickname: Some(server.nickname.clone()),
        username: Some(server.username.clone()),
        realname: Some(server.nickname.clone()),
        server: Some(server.server.clone()),
        port: Some(server.port),
        password: Some(server.password.clone()),
        use_tls: Some(server.use_tls),
        channels: channels.to_vec(),
        ..Default::default()
    }
}

/// Start one controllable worker for the lifetime of a configured server.
/// Dropping the handle cancels pending attempts and closes the active socket.
pub fn spawn_irc(
    server: ServerConfig,
    server_label: String,
    channels: Vec<String>,
    tx: mpsc::Sender<IrcEvent>,
) -> (thread::JoinHandle<()>, ConnectionHandle) {
    spawn_irc_with_policy(server, server_label, channels, tx, RetryPolicy::default())
}

/// Start a worker with an explicit retry and timeout policy.
pub fn spawn_irc_with_policy(
    server: ServerConfig,
    label: String,
    channels: Vec<String>,
    tx: mpsc::Sender<IrcEvent>,
    policy: RetryPolicy,
) -> (thread::JoinHandle<()>, ConnectionHandle) {
    let (outgoing, out_rx) = tokio::sync::mpsc::channel(OUTGOING_CAPACITY);
    let (control, control_rx) = tokio::sync::mpsc::channel(16);
    let handle = thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(session::run(
                server, label, channels, tx, policy, out_rx, control_rx,
            )),
            Err(_) => {
                let _ = tx.send(IrcEvent::Error(label.clone(), "runtime init failed".into()));
                let _ = tx.send(IrcEvent::Connection(label, ConnectionState::Stopped));
            }
        }
    });
    (handle, ConnectionHandle { outgoing, control })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_config() -> ServerConfig {
        ServerConfig {
            username: "alice".to_string(),
            nickname: "alice_".to_string(),
            password: "secret".to_string(),
            server: "irc.example.org".to_string(),
            use_tls: false,
            port: 6667,
            channels: vec!["#osu".to_string(), "#chinese".to_string()],
            queries: vec![],
        }
    }

    #[test]
    fn maps_server_fields_onto_irc_config() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server, &server.channels);

        // Assert
        assert_eq!(client_config.server.as_deref(), Some("irc.example.org"));
        assert_eq!(client_config.port, Some(6667));
        assert_eq!(client_config.nickname.as_deref(), Some("alice_"));
        assert_eq!(client_config.username.as_deref(), Some("alice"));
        assert_eq!(client_config.password.as_deref(), Some("secret"));
        assert_eq!(client_config.use_tls, Some(false));
    }

    #[test]
    fn joins_every_configured_channel() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server, &server.channels);

        // Assert: both channels are joined, in config order.
        assert_eq!(client_config.channels, server.channels);
    }

    #[test]
    fn realname_defaults_to_nickname() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server, &server.channels);

        // Assert
        assert_eq!(client_config.realname.as_deref(), Some("alice_"));
    }
}
