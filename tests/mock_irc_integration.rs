//! End-to-end tests for the IRC adapter against a local mock server.
//!
//! The mock speaks just enough IRC over a real TCP socket to register a
//! client, receive its JOIN, and send a PRIVMSG plus a PING — exercising the
//! full connect → identify → join → receive → keepalive path without touching
//! the network.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use termirc::config::ServerConfig;
use termirc::irc::{IrcEvent, spawn_irc};
use termirc::message::ChatMessage;

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

fn server_config_for(port: u16) -> ServerConfig {
    ServerConfig {
        username: "test".to_string(),
        nickname: "test".to_string(),
        password: "secret".to_string(),
        server: "127.0.0.1".to_string(),
        use_tls: false,
        port,
        channels: vec!["#test".to_string()],
    }
}

/// Start a mock IRC server on an ephemeral port.
///
/// Returns the port and a receiver streaming every line the client sends.
/// The server greets the client once it sees `USER`, and after the client
/// JOINs it sends one PRIVMSG and a PING, then keeps the socket open.
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
        let mut joined = false;

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break; // client disconnected
            }
            let trimmed = line.trim_end().to_string();
            if lines_tx.send(trimmed.clone()).is_err() {
                break;
            }
            if !greeted && trimmed.starts_with("USER") {
                write!(
                    writer,
                    ":mock 001 test :Welcome to the Mock IRC Network\r\n\
                     :mock 002 test :Your host is mock\r\n\
                     :mock 003 test :This server was created today\r\n\
                     :mock 004 test mock 1.0 ov o\r\n\
                     :mock 375 test :- mock Message of the Day -\r\n\
                     :mock 376 test :End of /MOTD command.\r\n"
                )
                .unwrap();
                writer.flush().unwrap();
                greeted = true;
            }
            if greeted && !joined && trimmed.starts_with("JOIN") {
                write!(
                    writer,
                    ":alice!a@b PRIVMSG #test :hello world\r\nPING :mock\r\n"
                )
                .unwrap();
                writer.flush().unwrap();
                joined = true;
            }
        }
    });

    (port, lines_rx)
}

/// Collect lines from the mock server's view of the client until `stop`
/// matches or the timeout elapses; returns everything collected.
fn collect_client_lines_until(
    lines_rx: &mpsc::Receiver<String>,
    stop: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut collected = Vec::new();
    while let Ok(line) = lines_rx.recv_timeout(RECV_TIMEOUT) {
        let done = stop(&line);
        collected.push(line);
        if done {
            break;
        }
    }
    collected
}

#[test]
fn connects_sends_pass_before_nick_and_joins_target_channel() {
    // Arrange
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(server_config_for(port), "#test".to_string(), tx);
    let lines = collect_client_lines_until(&lines_rx, |l| l.starts_with("JOIN"));

    // Assert
    let pass_at = lines
        .iter()
        .position(|l| l == "PASS secret")
        .expect("PASS not sent");
    let nick_at = lines
        .iter()
        .position(|l| l == "NICK test")
        .expect("NICK not sent");
    assert!(pass_at < nick_at, "PASS must precede NICK, got: {lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("JOIN #test")),
        "no JOIN #test in {lines:?}"
    );
}

#[test]
fn forwards_privmsg_as_chat_message() {
    // Arrange
    let (port, _lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(server_config_for(port), "#test".to_string(), tx);

    // Assert: skipping the status event, the PRIVMSG arrives as a ChatMessage.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut chat = None;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Message(message)) => {
                chat = Some(message);
                break;
            }
            Ok(_) => continue, // Status/Error events before the message
            Err(_) => break,
        }
    }
    assert_eq!(
        chat,
        Some(ChatMessage {
            nick: "alice".to_string(),
            text: "hello world".to_string(),
        })
    );
}

#[test]
fn client_answers_server_ping_with_pong() {
    // Arrange
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();

    // Act: the mock sends PING right after the PRIVMSG.
    let _handle = spawn_irc(server_config_for(port), "#test".to_string(), tx);
    let lines = collect_client_lines_until(&lines_rx, |l| l.starts_with("PONG"));

    // Assert
    assert!(
        lines.iter().any(|l| l.starts_with("PONG")),
        "no PONG in {lines:?}"
    );
}

/// A mock that greets the client, answers its JOIN with one PRIVMSG, and then
/// closes the connection — simulating a server restart or idle kick.
fn spawn_mock_server_that_closes() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        let (socket, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut writer = socket;

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line.starts_with("USER") {
                write!(
                    writer,
                    ":mock 001 test :Welcome to the Mock IRC Network\r\n\
                     :mock 376 test :End of /MOTD command.\r\n"
                )
                .unwrap();
                writer.flush().unwrap();
            }
            if line.starts_with("JOIN") {
                write!(writer, ":alice!a@b PRIVMSG #test :hi\r\n").unwrap();
                writer.flush().unwrap();
                break; // drop everything -> clean TCP close
            }
        }
    });

    port
}

#[test]
fn reports_status_when_server_closes_connection() {
    // Arrange: the server will close the socket right after one PRIVMSG.
    let port = spawn_mock_server_that_closes();
    let (tx, rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(server_config_for(port), "#test".to_string(), tx);

    // Assert: after the chat message, a disconnect notification must arrive —
    // the UI must never keep showing "connected" to a dead feed.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut disconnected = false;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Status(text)) if text.contains("disconnected") => {
                disconnected = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(
        disconnected,
        "no disconnect notification after server close"
    );
}

#[test]
fn reports_error_when_connection_is_refused() {
    // Arrange: claim an ephemeral port, then drop the listener — nothing listens.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (tx, rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(server_config_for(port), "#test".to_string(), tx);

    // Assert: the failed connect surfaces as an Error event, not silence.
    match rx.recv_timeout(RECV_TIMEOUT) {
        Ok(IrcEvent::Error(_)) => {}
        other => panic!("expected IrcEvent::Error, got {other:?}"),
    }
}
