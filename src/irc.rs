//! IRC client adapter: runs the async client on a dedicated thread.
//!
//! The async `irc` crate needs a tokio runtime; we confine it to a single
//! background thread with a cheap current-thread runtime and forward events
//! to the synchronous UI loop over an `mpsc` channel.

use std::sync::mpsc;
use std::thread;

use futures_util::StreamExt;
use irc::client::prelude::{Client, Config as IrcClientConfig};

use crate::config::ServerConfig;
use crate::message::ChatMessage;

/// Events produced by the IRC adapter thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcEvent {
    /// A chat message received from the channel.
    Message(ChatMessage),
    /// Informational status (e.g. "connected to ...").
    Status(String),
    /// A connection or protocol error; the thread stops afterwards.
    Error(String),
}

/// Map our server settings onto the irc crate's client configuration.
pub fn build_client_config(server: &ServerConfig, channel: &str) -> IrcClientConfig {
    IrcClientConfig {
        nickname: Some(server.nickname.clone()),
        username: Some(server.username.clone()),
        realname: Some(server.nickname.clone()),
        server: Some(server.server.clone()),
        port: Some(server.port),
        password: Some(server.password.clone()),
        use_tls: Some(server.use_tls),
        channels: vec![channel.to_string()],
        ..Default::default()
    }
}

/// Spawn the IRC client thread; events arrive on `tx`.
///
/// The thread connects, registers, joins the channel, and forwards chat
/// messages until the connection ends. A terminal event is ALWAYS emitted on
/// exit — `Status` for a clean close, `Error` otherwise — so the UI never
/// keeps claiming "connected" to a dead feed.
pub fn spawn_irc(
    server: ServerConfig,
    channel: String,
    tx: mpsc::Sender<IrcEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                let _ = tx.send(IrcEvent::Error(format!("runtime init failed: {e}")));
                return;
            }
        };

        let server_name = server.server.clone();
        let result = runtime.block_on(run_client(server, channel, &tx));
        // `send` failing means the receiver is gone — the UI has quit and the
        // process is about to reap this thread; nothing to report anywhere.
        match result {
            Ok(()) => {
                let _ = tx.send(IrcEvent::Status(format!(
                    "disconnected from {server_name} (connection closed)"
                )));
            }
            Err(e) => {
                let _ = tx.send(IrcEvent::Error(e.to_string()));
            }
        }
    })
}

async fn run_client(
    server: ServerConfig,
    channel: String,
    tx: &mpsc::Sender<IrcEvent>,
) -> anyhow::Result<()> {
    let mut client = Client::from_config(build_client_config(&server, &channel)).await?;
    client.identify()?;
    let mut stream = client.stream()?;
    let _ = tx.send(IrcEvent::Status(format!("connected to {}", server.server)));

    while let Some(result) = stream.next().await {
        match result {
            Ok(message) => {
                if let Some(chat) = ChatMessage::from_proto(&message, &channel) {
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
        let client_config = build_client_config(&server, "#osu");

        // Assert
        assert_eq!(client_config.server.as_deref(), Some("irc.example.org"));
        assert_eq!(client_config.port, Some(6667));
        assert_eq!(client_config.nickname.as_deref(), Some("alice_"));
        assert_eq!(client_config.username.as_deref(), Some("alice"));
        assert_eq!(client_config.password.as_deref(), Some("secret"));
        assert_eq!(client_config.use_tls, Some(false));
    }

    #[test]
    fn channels_contains_exactly_the_target_channel() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server, "#osu");

        // Assert: only the one channel we actually connect to.
        assert_eq!(client_config.channels, vec!["#osu".to_string()]);
    }

    #[test]
    fn realname_defaults_to_nickname() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server, "#osu");

        // Assert
        assert_eq!(client_config.realname.as_deref(), Some("alice_"));
    }
}
