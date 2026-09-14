use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use termirc::config::ServerConfig;
use termirc::irc::{
    ConnectionCommand, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy,
};

const WAIT: Duration = Duration::from_secs(3);

fn config(port: u16) -> ServerConfig {
    ServerConfig {
        username: "test".into(),
        nickname: "test".into(),
        password: "".into(),
        server: "127.0.0.1".into(),
        port,
        use_tls: false,
        channels: vec!["#ok".into(), "#bad".into()],
    }
}

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 2,
        delay: Duration::from_millis(30),
        timeout: Duration::from_millis(500),
    }
}

fn receive_until(
    rx: &mpsc::Receiver<IrcEvent>,
    predicate: impl Fn(&IrcEvent) -> bool,
) -> Vec<IrcEvent> {
    let deadline = Instant::now() + WAIT;
    let mut events = Vec::new();
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|e| panic!("missing expected event: {e}; received {events:?}"));
        let done = predicate(&event);
        events.push(event);
        if done {
            return events;
        }
    }
}

fn registered(reader: &mut BufReader<TcpStream>) {
    reader.get_ref().set_read_timeout(Some(WAIT)).unwrap();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line.starts_with("USER ") {
            return;
        }
    }
}

fn line(reader: &mut BufReader<TcpStream>, prefix: &str) -> String {
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "closed before {prefix}"
        );
        if line.starts_with(prefix) {
            return line.trim_end().into();
        }
    }
}

#[test]
fn registration_and_channel_states_wait_for_server_confirmation() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec!["#ok".into(), "#bad".into()],
        tx,
        policy(),
    );
    let (mut socket, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    registered(&mut reader);
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        IrcEvent::Connection("srv".into(), ConnectionState::Connecting)
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(30)).is_err(),
        "green before 001"
    );
    socket
        .write_all(b":mock 001 assigned :Welcome\r\n:mock 376 assigned :End\r\n")
        .unwrap();
    let events = receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    assert!(events.contains(&IrcEvent::Nickname("srv".into(), "assigned".into())));
    line(&mut reader, "JOIN #bad");
    socket.write_all(b":someone!u@h JOIN #ok\r\n:assigned!u@h JOIN #ok\r\n:mock 473 assigned #bad :Invite only\r\n").unwrap();
    let events = receive_until(
        &rx,
        |e| matches!(e, IrcEvent::Channel(_, c, ConnectionState::Stopped) if c == "#bad"),
    );
    assert_eq!(
        events
            .iter()
            .filter(
                |e| matches!(e, IrcEvent::Channel(_, c, ConnectionState::Connected) if c == "#ok")
            )
            .count(),
        1
    );
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn nick_away_and_disconnect_use_protocol_without_chat_echo() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec![],
        tx,
        policy(),
    );
    let (mut socket, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    registered(&mut reader);
    socket
        .write_all(b":mock 001 test :Welcome\r\n:mock 376 test :End\r\n")
        .unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Nick("busy".into()))
        .unwrap();
    assert_eq!(line(&mut reader, "NICK"), "NICK busy");
    socket
        .write_all(b":mock 433 test busy :Nickname in use\r\nPING :alive\r\n")
        .unwrap();
    assert_eq!(line(&mut reader, "PONG"), "PONG alive");
    handle
        .control
        .blocking_send(ConnectionCommand::Nick("Alice".into()))
        .unwrap();
    assert_eq!(line(&mut reader, "NICK"), "NICK Alice");
    assert!(
        !rx.try_iter()
            .any(|e| matches!(e, IrcEvent::Nickname(_, n) if n == "Alice"))
    );
    socket.write_all(b":test!u@h NICK :Alice\r\n").unwrap();
    receive_until(&rx, |e| {
        *e == IrcEvent::Nickname("srv".into(), "Alice".into())
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Away(Some("lunch  break".into())))
        .unwrap();
    assert_eq!(line(&mut reader, "AWAY"), "AWAY :lunch  break");
    socket.write_all(b":mock 306 Alice :Away\r\n").unwrap();
    receive_until(&rx, |e| *e == IrcEvent::Away("srv".into(), true));
    handle
        .control
        .blocking_send(ConnectionCommand::Back)
        .unwrap();
    assert_eq!(line(&mut reader, "AWAY"), "AWAY");
    socket.write_all(b":mock 305 Alice :Back\r\n").unwrap();
    let feedback = receive_until(&rx, |e| *e == IrcEvent::Away("srv".into(), false));
    assert!(
        !feedback
            .iter()
            .any(|e| matches!(e, IrcEvent::Message(m) if m.text == "Away")),
        "away command feedback leaked into console"
    );
    handle
        .control
        .blocking_send(ConnectionCommand::Disconnect("bye  all".into()))
        .unwrap();
    assert_eq!(line(&mut reader, "QUIT"), "QUIT :bye  all");
    let events = receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Stopped))
    });
    assert!(events.contains(&IrcEvent::ControlApplied("srv".into())));
    assert!(
        !rx.recv_timeout(Duration::from_millis(100))
            .is_ok_and(|e| matches!(e, IrcEvent::Connection(_, ConnectionState::Connecting)))
    );
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn failed_attempts_stop_after_retry_budget_and_can_be_cancelled() {
    // An owned listener accepts, but never registers; each attempt times out.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec![],
        tx,
        policy(),
    );
    let events = receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Stopped))
    });
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, IrcEvent::Connection(_, ConnectionState::Connecting)))
            .count(),
        3
    );
    handle
        .control
        .blocking_send(ConnectionCommand::Connect)
        .unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connecting))
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Disconnect(String::new()))
        .unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Stopped))
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn manual_reconnect_keeps_confirmed_nick_and_rejoins_channels() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec!["#ok".into()],
        tx,
        policy(),
    );
    let (mut socket, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    registered(&mut reader);
    socket
        .write_all(b":mock 001 test :Welcome\r\n:mock 376 test :End\r\n")
        .unwrap();
    line(&mut reader, "JOIN");
    socket.write_all(b":test!u@h NICK :Alice\r\n").unwrap();
    receive_until(&rx, |e| {
        *e == IrcEvent::Nickname("srv".into(), "Alice".into())
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Reconnect)
        .unwrap();
    assert_eq!(line(&mut reader, "QUIT"), "QUIT Reconnecting");
    socket.shutdown(std::net::Shutdown::Both).unwrap();
    let (mut socket, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    reader.get_ref().set_read_timeout(Some(WAIT)).unwrap();
    assert_eq!(line(&mut reader, "NICK"), "NICK Alice");
    line(&mut reader, "USER");
    socket
        .write_all(b":mock 001 Alice :Welcome\r\n:mock 376 Alice :End\r\n")
        .unwrap();
    assert_eq!(line(&mut reader, "JOIN"), "JOIN #ok");
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Connect)
        .unwrap();
    handle
        .control
        .blocking_send(ConnectionCommand::Nick("still_here".into()))
        .unwrap();
    assert_eq!(line(&mut reader, "NICK"), "NICK still_here");
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn authentication_failure_stops_without_retrying() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec![],
        tx,
        policy(),
    );
    let (mut socket, _) = listener.accept().unwrap();
    registered(&mut BufReader::new(socket.try_clone().unwrap()));
    socket
        .write_all(b":mock 464 test :Password incorrect\r\n")
        .unwrap();
    let events = receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Stopped))
    });
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, IrcEvent::Connection(_, ConnectionState::Connecting)))
            .count(),
        1
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn cancelling_backoff_prevents_future_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let mut retry = policy();
    retry.delay = Duration::from_secs(10);
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec![],
        tx,
        retry,
    );
    receive_until(&rx, |e| matches!(e, IrcEvent::Error(..)));
    let start = Instant::now();
    handle
        .control
        .blocking_send(ConnectionCommand::Disconnect(String::new()))
        .unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Stopped))
    });
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "disconnect waited for backoff"
    );
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(handle);
    worker.join().unwrap();
}

#[test]
fn unexpected_disconnect_reconnects_without_replaying_queued_chat() {
    use termirc::irc::OutgoingMessage;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let (tx, rx) = mpsc::channel();
    let (worker, handle) = spawn_irc_with_policy(
        config(listener.local_addr().unwrap().port()),
        "srv".into(),
        vec!["#ok".into()],
        tx,
        policy(),
    );
    let (mut socket, _) = listener.accept().unwrap();
    registered(&mut BufReader::new(socket.try_clone().unwrap()));
    socket.write_all(b":mock 001 test :Welcome\r\n").unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    socket.shutdown(std::net::Shutdown::Both).unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connecting))
    });
    let (mut socket, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    registered(&mut reader);
    handle
        .outgoing
        .blocking_send(OutgoingMessage::Privmsg {
            server: "srv".into(),
            target: "#ok".into(),
            text: "must not replay".into(),
        })
        .unwrap();
    socket
        .write_all(b":mock 001 test :Welcome again\r\n:mock 376 test :End\r\n")
        .unwrap();
    receive_until(&rx, |e| {
        matches!(e, IrcEvent::Connection(_, ConnectionState::Connected))
    });
    handle
        .control
        .blocking_send(ConnectionCommand::Nick("sentinel".into()))
        .unwrap();
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        assert!(!line.starts_with("PRIVMSG"), "stale chat sent: {line}");
        if line.starts_with("NICK sentinel") {
            break;
        }
    }
    drop(handle);
    worker.join().unwrap();
}
