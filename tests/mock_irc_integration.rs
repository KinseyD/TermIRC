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
use termirc::irc::{IrcEvent, OutgoingMessage, spawn_irc};
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
        channels: vec!["#test".to_string(), "#test2".to_string()],
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
                // A names line right before the chat, mirroring real
                // servers (and giving the console a flood to resist).
                write!(
                    writer,
                    ":mock 353 test = #test :test\r\n:alice!a@b PRIVMSG #test :hello world\r\nPING :mock\r\n"
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
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
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
fn joins_all_configured_channels() {
    // Arrange: the server config lists two channels.
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();

    // Act: collect until both JOINs have been sent (or the timeout elapses).
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
    let joins = std::cell::Cell::new(0);
    let lines = collect_client_lines_until(&lines_rx, |l| {
        if l.starts_with("JOIN") {
            joins.set(joins.get() + 1);
            joins.get() >= 2
        } else {
            false
        }
    });

    // Assert: every configured channel is joined.
    assert!(
        lines.iter().any(|l| l.starts_with("JOIN #test")),
        "no JOIN #test in {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("JOIN #test2")),
        "no JOIN #test2 in {lines:?}"
    );
}

#[test]
fn forwards_privmsg_as_chat_message() {
    // Arrange
    let (port, _lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );

    // Assert: skipping the status event and the console-bound greeting
    // numerics, the PRIVMSG arrives as a ChatMessage.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut chat = None;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Message(message)) if !message.channel.is_empty() => {
                chat = Some(message);
                break;
            }
            Ok(_) => continue, // status events and raw console replies
            Err(_) => break,
        }
    }
    assert_eq!(
        chat,
        Some(ChatMessage {
            server: "osu_irc".to_string(),
            channel: "#test".to_string(),
            nick: "alice".to_string(),
            text: "hello world".to_string(),
        })
    );
}

#[test]
fn server_replies_land_in_the_console_view() {
    // Arrange: the mock greets with 001..376 once the client registers.
    let (port, _lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();

    // Act
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );

    // Assert: skipping the status event, the greeting numerics arrive as
    // console-bound messages (empty channel and nick, verbatim wire text).
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut welcome = None;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Message(message)) if message.channel.is_empty() => {
                if message.text.starts_with(":mock 001") {
                    welcome = Some(message);
                    break;
                }
            }
            Ok(_) => continue, // chat for channels, status events
            Err(_) => break,
        }
    }
    assert_eq!(
        welcome,
        Some(ChatMessage {
            server: "osu_irc".to_string(),
            channel: String::new(),
            nick: String::new(),
            text: ":mock 001 test :Welcome to the Mock IRC Network".to_string(),
        })
    );
}

#[test]
fn names_replies_stay_out_of_the_console() {
    // Arrange: the mock sends a 353 names line immediately before the
    // channel PRIVMSG after JOIN.
    let (port, _lines_rx) = spawn_mock_server();
    let (tx, rx) = mpsc::channel();
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );

    // Act: collect every console line until the channel chat arrives.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut console = Vec::new();
    let mut chat = false;
    while !chat && let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now())
    {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Message(m)) if !m.channel.is_empty() => chat = true,
            Ok(IrcEvent::Message(m)) => console.push(m.text),
            Ok(_) => {}
            Err(_) => break,
        }
    }

    // Assert: the chat arrived, and no names line leaked into the console.
    assert!(chat, "channel chat never arrived");
    assert!(
        console.iter().all(|t| !t.contains(" 353 ")),
        "353 leaked into the console: {console:?}"
    );
}

#[test]
fn client_answers_server_ping_with_pong() {
    // Arrange
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();

    // Act: the mock sends PING right after the PRIVMSG.
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
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
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );

    // Assert: after the chat message, a disconnect notification carrying the
    // server's config key must arrive — the UI never keeps showing
    // "connected" to a dead feed.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let mut disconnected = false;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(IrcEvent::Status(server, text))
                if server == "osu_irc" && text.contains("disconnected") =>
            {
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
    let _handle = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );

    // Assert: the failed connect surfaces as an Error event tagged with the
    // server's config key, not silence.
    match rx.recv_timeout(RECV_TIMEOUT) {
        Ok(IrcEvent::Error(server, _)) => assert_eq!(server, "osu_irc"),
        other => panic!("expected IrcEvent::Error, got {other:?}"),
    }
}

#[test]
fn outgoing_message_is_sent_as_privmsg_on_the_wire() {
    // Arrange: connect to the mock and wait for the JOIN to land.
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();
    let (_handle, sender) = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
    let _ = collect_client_lines_until(&lines_rx, |l| l.starts_with("JOIN"));

    // Act: submit a message through the outgoing channel.
    sender
        .blocking_send(OutgoingMessage::Privmsg {
            server: "osu_irc".to_string(),
            target: "#test".to_string(),
            text: "hello world".to_string(),
        })
        .unwrap();
    let lines = collect_client_lines_until(&lines_rx, |l| l.starts_with("PRIVMSG"));

    // Assert: the message went out addressed to the channel.
    assert!(
        lines.iter().any(|l| l == "PRIVMSG #test :hello world"),
        "no PRIVMSG #test in {lines:?}"
    );
}

#[test]
fn raw_line_is_sent_verbatim_on_the_wire() {
    // Arrange: connect to the mock and wait for the JOIN to land.
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();
    let (_handle, sender) = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
    let _ = collect_client_lines_until(&lines_rx, |l| l.starts_with("JOIN"));

    // Act: submit a raw console line through the outgoing channel.
    sender
        .blocking_send(OutgoingMessage::Raw {
            server: "osu_irc".to_string(),
            line: "WHOIS test".to_string(),
        })
        .unwrap();
    let lines = collect_client_lines_until(&lines_rx, |l| l.starts_with("WHOIS"));

    // Assert: the line goes out verbatim as command + parameters.
    assert!(
        lines.iter().any(|l| l == "WHOIS test"),
        "no WHOIS test in {lines:?}"
    );
}

#[test]
fn raw_line_marks_a_colon_last_param_as_trailing() {
    // Arrange: connect to the mock and wait for the JOIN to land.
    let (port, lines_rx) = spawn_mock_server();
    let (tx, _rx) = mpsc::channel();
    let (_handle, sender) = spawn_irc(
        server_config_for(port),
        "osu_irc".to_string(),
        server_config_for(port).channels,
        tx,
    );
    let _ = collect_client_lines_until(&lines_rx, |l| l.starts_with("JOIN"));

    // Act: submit a raw line whose last parameter explicitly starts with
    // ':' (the client's own keepalive PING carries a bare token, so filter
    // on the colon form).
    sender
        .blocking_send(OutgoingMessage::Raw {
            server: "osu_irc".to_string(),
            line: "PING :smoke".to_string(),
        })
        .unwrap();
    let lines = collect_client_lines_until(&lines_rx, |l| l.starts_with("PING :"));

    // Assert: the irc crate's `stringify` marks a ':'-prefixed last param
    // as the trailing param by prefixing another ':' — crate-inherent
    // behavior that keeps the param a single token. Last params without a
    // leading ':' go out verbatim.
    assert!(
        lines.iter().any(|l| l == "PING ::smoke"),
        "no PING ::smoke in {lines:?}"
    );
}
