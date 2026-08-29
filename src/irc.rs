//! IRC client adapter: runs the async client on a dedicated thread.
//!
//! The async `irc` crate needs a tokio runtime; we confine it to a single
//! background thread with a cheap current-thread runtime and forward events
//! to the synchronous UI loop over an `mpsc` channel. Sending goes the other
//! way through a bounded tokio channel (`OutgoingMessage`). One thread is
//! spawned per configured server, each joining all of that server's channels.

use std::sync::mpsc;
use std::thread;

use futures_util::StreamExt;
use irc::client::prelude::{Client, Command, Config as IrcClientConfig};

use crate::config::ServerConfig;
use crate::message::ChatMessage;

/// Events produced by the IRC adapter thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrcEvent {
    /// A chat message received from one of the joined channels.
    Message(ChatMessage),
    /// Informational status for `server` (the config key), e.g. "connected
    /// to irc.ppy.sh".
    Status(String, String),
    /// A connection or protocol error for `server` (the config key); the
    /// thread stops afterwards.
    Error(String, String),
}

/// A message the user wants to send through one of our connections.
///
/// `server` is the config key used by the UI to pick the right connection.
/// `Privmsg` sends `text` to `target` (a channel); `Raw` sends `line` to the
/// server itself as a raw IRC command (e.g. `JOIN #foo`, `WHOIS nick`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutgoingMessage {
    Privmsg {
        server: String,
        target: String,
        text: String,
    },
    Raw {
        server: String,
        line: String,
    },
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

/// Spawn the IRC client thread for one server; events arrive on `tx`.
///
/// `server_label` is the config key identifying the server in events and
/// statuses. The thread connects, registers, joins all channels, forwards
/// chat messages, and sends whatever the returned `Sender` receives until
/// the connection ends. A terminal event is ALWAYS emitted on exit — `Status`
/// for a clean close, `Error` otherwise — so the UI never keeps claiming
/// "connected" to a dead feed.
pub fn spawn_irc(
    server: ServerConfig,
    server_label: String,
    channels: Vec<String>,
    tx: mpsc::Sender<IrcEvent>,
) -> (
    thread::JoinHandle<()>,
    tokio::sync::mpsc::Sender<OutgoingMessage>,
) {
    let (out_tx, out_rx) = tokio::sync::mpsc::channel(OUTGOING_CAPACITY);
    let handle = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                tracing::error!("{server_label}: runtime init failed: {e}");
                let _ = tx.send(IrcEvent::Error(
                    server_label.clone(),
                    format!("runtime init failed: {e}"),
                ));
                return;
            }
        };

        let host = server.server.clone();
        let label = server_label.clone();
        let result = runtime.block_on(run_client(server, &server_label, &channels, &tx, out_rx));
        // `send` failing means the receiver is gone — the UI has quit and the
        // process is about to reap this thread; nothing to report anywhere.
        match result {
            Ok(()) => {
                tracing::info!("{label}: disconnected from {host} (connection closed)");
                let _ = tx.send(IrcEvent::Status(
                    label.clone(),
                    format!("disconnected from {host} (connection closed)"),
                ));
            }
            Err(e) => {
                tracing::error!("{label}: {e}");
                let _ = tx.send(IrcEvent::Error(label.clone(), format!("{e}")));
            }
        }
    });
    (handle, out_tx)
}

async fn run_client(
    server: ServerConfig,
    server_label: &str,
    channels: &[String],
    tx: &mpsc::Sender<IrcEvent>,
    mut out_rx: tokio::sync::mpsc::Receiver<OutgoingMessage>,
) -> anyhow::Result<()> {
    let mut client = Client::from_config(build_client_config(&server, channels)).await?;
    client.identify()?;
    let mut stream = client.stream()?;
    let _ = tx.send(IrcEvent::Status(
        server_label.to_string(),
        format!("connected to {}", server.server),
    ));
    tracing::info!("{server_label}: connected to {}", server.server);

    loop {
        tokio::select! {
            outgoing = out_rx.recv() => {
                match outgoing {
                    // The UI is shutting down (all senders dropped): stop.
                    None => return Ok(()),
                    Some(message) => match message {
                        OutgoingMessage::Privmsg { target, text, .. } => {
                            client.send_privmsg(&target, &text)?
                        }
                        OutgoingMessage::Raw { line, .. } => {
                            let mut parts = line.split_whitespace();
                            let cmd = parts.next().unwrap_or_default().to_string();
                            let args: Vec<String> =
                                parts.map(str::to_string).collect();
                            client.send(Command::Raw(cmd, args))?;
                        }
                    },
                }
            }
            item = stream.next() => {
                match item {
                    Some(Ok(message)) => {
                        if let Some(chat) =
                            ChatMessage::from_proto(&message, server_label, channels)
                        {
                            let _ = tx.send(IrcEvent::Message(chat));
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
        }
    }
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
