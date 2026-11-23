//! Integration tests for `--listen`: a real connection worker against a
//! mock server receives another user's PRIVMSG and renders it exactly,
//! while the wire never sees the client send one — the read-only proof.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use termirc::config::ServerConfig;
use termirc::connection::{IrcEvent, spawn_irc};
use termirc::listen::render_event;

const RECV_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to keep draining the wire after the chat arrived: any echo
/// would have been sent immediately after the PRIVMSG was processed.
const WIRE_GRACE: Duration = Duration::from_secs(1);

fn server_config_for(port: u16) -> ServerConfig {
    ServerConfig {
        username: "test".to_string(),
        nickname: "test".to_string(),
        password: String::new(),
        server: "127.0.0.1".to_string(),
        use_tls: false,
        port,
        channels: vec!["#test".to_string()],
        queries: vec![],
        sasl_username: None,
        sasl_password: None,
        proxy: None,
    }
}

/// Start a mock IRC server on an ephemeral port.
///
/// Returns the port and a receiver streaming every line the client
/// sends. The server greets on `USER`, confirms the client's JOIN, then
/// delivers one PRIVMSG from another user — the traffic `--listen`
/// exists to print.
fn spawn_mock_server() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (lines_tx, lines_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let (socket, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut writer = socket;
        let mut greeted = false;

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break; // client disconnected
            }
            let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
            if lines_tx.send(trimmed.clone()).is_err() {
                break;
            }
            if !greeted && trimmed.starts_with("USER") {
                write!(
                    writer,
                    ":mock 001 test :Welcome to the Mock IRC Network\r\n\
                     :mock 376 test :End of /MOTD command.\r\n"
                )
                .unwrap();
                writer.flush().unwrap();
                greeted = true;
            }
            if greeted && let Some(channel) = trimmed.strip_prefix("JOIN ") {
                write!(writer, ":test!u@h JOIN {channel}\r\n").unwrap();
                writer.flush().unwrap();
                if channel == "#test" {
                    write!(writer, ":alice!a@b PRIVMSG #test :hello world\r\n").unwrap();
                    writer.flush().unwrap();
                }
            }
        }
    });

    (port, lines_rx)
}

/// Wait for the mock's chat line to arrive as a routed message event,
/// panicking with every event seen so far if it never does.
fn receive_chat_event(rx: &mpsc::Receiver<IrcEvent>) -> IrcEvent {
    let deadline = Instant::now() + RECV_TIMEOUT;
    let mut seen = Vec::new();
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| panic!("no chat message arrived: {error}; events: {seen:?}"));
        let done = matches!(
            &event,
            IrcEvent::Message(message) if message.content.text == "hello world"
        );
        seen.push(event.clone());
        if done {
            return event;
        }
    }
}

/// The UTC clock face `listen::render_event` derives from a message's
/// `received_at`, restated here so the expected line is exact.
fn expected_time(message: &termirc::core::RoutedMessage) -> String {
    let secs = message
        .content
        .received_at
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let seconds_of_day = secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds_of_day / 3_600,
        (seconds_of_day / 60) % 60,
        seconds_of_day % 60
    )
}

#[test]
fn renders_an_incoming_channel_chat_line_exactly() {
    // Arrange: one real worker against the mock, spawned exactly as
    // listen mode spawns it.
    let (port, _lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();
    let config = server_config_for(port);
    let channels = config.channels.clone();
    let (_worker, _handle) = spawn_irc(config, "srv".to_string(), channels, tx);

    // Act
    let event = receive_chat_event(&rx);

    // Assert: the whole rendered line, timestamp included.
    let IrcEvent::Message(message) = &event else {
        unreachable!("receive_chat_event only returns on a message");
    };
    assert_eq!(
        render_event(&event),
        Some(format!(
            "[{}][srv/#test][alice] hello world",
            expected_time(message)
        ))
    );
}

#[test]
fn never_puts_chat_traffic_on_the_wire() {
    // Arrange
    let (port, lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();
    let config = server_config_for(port);
    let channels = config.channels.clone();
    let (_worker, _handle) = spawn_irc(config, "srv".to_string(), channels, tx);

    // Act: let the chat arrive, then keep draining the wire for the
    // grace window so a late echo could not slip past the assertion.
    receive_chat_event(&rx);
    let mut lines = Vec::new();
    let deadline = Instant::now() + WIRE_GRACE;
    while let Ok(line) = lines_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        lines.push(line);
    }

    // Assert: registration lines plus transport keepalive — every
    // observed verb is one the worker emits to join or to keep the
    // connection alive, and none is user chat. PING/PONG are the
    // client's timing-dependent keepalive, not chat.
    assert!(
        lines.iter().any(|l| l == "NICK test"),
        "registration did not reach NICK: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l == "JOIN #test"),
        "the configured JOIN must still happen: {lines:?}"
    );
    for line in &lines {
        let verb = line.split(' ').next().unwrap_or_default();
        assert!(
            matches!(
                verb,
                "CAP" | "PASS" | "NICK" | "USER" | "JOIN" | "PING" | "PONG" | "QUIT"
            ),
            "listen mode sent unexpected traffic: {line} (all lines: {lines:?})"
        );
    }
}
