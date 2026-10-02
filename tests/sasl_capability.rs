//! SASL PLAIN and IRCv3 capability negotiation against a CAP-aware mock
//! server, exercising the full worker: wire handshake, registration, join,
//! server-time decoding, and echo-message confirmation through `Session`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, UNIX_EPOCH};

use termirc::application::{Session, SubmissionEffect, submit_composer};
use termirc::config::{Config, ServerConfig};
use termirc::connection::{
    ConnectionHandle, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy,
};
use termirc::core::{
    BufferId, ChannelState, ChannelStatus, DeliveryState, Direction, MessageKind, OutgoingMessage,
};

const WAIT: Duration = Duration::from_secs(5);

fn server_config(port: u16, password: &str, sasl: Option<(&str, &str)>) -> ServerConfig {
    ServerConfig {
        username: "test".into(),
        nickname: "test".into(),
        password: password.into(),
        server: "127.0.0.1".into(),
        port,
        use_tls: false,
        channels: vec!["#chan".into()],
        queries: vec![],
        sasl_username: sasl.map(|(username, _)| username.into()),
        sasl_password: sasl.map(|(_, password)| password.into()),
    }
}

fn policy(retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries: retries,
        delay: Duration::from_millis(30),
        timeout: Duration::from_millis(2_000),
    }
}

/// A CAP-aware mock server with one connected worker.
struct CapMock {
    listener: TcpListener,
    socket: Option<BufReader<TcpStream>>,
    rx: mpsc::Receiver<IrcEvent>,
    handle: Option<ConnectionHandle>,
    worker: Option<JoinHandle<()>>,
}

impl CapMock {
    fn start(password: &str, sasl: Option<(&str, &str)>, retries: u32) -> (Self, Config) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = server_config(listener.local_addr().unwrap().port(), password, sasl);
        let config = Config {
            servers: [("srv".into(), server.clone())].into_iter().collect(),
        };
        let (tx, rx) = mpsc::channel();
        let (worker, handle) = spawn_irc_with_policy(
            server,
            "srv".into(),
            vec!["#chan".into()],
            tx,
            policy(retries),
        );
        let mut mock = Self {
            listener,
            socket: None,
            rx,
            handle: Some(handle),
            worker: Some(worker),
        };
        mock.accept();
        (mock, config)
    }

    /// Poll the nonblocking listener until the worker connects; the accepted
    /// socket inherits non-blocking mode on Windows, so restore blocking I/O.
    fn accept(&mut self) {
        let deadline = Instant::now() + WAIT;
        let socket = loop {
            match self.listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "worker did not connect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        self.socket = Some(BufReader::new(socket));
    }

    fn send(&mut self, lines: &str) {
        self.socket
            .as_mut()
            .unwrap()
            .get_mut()
            .write_all(lines.as_bytes())
            .unwrap();
    }

    /// One client line, skipping the irc crate's keepalive pings (bare
    /// numeric payloads sent right after the MOTD).
    fn read_line(&mut self) -> String {
        loop {
            let line = {
                let reader = self.socket.as_mut().unwrap();
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0, "socket closed");
                line.trim_end_matches(['\r', '\n']).to_owned()
            };
            if line.starts_with("PING ") && line[5..].bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            return line;
        }
    }

    fn expect(&mut self, expected: &str) {
        let line = self.read_line();
        assert_eq!(line, expected);
    }

    fn receive_until(&mut self, predicate: impl Fn(&IrcEvent) -> bool) -> Vec<IrcEvent> {
        let deadline = Instant::now() + WAIT;
        let mut received = Vec::new();
        loop {
            let event = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("missing event: {error}; received {received:?}"));
            let done = predicate(&event);
            received.push(event);
            if done {
                return received;
            }
        }
    }

    /// Drop the handle, close the socket, and join the worker.
    fn finish(mut self) {
        self.handle.take();
        if let Some(socket) = self.socket.take() {
            let _ = socket.get_ref().shutdown(std::net::Shutdown::Both);
        }
        let worker = self.worker.take().unwrap();
        let deadline = Instant::now() + WAIT;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            worker.is_finished(),
            "worker did not stop after handle drop"
        );
        worker.join().unwrap();
    }
}

/// Drive the SASL handshake up to (and including) the credential line.
fn sasl_handshake(mock: &mut CapMock, password: &str) {
    mock.expect("CAP LS 302");
    mock.send(":mock CAP * LS 302 :sasl server-time echo-message\r\n");
    if !password.is_empty() {
        mock.expect(&format!("PASS {password}"));
    }
    mock.expect("NICK test");
    mock.expect("USER test 0 * test");
    mock.expect("CAP REQ :server-time echo-message sasl");
    mock.send(":mock CAP * ACK :server-time echo-message sasl\r\n");
    mock.expect("AUTHENTICATE PLAIN");
    mock.send("AUTHENTICATE +\r\n");
    mock.expect("AUTHENTICATE AGppbGxlcwBzZXNhbWU=");
}

#[test]
fn sasl_plain_handshake_registers_and_joins() {
    let (mut mock, _config) = CapMock::start("secret", Some(("jilles", "sesame")), 2);
    sasl_handshake(&mut mock, "secret");
    mock.send(":mock 903 test :SASL authentication successful\r\n");
    mock.expect("CAP END");
    mock.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
    mock.expect("JOIN #chan");
    mock.send(":test!u@h JOIN #chan\r\n");
    let events = mock.receive_until(|event| {
        matches!(event, IrcEvent::Channel(_, channel, ChannelStatus { state: ChannelState::Joined, desired: true }) if channel == "#chan")
    });
    assert!(
        events
            .iter()
            .any(|event| matches!(event, IrcEvent::Connection(_, ConnectionState::Connected)))
    );
    mock.finish();
}

#[test]
fn sasl_failure_is_permanent_without_retries() {
    let (mut mock, _config) = CapMock::start("", Some(("jilles", "sesame")), 2);
    sasl_handshake(&mut mock, "");
    mock.send(":mock 904 test :SASL authentication failed\r\n");
    let events = mock
        .receive_until(|event| matches!(event, IrcEvent::Connection(_, ConnectionState::Stopped)));
    assert!(
        events
            .iter()
            .any(|event| event
                == &IrcEvent::Error("srv".into(), "sasl authentication failed".into()))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, IrcEvent::Connection(_, ConnectionState::Connecting)))
            .count(),
        1,
        "retried after permanent SASL failure: {events:?}"
    );
    assert!(mock.rx.recv_timeout(Duration::from_millis(150)).is_err());
    // A retry would reopen the listener within the backoff window.
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        matches!(mock.listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
        "worker reconnected after SASL failure"
    );
    mock.finish();
}

#[test]
fn capless_server_registers_without_cap_traffic_after_welcome() {
    let (mut mock, _config) = CapMock::start("", None, 0);
    mock.expect("CAP LS 302");
    mock.expect("NICK test");
    mock.expect("USER test 0 * test");
    mock.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
    mock.expect("JOIN #chan");
    mock.send(":test!u@h JOIN #chan\r\n");
    mock.receive_until(|event| {
        matches!(event, IrcEvent::Channel(_, channel, ChannelStatus { state: ChannelState::Joined, desired: true }) if channel == "#chan")
    });
    // A trailing barrier proves no CAP END (or other negotiation line)
    // leaks out after registration on a capless server.
    mock.handle
        .as_ref()
        .unwrap()
        .outgoing
        .try_send(OutgoingMessage::Raw {
            server: "srv".into(),
            line: "PING :capless-barrier".into(),
        })
        .unwrap();
    loop {
        let line = mock.read_line();
        assert!(
            !line.starts_with("CAP "),
            "post-registration CAP line: {line}"
        );
        if line == "PING capless-barrier" {
            break;
        }
    }
    mock.finish();
}

#[test]
fn server_time_tag_sets_received_at() {
    let (mut mock, _config) = CapMock::start("", None, 0);
    mock.expect("CAP LS 302");
    mock.send(":mock CAP * LS 302 :server-time\r\n");
    mock.expect("NICK test");
    mock.expect("USER test 0 * test");
    mock.expect("CAP REQ server-time");
    mock.send(":mock CAP * ACK :server-time\r\n");
    mock.expect("CAP END");
    mock.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
    mock.expect("JOIN #chan");
    mock.send(":test!u@h JOIN #chan\r\n");
    mock.receive_until(|event| {
        matches!(event, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    mock.send("@time=2026-10-02T01:02:03.500Z :a!u@h PRIVMSG #chan :hi\r\n");
    let events = mock.receive_until(
        |event| matches!(event, IrcEvent::Message(message) if message.content.text == "hi"),
    );
    let message = events
        .iter()
        .find_map(|event| match event {
            IrcEvent::Message(message) if message.content.text == "hi" => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(message.content.kind, MessageKind::Chat);
    assert_eq!(
        message.content.received_at,
        UNIX_EPOCH + Duration::new(1_790_902_923, 500_000_000)
    );
    mock.finish();
}

#[test]
fn echo_message_confirms_local_echo_once() {
    let (mut mock, config) = CapMock::start("", None, 0);
    let mut session = Session::default();
    session.register_config(&config);
    let channel = session.open_channel("srv", "#chan");
    session.select_buffer_id(channel);

    // Handshake with the IRCv3.1 LS form (list in the third parameter).
    mock.expect("CAP LS 302");
    mock.send(":mock CAP * LS :echo-message\r\n");
    mock.expect("NICK test");
    mock.expect("USER test 0 * test");
    mock.expect("CAP REQ echo-message");
    mock.send(":mock CAP * ACK :echo-message\r\n");
    mock.expect("CAP END");
    mock.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
    mock.expect("JOIN #chan");
    mock.send(":test!u@h JOIN #chan\r\n");
    for event in mock.receive_until(|event| {
        matches!(event, IrcEvent::Channel(_, name, ChannelStatus { state: ChannelState::Joined, desired: true }) if name == "#chan")
    }) {
        session.handle_event(event);
    }

    // Arrange: send through the composer, leaving one unconfirmed local echo.
    session.restore_input("hi there".into());
    let outgoing = mock.handle.as_ref().unwrap().outgoing.clone();
    let control = mock.handle.as_ref().unwrap().control.clone();
    let connections = HashMap::from([("srv".into(), ConnectionHandle { outgoing, control })]);
    let mut status = String::new();
    let effect = submit_composer(&mut session, &config, &connections, &mut status);
    assert_eq!(effect, SubmissionEffect::None);
    assert_eq!(status, "");
    let echoed = |id: BufferId, session: &Session| {
        session
            .messages_for(id)
            .iter()
            .filter(|message| message.text == "hi there")
            .count()
    };
    assert_eq!(echoed(channel, &session), 1);
    assert_eq!(
        session
            .messages_for(channel)
            .iter()
            .find(|message| message.text == "hi there")
            .unwrap()
            .delivery,
        DeliveryState::Unconfirmed
    );

    // Act: the server echoes our own PRIVMSG back.
    mock.expect("PRIVMSG #chan :hi there");
    mock.send(":test!u@h PRIVMSG #chan :hi there\r\n");
    for event in mock.receive_until(|event| {
        matches!(event, IrcEvent::Message(message) if message.content.nick == "test" && message.content.text == "hi there")
    }) {
        session.handle_event(event);
    }

    // Assert: exactly one line, now server-confirmed.
    assert_eq!(echoed(channel, &session), 1);
    let message = session
        .messages_for(channel)
        .iter()
        .find(|message| message.text == "hi there")
        .unwrap();
    assert_eq!(message.delivery, DeliveryState::Confirmed);
    assert_eq!(message.direction, Direction::Outgoing);
    drop(connections);
    mock.finish();
}
