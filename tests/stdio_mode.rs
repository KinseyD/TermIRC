//! Integration tests for `--stdio`: a real connection worker against a
//! mock server, one queued send per stdin line, one rendered line per
//! event on stdout — chat forwarded after the JOIN, raw commands verbatim,
//! the server's numerics printed, refusals on stderr, and an EOF that
//! drains every queued verdict before the QUIT.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use termirc::config::Config;
use termirc::connection::{ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy};
use termirc::core::{ChannelState, ChannelStatus};
use termirc::stdio::{self, Input};

/// How long an expected event may take before a test gives up.
const WAIT: Duration = Duration::from_secs(5);

fn server_config_for(port: u16) -> Config {
    Config::parse(&format!(
        r##"
[servers.libera]
username = "test"
nickname = "test"
password = ""
server = "127.0.0.1"
port = {port}
channels = ["#test"]
"##
    ))
    .unwrap()
}

/// Start a mock IRC server on an ephemeral port: it greets on `USER`,
/// echoes every JOIN back so the channel joins, and answers a WHOIS with
/// the 311/318 pair. Returns the port and a receiver streaming every line
/// the client sends.
fn spawn_mock() -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (lines_tx, lines_rx) = mpsc::channel();
    thread::spawn(move || {
        let (socket, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
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
                write!(
                    writer,
                    ":mock 001 test :Welcome to the Mock IRC Network\r\n"
                )
                .unwrap();
            }
            if let Some(channel) = trimmed.strip_prefix("JOIN ") {
                write!(writer, ":test!u@h JOIN {channel}\r\n").unwrap();
            }
            if trimmed.starts_with("WHOIS") {
                write!(
                    writer,
                    ":mock 311 test alice alice user host * :Real Name\r\n\
                     :mock 318 test alice :End of WHOIS\r\n"
                )
                .unwrap();
            }
            writer.flush().unwrap();
        }
    });
    (port, lines_rx)
}

/// The worker timing every test runs under.
fn policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        delay: Duration::from_millis(10),
        timeout: WAIT,
    }
}

/// One connected worker with its `#test` JOIN confirmed, plus the event
/// receiver the stdio loop will keep reading from.
fn connected_worker(
    port: u16,
) -> (
    mpsc::Receiver<IrcEvent>,
    std::collections::HashMap<String, termirc::connection::ConnectionHandle>,
) {
    let (tx, rx) = mpsc::channel();
    let config = server_config_for(port);
    let server = config.servers.get("libera").unwrap().clone();
    let (_worker, handle) = spawn_irc_with_policy(
        server,
        "libera".to_string(),
        vec!["#test".to_string()],
        tx.clone(),
        policy(),
    );
    let mut workers = std::collections::HashMap::new();
    workers.insert("libera".to_string(), handle);

    // Drain the startup burst until the connection and the JOIN are
    // confirmed, then replay exactly those two events into the channel the
    // stdio loop reads: the loop's readiness view starts from the observed
    // state, deterministically.
    let mut connected = false;
    let mut joined = false;
    let deadline = std::time::Instant::now() + WAIT;
    while !(connected && joined) {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("the worker never became ready");
        match event {
            IrcEvent::Connection(_, ConnectionState::Connected) => connected = true,
            IrcEvent::Channel(
                _,
                channel,
                ChannelStatus {
                    state: ChannelState::Joined,
                    ..
                },
            ) if channel == "#test" => joined = true,
            _ => {}
        }
    }
    tx.send(IrcEvent::Connection(
        "libera".to_string(),
        ConnectionState::Connected,
    ))
    .unwrap();
    tx.send(IrcEvent::Channel(
        "libera".to_string(),
        "#test".to_string(),
        ChannelStatus {
            state: ChannelState::Joined,
            desired: true,
        },
    ))
    .unwrap();
    drop(tx);
    (rx, workers)
}

/// Run a stdio session over pre-written input, returning the session's
/// exit outcome and everything the loop printed on stdout and stderr.
fn session(
    rx: mpsc::Receiver<IrcEvent>,
    workers: std::collections::HashMap<String, termirc::connection::ConnectionHandle>,
    lines: Vec<Input>,
) -> (termirc::stdio::StdioExit, String, String) {
    let (input_tx, input_rx) = mpsc::channel();
    for line in lines {
        input_tx.send(line).unwrap();
    }
    drop(input_tx);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = stdio::run_with(workers, rx, input_rx, &mut out, &mut err).unwrap();
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// Where the first line starting with `prefix` sits in `lines`.
fn position(lines: &[String], prefix: &str) -> Option<usize> {
    lines.iter().position(|line| line.starts_with(prefix))
}

/// Every line the mock has seen so far, once the wire has gone quiet.
fn wire_lines(lines: &mpsc::Receiver<String>) -> Vec<String> {
    let quiet = Duration::from_millis(300);
    let mut seen = Vec::new();
    while let Ok(line) = lines.recv_timeout(quiet) {
        seen.push(line);
    }
    seen
}

#[test]
fn a_chat_line_reaches_the_wire_as_a_privmsg_after_the_join() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act
    let (_, _out, err) = session(
        rx,
        workers,
        vec![
            Input::Line(b"libera/#test hello world".to_vec()),
            Input::Eof,
        ],
    );

    // Assert: delivered on the wire and nothing refused.
    let lines = wire_lines(&lines_rx);
    assert!(
        lines
            .iter()
            .any(|line| line == "PRIVMSG #test :hello world"),
        "the chat never reached the wire: {lines:?}"
    );
    assert!(err.is_empty(), "stderr: {err:?}");
}

#[test]
fn a_raw_join_reaches_the_wire_verbatim() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act
    let (_, out, err) = session(
        rx,
        workers,
        vec![Input::Line(b"libera JOIN #extra".to_vec()), Input::Eof],
    );

    // Assert
    let lines = wire_lines(&lines_rx);
    assert!(
        lines.iter().any(|line| line == "JOIN #extra"),
        "the raw JOIN never reached the wire: {lines:?}"
    );
    assert!(err.is_empty(), "stderr: {err:?}");
    assert!(!out.is_empty(), "the JOIN echo should have been printed");
}

#[test]
fn a_whois_numeric_is_printed_as_an_info_line() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act
    let (_, out, err) = session(
        rx,
        workers,
        vec![Input::Line(b"libera WHOIS alice".to_vec()), Input::Eof],
    );

    // Assert: both numerics restated as status lines, the routed text as is.
    assert!(err.is_empty(), "stderr: {err:?}");
    assert!(
        out.lines().any(|line| line.contains("Real Name")),
        "stdout: {out:?}"
    );
    assert!(
        out.lines().any(|line| line.contains("End of WHOIS")),
        "stdout: {out:?}"
    );
    assert!(
        wire_lines(&lines_rx)
            .iter()
            .any(|line| line == "WHOIS alice"),
        "the WHOIS never reached the wire"
    );
}

#[test]
fn a_line_for_an_unjoined_channel_is_refused_and_never_queued() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act
    let (exit, _out, err) = session(
        rx,
        workers,
        vec![Input::Line(b"libera/#other hi".to_vec()), Input::Eof],
    );

    // Assert: refused on stderr, nothing on the wire beyond the configured
    // JOIN. The line and the EOF land before the loop has consumed the
    // worker's readiness events, so the refusal is decided after EOF: the
    // exit code says the piped line went nowhere.
    assert!(err.contains("could not join #other"), "stderr: {err:?}");
    assert_eq!(exit, termirc::stdio::StdioExit::Undelivered);
    let lines = wire_lines(&lines_rx);
    assert!(
        !lines.iter().any(|line| line.starts_with("PRIVMSG")),
        "a refused line reached the wire: {lines:?}"
    );
}

#[test]
fn eof_ends_the_session_with_a_quit_and_an_ok_run() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act: no lines at all — pure EOF.
    let (exit, _out, err) = session(rx, workers, vec![Input::Eof]);

    // Assert: the worker quit, nothing was refused, clean exit.
    assert!(err.is_empty(), "stderr: {err:?}");
    assert_eq!(exit, termirc::stdio::StdioExit::Clean);
    assert!(
        wire_lines(&lines_rx)
            .iter()
            .any(|line| line.starts_with("QUIT")),
        "EOF never produced a QUIT"
    );
}

#[test]
fn a_refusal_after_eof_exits_three() {
    // Arrange: a worker that is still connecting, so the line and the EOF
    // land before any readiness is known — the refusal can only come from
    // a later worker event.
    let (port, lines_rx) = spawn_mock();
    let (tx, rx) = mpsc::channel();
    let config = server_config_for(port);
    let server = config.servers.get("libera").unwrap().clone();
    let (_worker, handle) = spawn_irc_with_policy(
        server,
        "libera".to_string(),
        vec!["#test".to_string()],
        tx,
        policy(),
    );
    let mut workers = std::collections::HashMap::new();
    workers.insert("libera".to_string(), handle);

    // Act
    let (exit, _out, err) = session(
        rx,
        workers,
        vec![Input::Line(b"libera/#other hi".to_vec()), Input::Eof],
    );

    // Assert: the worker connects and joins #test, and once its joins have
    // settled, `#other` is refused for good — after EOF, so the exit code
    // says the piped line went nowhere.
    assert_eq!(exit, termirc::stdio::StdioExit::Undelivered);
    assert!(err.contains("could not join #other"), "stderr: {err:?}");
    assert!(
        !wire_lines(&lines_rx)
            .iter()
            .any(|line| line.starts_with("PRIVMSG")),
        "a refused line reached the wire"
    );
}

#[test]
fn a_line_queued_just_before_eof_is_still_sent_before_the_quit() {
    // Arrange
    let (port, lines_rx) = spawn_mock();
    let (rx, workers) = connected_worker(port);

    // Act: the line and the EOF arrive in the same batch — the biased
    // worker select would QUIT past a queued message if the drain did not
    // wait for its verdict.
    let (_, _out, err) = session(
        rx,
        workers,
        vec![Input::Line(b"libera/#test last words".to_vec()), Input::Eof],
    );

    // Assert: PRIVMSG precedes QUIT on the wire.
    assert!(err.is_empty(), "stderr: {err:?}");
    let lines = wire_lines(&lines_rx);
    let privmsg = position(&lines, "PRIVMSG #test :last words")
        .expect("the message queued before EOF must still be sent");
    let quit = position(&lines, "QUIT").expect("the worker must still quit");
    assert!(
        privmsg < quit,
        "the QUIT overtook a queued message (PRIVMSG at {privmsg}, QUIT at {quit}): {lines:?}"
    );
}
