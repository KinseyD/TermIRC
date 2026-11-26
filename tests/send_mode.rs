//! Integration tests for the `send` subcommand: the one-shot sender against
//! a mock server over a real socket — one PRIVMSG where it belongs, a QUIT
//! after it, and the failure class for everything that can go wrong.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use termirc::config::Config;
use termirc::connection::RetryPolicy;
use termirc::protocol::SendError;
use termirc::send::{self, SendFailure};

/// How long an expected wire line or verdict may take before a test gives up.
const WAIT: Duration = Duration::from_secs(5);
/// Wire silence that means a line which should not exist is absent.
const QUIET: Duration = Duration::from_millis(300);

/// What the mock answers a JOIN with.
#[derive(Clone, Copy)]
enum Join {
    /// Echo it back, so the channel is joined.
    Confirm,
    /// Refuse it: 473, invite-only.
    Refuse,
}

/// Start a mock IRC server on an ephemeral port.
///
/// Returns the port and a receiver streaming every line the client sends.
/// `greet` decides whether `USER` is answered with 001 — without it
/// registration never completes — and `join` is what a JOIN gets back.
fn spawn_mock(greet: bool, join: Join) -> (u16, mpsc::Receiver<String>) {
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
            if greet && trimmed.starts_with("USER") {
                write!(
                    writer,
                    ":mock 001 test :Welcome to the Mock IRC Network\r\n"
                )
                .unwrap();
            }
            if let Some(channel) = trimmed.strip_prefix("JOIN ") {
                let answer = match join {
                    Join::Confirm => format!(":test!u@h JOIN {channel}\r\n"),
                    Join::Refuse => {
                        format!(":mock 473 test {channel} :Cannot join channel (invite only)\r\n")
                    }
                };
                write!(writer, "{answer}").unwrap();
            }
            writer.flush().unwrap();
        }
    });
    (port, lines_rx)
}

/// A one-server configuration pointing at `port`; `send` names it `libera`.
fn config_for(port: u16) -> Config {
    Config::parse(&format!(
        r#"
[servers.libera]
username = "test"
nickname = "test"
password = ""
server = "127.0.0.1"
port = {port}
channels = []
"#
    ))
    .unwrap()
}

/// The worker timing every test runs under: one attempt, no delay worth
/// waiting for, and a short ceiling on connecting and joining.
fn policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        delay: Duration::from_millis(10),
        timeout: Duration::from_secs(3),
    }
}

/// Every line up to and including the first one that satisfies `done`,
/// panicking with the lines seen so far if none arrives in time.
fn lines_until(lines: &mpsc::Receiver<String>, done: impl Fn(&str) -> bool) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        let line = lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|error| {
                panic!("expected wire line never arrived: {error}; lines: {seen:?}")
            });
        let matched = done(&line);
        seen.push(line);
        if matched {
            return seen;
        }
    }
}

/// Every line the mock has seen, once the wire has been quiet for [`QUIET`].
fn quiet_lines(lines: &mpsc::Receiver<String>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(line) = lines.recv_timeout(QUIET) {
        seen.push(line);
    }
    seen
}

/// Where the first line starting with `prefix` sits in `lines`.
fn position(lines: &[String], prefix: &str) -> Option<usize> {
    lines.iter().position(|line| line.starts_with(prefix))
}

#[test]
fn a_channel_message_follows_its_join_on_the_wire_and_the_worker_quits() {
    // Arrange: the server key is matched ASCII case-insensitively.
    let (port, lines_rx) = spawn_mock(true, Join::Confirm);
    let config = config_for(port);

    // Act
    let result = send::run_with_policy(&config, "LIBERA/#test", "hello world", policy());

    // Assert: delivered, and in this order — the JOIN, the PRIVMSG, then
    // the QUIT the worker sends on its way out.
    assert_eq!(result, Ok(()));
    let lines = lines_until(&lines_rx, |line| line.starts_with("QUIT"));
    let join = position(&lines, "JOIN #test").expect("the JOIN must be sent");
    let privmsg = position(&lines, "PRIVMSG #test :hello world").expect("the message must be sent");
    let quit = position(&lines, "QUIT").expect("lines_until stops on the QUIT");
    assert!(
        join < privmsg && privmsg < quit,
        "wrong wire order (JOIN at {join}, PRIVMSG at {privmsg}, QUIT at {quit}): {lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("PRIVMSG"))
            .count(),
        1,
        "{lines:?}"
    );
}

#[test]
fn a_nickname_message_needs_no_join() {
    // Arrange
    let (port, lines_rx) = spawn_mock(true, Join::Confirm);
    let config = config_for(port);

    // Act
    let result = send::run_with_policy(&config, "libera/alice", "hi", policy());

    // Assert: one PRIVMSG straight to the nickname, and not a single JOIN.
    assert_eq!(result, Ok(()));
    let lines = lines_until(&lines_rx, |line| line.starts_with("QUIT"));
    // The crate writes the trailing `:` only when the text needs it (it
    // contains spaces, starts with `:`, or is empty), so a single word
    // travels bare — a valid PRIVMSG either way.
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("PRIVMSG"))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["PRIVMSG alice hi"],
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("JOIN")),
        "a nickname send joined something: {lines:?}"
    );
}

#[test]
fn a_refused_join_reports_the_servers_reason_without_sending() {
    // Arrange
    let (port, lines_rx) = spawn_mock(true, Join::Refuse);
    let config = config_for(port);

    // Act
    let result = send::run_with_policy(&config, "libera/#test", "hello world", policy());

    // Assert: exit 3 with the numeric's own wording, and no message on the
    // wire although the JOIN was attempted.
    assert_eq!(
        result,
        Err(SendFailure::NotJoined {
            server: "libera".to_string(),
            channel: "#test".to_string(),
            reason: Some("473: Cannot join channel (invite only)".to_string()),
        })
    );
    assert_eq!(result.unwrap_err().exit_code(), 3);
    let lines = quiet_lines(&lines_rx);
    assert!(
        lines.iter().any(|line| line == "JOIN #test"),
        "the JOIN must be attempted: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.starts_with("PRIVMSG")),
        "a refused send put a message on the wire: {lines:?}"
    );
}

#[test]
fn an_oversized_message_is_refused_before_anything_connects() {
    // Arrange: a listener nobody should ever reach.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = config_for(listener.local_addr().unwrap().port());

    // Act
    let result = send::run_with_policy(&config, "libera/#test", &"x".repeat(600), policy());

    // Assert
    assert_eq!(result, Err(SendFailure::Rejected(SendError::TooLong)));
    assert_eq!(result.unwrap_err().exit_code(), 2);
    assert!(
        listener.accept().is_err(),
        "a locally refused request still connected"
    );
}

#[test]
fn a_server_that_never_registers_fails_within_the_injected_timeout() {
    // Arrange: the socket is accepted and then left hanging, so only the
    // policy's timeout can end the attempt.
    let (port, _lines_rx) = spawn_mock(false, Join::Confirm);
    let config = config_for(port);
    let timeout = Duration::from_millis(300);

    // Act
    let started = Instant::now();
    let result = send::run_with_policy(
        &config,
        "libera/#test",
        "hello world",
        RetryPolicy {
            max_retries: 0,
            delay: Duration::from_millis(10),
            timeout,
        },
    );
    let elapsed = started.elapsed();

    // Assert: the worker's own reason, as exit 3, well before the test's
    // own patience would have run out.
    assert_eq!(
        result,
        Err(SendFailure::NotConnected {
            server: "libera".to_string(),
            reason: Some("connection or registration failed".to_string()),
        })
    );
    assert_eq!(result.unwrap_err().exit_code(), 3);
    assert!(
        elapsed < WAIT,
        "took {elapsed:?}, far past the {timeout:?} timeout"
    );
}

#[test]
fn a_raw_command_to_a_bare_server_goes_out_and_the_run_succeeds() {
    // Arrange: the address names the server alone, so the payload is one
    // raw IRC command line.
    let (port, lines_rx) = spawn_mock(true, Join::Confirm);
    let config = config_for(port);

    // Act
    let result = send::run_with_policy(&config, "libera", "WHOIS alice", policy());

    // Assert: delivered verbatim, then the worker's QUIT.
    assert_eq!(result, Ok(()));
    let lines = lines_until(&lines_rx, |line| line.starts_with("QUIT"));
    let whois = position(&lines, "WHOIS alice").expect("the raw command must be sent");
    let quit = position(&lines, "QUIT").expect("lines_until stops on the QUIT");
    assert!(whois < quit, "wrong wire order: {lines:?}");
}

#[test]
fn a_raw_join_completes_on_its_control_acknowledgement() {
    // Arrange: no configured channels, so the raw JOIN's channel is new to
    // the registry and the command must reach the wire.
    let (port, lines_rx) = spawn_mock(true, Join::Confirm);
    let config = config_for(port);

    // Act
    let result = send::run_with_policy(&config, "libera", "JOIN #extra", policy());

    // Assert: the acknowledgement of the raw control is what completed the
    // send (the ordinary accepted verdict never fires for one), the JOIN
    // is on the wire, and the worker still quits cleanly.
    assert_eq!(result, Ok(()));
    let lines = lines_until(&lines_rx, |line| line.starts_with("QUIT"));
    assert!(
        lines.iter().any(|line| line == "JOIN #extra"),
        "the raw JOIN must reach the wire: {lines:?}"
    );
}
