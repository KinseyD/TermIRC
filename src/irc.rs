//! IRC client adapter: runs the async client on a dedicated thread.
//!
//! The async `irc` crate needs a tokio runtime; we confine it to a single
//! background thread with a cheap current-thread runtime and forward events
//! to the synchronous UI loop over an `mpsc` channel. One thread is spawned
//! per configured server, each joining all of that server's channels.

use std::sync::mpsc;
use std::thread;

use futures_util::StreamExt;
use irc::client::prelude::{Client, Config as IrcClientConfig};

use crate::config::ServerConfig;
use crate::message::ChatMessage;

/// Events produced by the IRC adapter thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcEvent {
    /// A chat message received from one of the joined channels.
    Message(ChatMessage),
    /// Informational status (e.g. "osu_irc: connected to irc.ppy.sh").
    Status(String),
    /// A connection or protocol error; the thread stops afterwards.
    Error(String),
}

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

/// Spawn the IRC client thread for one server; events arrive on `tx`.
///
/// `server_label` is the config key identifying the server in events and
/// statuses. The thread connects, registers, joins all channels, and forwards
/// chat messages until the connection ends. A terminal event is ALWAYS emitted
/// on exit — `Status` for a clean close, `Error` otherwise — so the UI never
/// keeps claiming "connected" to a dead feed.
pub fn spawn_irc(
    server: ServerConfig,
    server_label: String,
    channels: Vec<String>,
    tx: mpsc::Sender<IrcEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                let _ = tx.send(IrcEvent::Error(format!(
                    "{server_label}: runtime init failed: {e}"
                )));
                return;
            }
        };

        let host = server.server.clone();
        let label = server_label.clone();
        let result = runtime.block_on(run_client(server, &server_label, &channels, &tx));
        // `send` failing means the receiver is gone — the UI has quit and the
        // process is about to reap this thread; nothing to report anywhere.
        match result {
            Ok(()) => {
                let _ = tx.send(IrcEvent::Status(format!(
                    "{label}: disconnected from {host} (connection closed)"
                )));
            }
            Err(e) => {
                let _ = tx.send(IrcEvent::Error(format!("{label}: {e}")));
            }
        }
    })
}

async fn run_client(
    server: ServerConfig,
    server_label: &str,
    channels: &[String],
    tx: &mpsc::Sender<IrcEvent>,
) -> anyhow::Result<()> {
    let mut client = Client::from_config(build_client_config(&server, channels)).await?;
    client.identify()?;
    let mut stream = client.stream()?;
    let _ = tx.send(IrcEvent::Status(format!(
        "{server_label}: connected to {}",
        server.server
    )));

    while let Some(result) = stream.next().await {
        match result {
            Ok(message) => {
                if let Some(chat) = ChatMessage::from_proto(&message, server_label, channels) {
                    let _ = tx.send(IrcEvent::Message(chat));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
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
