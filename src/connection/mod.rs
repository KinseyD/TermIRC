//! IRC client adapter: one controllable worker thread per configured server.
//!
//! The async `irc` crate needs a tokio runtime; we confine it to a single
//! background thread with a cheap current-thread runtime and forward events
//! to the synchronous UI loop over an `mpsc` channel. Sending goes the other
//! way through a bounded tokio channel (`OutgoingMessage`). One thread is
//! spawned per configured server, each joining all of that server's channels.

mod channels;
mod sasl;
mod session;

use std::sync::mpsc;
use std::thread;

use irc::client::data::ProxyType as IrcProxyType;
use irc::client::prelude::Config as IrcClientConfig;

use crate::config::ServerConfig;
use crate::core::{ChannelStatus, ConnectionCommand, OutgoingMessage, RoutedMessage};

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
    Channel(String, String, ChannelStatus),
    ChannelControlApplied(String, String),
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
    /// The worker handed a queued message to the IRC crate (`client.send`
    /// returned `Ok`). Every queued message that is not a raw JOIN/PART
    /// control yields exactly one of this or `OutgoingDropped`, so a caller
    /// can wait for a verdict instead of assuming delivery. The string is
    /// the server label; the message text never rides on events.
    OutgoingAccepted(String),
    /// The worker discarded a queued message without sending it (arrived
    /// before registration, an unjoined channel, or a failed encode).
    OutgoingDropped(String),
}

/// How many messages may wait in flight to a connection before `try_send`
/// starts failing (the UI never blocks on a send).
const OUTGOING_CAPACITY: usize = 64;

/// Map transport settings without enabling the IRC library's automatic JOIN.
pub fn build_client_config(server: &ServerConfig) -> IrcClientConfig {
    IrcClientConfig {
        nickname: Some(server.nickname.clone()),
        username: Some(server.username.clone()),
        realname: Some(server.nickname.clone()),
        server: Some(server.server.clone()),
        port: Some(server.port),
        password: Some(server.password.clone()),
        use_tls: Some(server.use_tls),
        channels: Vec::new(),
        // SOCKS5 via tokio-socks; the irc crate resolves the IRC host through
        // the proxy too, so DNS never leaves the tunnel (Tor-friendly).
        proxy_type: server.proxy.as_ref().map(|_| IrcProxyType::Socks5),
        proxy_server: server.proxy.as_ref().map(|proxy| proxy.host.clone()),
        proxy_port: server.proxy.as_ref().map(|proxy| proxy.port),
        proxy_username: server
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.username.clone()),
        proxy_password: server
            .proxy
            .as_ref()
            .and_then(|proxy| proxy.password.clone()),
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    use crate::core::ChannelState;

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
            sasl_username: None,
            sasl_password: None,
            proxy: None,
        }
    }

    #[test]
    fn maps_server_fields_onto_irc_config() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server);

        // Assert
        assert_eq!(client_config.server.as_deref(), Some("irc.example.org"));
        assert_eq!(client_config.port, Some(6667));
        assert_eq!(client_config.nickname.as_deref(), Some("alice_"));
        assert_eq!(client_config.username.as_deref(), Some("alice"));
        assert_eq!(client_config.password.as_deref(), Some("secret"));
        assert_eq!(client_config.use_tls, Some(false));
    }

    #[test]
    fn leaves_automatic_join_to_the_worker() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server);

        assert!(client_config.channels.is_empty());
    }

    #[test]
    fn realname_defaults_to_nickname() {
        // Arrange
        let server = server_config();

        // Act
        let client_config = build_client_config(&server);

        // Assert
        assert_eq!(client_config.realname.as_deref(), Some("alice_"));
    }

    #[test]
    fn maps_proxy_fields_onto_irc_config_when_configured() {
        // Arrange
        let mut server = server_config();
        server.proxy = Some(crate::config::ProxyConfig {
            r#type: "socks5".into(),
            host: "127.0.0.1".into(),
            port: 9050,
            username: Some("tor".into()),
            password: Some("sesame".into()),
        });

        // Act
        let client_config = build_client_config(&server);

        // Assert
        assert_eq!(client_config.proxy_type, Some(IrcProxyType::Socks5));
        assert_eq!(client_config.proxy_server.as_deref(), Some("127.0.0.1"));
        assert_eq!(client_config.proxy_port, Some(9050));
        assert_eq!(client_config.proxy_username.as_deref(), Some("tor"));
        assert_eq!(client_config.proxy_password.as_deref(), Some("sesame"));
    }

    #[test]
    fn leaves_proxy_unset_when_unconfigured() {
        // Arrange: no proxy table in the config.

        // Act
        let client_config = build_client_config(&server_config());

        // Assert
        assert_eq!(client_config.proxy_type, None);
        assert_eq!(client_config.proxy_server, None);
        assert_eq!(client_config.proxy_port, None);
    }

    /// How long a verdict may take to arrive before the test gives up.
    const WAIT: Duration = Duration::from_secs(5);
    /// Wire silence that means a line which should not exist is absent.
    const QUIET: Duration = Duration::from_millis(300);

    /// The timing the verdict tests run under: no retries and no delay
    /// worth waiting for.
    fn one_shot() -> RetryPolicy {
        RetryPolicy {
            max_retries: 0,
            delay: Duration::from_millis(30),
            timeout: WAIT,
        }
    }

    /// The test server, pointed at a loopback port instead of its example host.
    fn local_server(port: u16) -> ServerConfig {
        let mut server = server_config();
        server.server = "127.0.0.1".to_string();
        server.port = port;
        server
    }

    /// A mock that greets on `USER`, streams every client line to the
    /// returned receiver, and — only when `confirm_join` — echoes a JOIN
    /// back so the channel becomes sendable.
    fn spawn_mock(confirm_join: bool) -> (u16, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (lines_tx, lines_rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok((socket, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut writer = socket;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let trimmed = line.trim_end().to_string();
                line.clear();
                if lines_tx.send(trimmed.clone()).is_err() {
                    break;
                }
                if trimmed.starts_with("USER") {
                    let _ = write!(
                        writer,
                        ":mock 001 test :Welcome to the Mock IRC Network\r\n"
                    );
                }
                if confirm_join && let Some(channel) = trimmed.strip_prefix("JOIN ") {
                    let _ = write!(writer, ":test!u@h JOIN {channel}\r\n");
                }
            }
        });
        (port, lines_rx)
    }

    /// Collect events until one satisfies `done`, panicking with everything
    /// seen so far when none does, so a missing verdict cannot pass as an
    /// unobserved one.
    fn events_until(
        rx: &mpsc::Receiver<IrcEvent>,
        done: impl Fn(&IrcEvent) -> bool,
    ) -> Vec<IrcEvent> {
        let deadline = Instant::now() + WAIT;
        let mut seen = Vec::new();
        loop {
            let event = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("no verdict arrived: {error}; events: {seen:?}"));
            let matched = done(&event);
            seen.push(event);
            if matched {
                return seen;
            }
        }
    }

    /// Every line the mock has seen, once the wire has been quiet for
    /// [`QUIET`].
    fn wire_lines(lines: &mpsc::Receiver<String>) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(line) = lines.recv_timeout(QUIET) {
            seen.push(line);
        }
        seen
    }

    /// Only the outgoing verdicts among `events`, in order: what a sender
    /// waiting on a queued message sees once the session's own traffic
    /// (state changes, the welcome burst) is ignored.
    fn verdicts(events: &[IrcEvent]) -> Vec<IrcEvent> {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    IrcEvent::OutgoingAccepted(_) | IrcEvent::OutgoingDropped(_)
                )
            })
            .cloned()
            .collect()
    }

    fn privmsg(target: &str, text: &str) -> OutgoingMessage {
        OutgoingMessage::Privmsg {
            server: "srv".to_string(),
            target: target.to_string(),
            text: text.to_string(),
        }
    }

    fn connected(event: &IrcEvent) -> bool {
        matches!(event, IrcEvent::Connection(_, ConnectionState::Connected))
    }

    fn joined(event: &IrcEvent) -> bool {
        matches!(
            event,
            IrcEvent::Channel(_, channel, ChannelStatus { state: ChannelState::Joined, .. })
                if channel == "#test"
        )
    }

    #[test]
    fn a_privmsg_to_an_unjoined_channel_is_reported_dropped_and_never_reaches_the_wire() {
        // Arrange: the server never confirms the JOIN, so the channel stays
        // `Joining` and is not sendable.
        let (port, lines_rx) = spawn_mock(false);
        let (tx, rx) = mpsc::channel();
        let server = local_server(port);
        let (_worker, handle) = spawn_irc_with_policy(
            server,
            "srv".to_string(),
            vec!["#test".to_string()],
            tx,
            one_shot(),
        );

        // Act: queue once the connection is registered, exactly as a sender
        // that waits for `Connected` would.
        events_until(&rx, connected);
        handle.outgoing.try_send(privmsg("#test", "hello")).unwrap();
        let seen = events_until(&rx, |event| {
            matches!(
                event,
                IrcEvent::OutgoingDropped(_) | IrcEvent::OutgoingAccepted(_)
            )
        });

        // Assert: the channel is still joining, exactly one verdict follows
        // and it is the drop, and nothing reached the wire.
        assert!(
            seen.iter().any(|event| matches!(
                event,
                IrcEvent::Channel(_, channel, ChannelStatus { state: ChannelState::Joining, .. })
                    if channel == "#test"
            )),
            "events: {seen:?}"
        );
        assert_eq!(
            verdicts(&seen),
            vec![IrcEvent::OutgoingDropped("srv".to_string())]
        );
        let lines = wire_lines(&lines_rx);
        assert!(
            lines.iter().any(|line| line == "JOIN #test"),
            "the JOIN must still be sent: {lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.starts_with("PRIVMSG")),
            "a dropped message reached the wire: {lines:?}"
        );
    }

    #[test]
    fn a_privmsg_queued_before_registration_is_reported_dropped() {
        // Arrange
        let (port, lines_rx) = spawn_mock(false);
        let (tx, rx) = mpsc::channel();
        let server = local_server(port);
        let (_worker, handle) = spawn_irc_with_policy(
            server,
            "srv".to_string(),
            vec!["#test".to_string()],
            tx,
            one_shot(),
        );

        // Act: queue before the worker can have registered, so one of the
        // pre-registration drains takes the message.
        handle.outgoing.try_send(privmsg("#test", "early")).unwrap();
        let seen = events_until(&rx, connected);

        // Assert: exactly one verdict, and it is the drop.
        assert_eq!(
            verdicts(&seen),
            vec![IrcEvent::OutgoingDropped("srv".to_string())]
        );
        let lines = wire_lines(&lines_rx);
        assert!(
            !lines.iter().any(|line| line.starts_with("PRIVMSG")),
            "a dropped message reached the wire: {lines:?}"
        );
    }

    #[test]
    fn a_privmsg_to_a_joined_channel_is_reported_accepted_exactly_once() {
        // Arrange
        let (port, lines_rx) = spawn_mock(true);
        let (tx, rx) = mpsc::channel();
        let server = local_server(port);
        let (_worker, handle) = spawn_irc_with_policy(
            server,
            "srv".to_string(),
            vec!["#test".to_string()],
            tx,
            one_shot(),
        );

        // Act
        events_until(&rx, joined);
        handle.outgoing.try_send(privmsg("#test", "hello")).unwrap();
        let seen = events_until(&rx, |event| matches!(event, IrcEvent::OutgoingAccepted(_)));

        // Assert
        assert_eq!(
            verdicts(&seen),
            vec![IrcEvent::OutgoingAccepted("srv".to_string())]
        );
        // The crate writes the trailing `:` only when the text needs it
        // (spaces, a leading colon, or empty), so a single word goes bare.
        let lines = wire_lines(&lines_rx);
        assert!(
            lines.contains(&"PRIVMSG #test hello".to_string()),
            "the message must reach the wire: {lines:?}"
        );
    }

    #[test]
    fn a_raw_channel_control_gets_neither_verdict() {
        // Arrange: no configured channels, so the registry only knows the
        // one the raw control asks for.
        let (port, lines_rx) = spawn_mock(true);
        let (tx, rx) = mpsc::channel();
        let server = local_server(port);
        let (_worker, handle) =
            spawn_irc_with_policy(server, "srv".to_string(), Vec::new(), tx, one_shot());
        let join = || OutgoingMessage::Raw {
            server: "srv".to_string(),
            line: "JOIN #test".to_string(),
        };

        // Act: a raw JOIN is a control both before and after registration;
        // the registry acts on it instead of reporting a verdict.
        handle.outgoing.try_send(join()).unwrap();
        let mut seen = events_until(&rx, connected);
        handle.outgoing.try_send(join()).unwrap();
        seen.extend(events_until(&rx, joined));

        // Assert
        let reported = verdicts(&seen);
        assert!(reported.is_empty(), "a control was reported: {reported:?}");
        let late = rx.recv_timeout(QUIET);
        assert!(late.is_err(), "a control was reported late: {late:?}");
        assert_eq!(
            wire_lines(&lines_rx)
                .iter()
                .filter(|line| *line == "JOIN #test")
                .count(),
            1,
            "the pre-registration JOIN is retained, the later one is already satisfied"
        );
    }
}
