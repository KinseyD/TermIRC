//! Headless one-shot send: the `send` subcommand connects to one configured
//! server, delivers one PRIVMSG, and exits with a scriptable code.
//!
//! Layer boundary: depends only on `core` message types, the `connection`
//! worker, `protocol` validation, and the `config` server table (plus std).
//! It owns no `Session`, no buffers, and no history: nothing it sends or
//! receives is recorded. Listen mode never sends; this is the one headless
//! path that does, and it sends exactly the message it was given — which no
//! event, failure, or log line ever quotes.
//!
//! The worker owns all timing (connect, registration, and JOIN deadlines
//! come from its [`RetryPolicy`]); this module only reads the worker's
//! events. Reading them is a pure event → decision step ([`Delivery`]), so
//! the unit tests drive it with synthetic events and no sockets.

use std::{
    fmt,
    sync::mpsc::{self, RecvTimeoutError},
    time::{Duration, Instant},
};

use crate::{
    config::{Config, ServerConfig},
    connection::{ConnectionHandle, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy},
    core::{ChannelState, MessageKind, OutgoingMessage, valid_channel_name, valid_query_nickname},
    i18n,
    protocol::{SendError, encode_outgoing},
};

/// How long the worker may take to report a queued message. It answers
/// within one loop turn, so this only bounds a wedged worker.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the worker may take to send its QUIT and drain before the
/// process is allowed to give up waiting for it.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to keep reading after a channel left `Joining` without a reason
/// yet. The worker's next events arrive at once, so only a server that said
/// nothing (the JOIN-timeout case) waits this out.
const REFUSAL_GRACE: Duration = Duration::from_millis(500);

/// Exit code of a request refused on its own terms: nothing was contacted.
const EXIT_LOCAL: i32 = 2;
/// Exit code of a message the worker tried to deliver and could not.
const EXIT_NOT_SENT: i32 = 3;

/// The policy of a real `send`: the worker's default connect timeout and
/// backoff, but no retries. A reconnect never replays queued messages, so a
/// retry could only hold back the failure a script is waiting for.
pub fn one_shot_policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        ..RetryPolicy::default()
    }
}

/// Why a one-shot send did not happen. Two classes, told apart by
/// [`SendFailure::exit_code`]: the request was never valid (nothing was
/// contacted), or the worker tried and the message was not delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendFailure {
    /// `--listen` and `send` are both headless but contradictory modes.
    ListenConflict,
    /// The message had to come from stdin, which could not be read as UTF-8.
    StdinUnreadable,
    /// The buffer address has no `/` between server and target.
    NoTarget { buffer: String },
    /// The server part of the address is not a `[servers.<name>]` key.
    UnknownServer { server: String },
    /// The target is neither a valid `#channel` nor a valid nickname.
    InvalidTarget { target: String },
    /// The message is empty once trimmed, like an empty composer.
    EmptyMessage,
    /// The wire encoding refused the message (a control character, or a
    /// line past the 512-byte limit).
    Rejected(SendError),
    /// The worker stopped before the message was queued, lost the
    /// connection after, or ended without a verdict; `reason` is the last
    /// status text it produced, when it produced one.
    NotConnected {
        server: String,
        reason: Option<String>,
    },
    /// The channel JOIN was refused or never confirmed; `reason` is the
    /// server's wording when it sent one.
    NotJoined {
        server: String,
        channel: String,
        reason: Option<String>,
    },
    /// The worker discarded the queued message without sending it.
    Dropped { server: String },
    /// The worker neither accepted nor dropped the queued message in time.
    AckTimeout { server: String },
    /// The worker did not finish its QUIT within the shutdown window.
    ShutdownTimeout { server: String },
}

impl SendFailure {
    /// `2` when the request itself was refused and nothing was contacted,
    /// `3` when the worker tried and the message was not delivered.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ListenConflict
            | Self::StdinUnreadable
            | Self::NoTarget { .. }
            | Self::UnknownServer { .. }
            | Self::InvalidTarget { .. }
            | Self::EmptyMessage
            | Self::Rejected(_) => EXIT_LOCAL,
            Self::NotConnected { .. }
            | Self::NotJoined { .. }
            | Self::Dropped { .. }
            | Self::AckTimeout { .. }
            | Self::ShutdownTimeout { .. } => EXIT_NOT_SENT,
        }
    }
}

impl fmt::Display for SendFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Resolved when printed, through the catalog startup loaded: the
        // language is a configuration choice made after this type exists.
        let text = match self {
            Self::ListenConflict => text("send.listen_conflict", &[]),
            Self::StdinUnreadable => text("send.stdin_read", &[]),
            Self::NoTarget { buffer } => text("send.no_target", &[("buffer", buffer)]),
            Self::UnknownServer { server } => text("send.unknown_server", &[("server", server)]),
            Self::InvalidTarget { target } => text("send.invalid_target", &[("target", target)]),
            Self::EmptyMessage => text("send.empty_message", &[]),
            Self::Rejected(error) => text("send.rejected", &[("reason", &error.to_string())]),
            Self::NotConnected {
                server,
                reason: Some(reason),
            } => text(
                "send.not_connected",
                &[("server", server), ("reason", reason)],
            ),
            Self::NotConnected {
                server,
                reason: None,
            } => text("send.not_connected_no_reason", &[("server", server)]),
            Self::NotJoined {
                server,
                channel,
                reason: Some(reason),
            } => text(
                "send.not_joined",
                &[("server", server), ("channel", channel), ("reason", reason)],
            ),
            Self::NotJoined {
                server,
                channel,
                reason: None,
            } => text(
                "send.not_joined_no_reason",
                &[("server", server), ("channel", channel)],
            ),
            Self::Dropped { server } => text("send.dropped", &[("server", server)]),
            Self::AckTimeout { server } => text(
                "send.ack_timeout",
                &[
                    ("server", server),
                    ("seconds", &ACK_TIMEOUT.as_secs().to_string()),
                ],
            ),
            Self::ShutdownTimeout { server } => text(
                "send.shutdown_timeout",
                &[
                    ("server", server),
                    ("seconds", &SHUTDOWN_TIMEOUT.as_secs().to_string()),
                ],
            ),
        };
        f.write_str(&text)
    }
}

/// Render one catalog template with its parameters.
fn text(key: &str, params: &[(&str, &str)]) -> String {
    i18n::format(i18n::t(key), params)
}

/// The usage error for a `send` invoked with `--listen`: both are headless,
/// but listen mode promises that nothing is ever sent, so it cannot wrap a
/// send. clap cannot refuse the pair itself (`--config` has to stay valid
/// beside the subcommand), so `main` asks here before anything connects.
pub fn listen_conflict(listen: bool) -> Option<SendFailure> {
    listen.then_some(SendFailure::ListenConflict)
}

/// Send one message with the one-shot policy and return once the worker has
/// sent its QUIT. Nothing is printed; [`SendFailure::exit_code`] is the
/// scriptable verdict and [`fmt::Display`] the text to report.
pub fn run(config: &Config, buffer: &str, message: &str) -> Result<(), SendFailure> {
    run_with_policy(config, buffer, message, one_shot_policy())
}

/// [`run`] with the worker's retry and timeout policy stated explicitly: the
/// seam tests use to bound a send that is expected to fail.
pub fn run_with_policy(
    config: &Config,
    buffer: &str,
    message: &str,
    policy: RetryPolicy,
) -> Result<(), SendFailure> {
    let Prepared {
        server,
        label,
        message,
        channel,
    } = prepare(config, buffer, message)?;
    let (tx, rx) = mpsc::channel();
    let (_worker, handle) = spawn_irc_with_policy(
        server,
        label.clone(),
        channel.iter().cloned().collect(),
        tx,
        policy,
    );
    let mut delivery = Delivery::new(&label, channel.as_deref());
    let outcome = deliver(&rx, &handle, &message, &mut delivery);
    // Closing the control channel is how a caller asks a worker to QUIT and
    // drain. After a failure that releases a worker which may still be
    // connected; after success it is the step `wait_for_shutdown` times.
    drop(handle);
    match outcome {
        Ok(()) => wait_for_shutdown(&rx, &label),
        Err(failure) => Err(failure),
    }
}

/// Everything checked before a worker exists, so an invalid request never
/// opens a socket.
struct Prepared {
    server: ServerConfig,
    /// The `[servers.<name>]` key as written: the label on every worker event.
    label: String,
    message: OutgoingMessage,
    /// The channel to join first; `None` for a nickname, which needs only a
    /// registered connection.
    channel: Option<String>,
}

/// Validate the request the way the composer would — a channel or a
/// nickname, text trimmed and non-empty — and then against the wire with the
/// same [`encode_outgoing`] the worker runs, so there is one validation rule
/// rather than two.
fn prepare(config: &Config, buffer: &str, message: &str) -> Result<Prepared, SendFailure> {
    let (name, target) = buffer
        .split_once('/')
        .ok_or_else(|| SendFailure::NoTarget {
            buffer: buffer.to_owned(),
        })?;
    // Config validation already refuses two keys that differ only in ASCII
    // case, so this finds at most one entry.
    let (label, server) = config
        .servers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .ok_or_else(|| SendFailure::UnknownServer {
            server: name.to_owned(),
        })?;
    let is_channel = target.starts_with(['#', '&']);
    let valid = if is_channel {
        valid_channel_name(target)
    } else {
        valid_query_nickname(target)
    };
    if !valid {
        return Err(SendFailure::InvalidTarget {
            target: target.to_owned(),
        });
    }
    let text = message.trim();
    if text.is_empty() {
        return Err(SendFailure::EmptyMessage);
    }
    let outgoing = OutgoingMessage::Privmsg {
        server: label.clone(),
        target: target.to_owned(),
        text: text.to_owned(),
    };
    encode_outgoing(&outgoing).map_err(SendFailure::Rejected)?;
    Ok(Prepared {
        server: server.clone(),
        label: label.clone(),
        message: outgoing,
        channel: is_channel.then(|| target.to_owned()),
    })
}

/// Queue the message once the connection is ready, then wait for the
/// worker's verdict.
fn deliver(
    rx: &mpsc::Receiver<IrcEvent>,
    handle: &ConnectionHandle,
    message: &OutgoingMessage,
    delivery: &mut Delivery,
) -> Result<(), SendFailure> {
    // Set once the message is queued: from then on the worker owes a prompt
    // answer, and the wait is bounded here as well as by the worker.
    let mut ack_deadline: Option<Instant> = None;
    loop {
        let event = match ack_deadline {
            // Before the queue the worker's own deadlines bound the wait.
            None => rx.recv().map_err(|_| delivery.not_connected())?,
            Some(deadline) => {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => {
                        return Err(delivery.ack_timeout());
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(delivery.not_connected());
                    }
                }
            }
        };
        match delivery.on_event(&event) {
            Step::Wait => {}
            Step::Enqueue => {
                // The queue is empty and holds dozens of messages, so a
                // refusal here can only mean the worker is already gone.
                if handle.outgoing.try_send(message.clone()).is_err() {
                    return Err(delivery.not_connected());
                }
                ack_deadline = Some(Instant::now() + ACK_TIMEOUT);
            }
            Step::Done => return Ok(()),
            Step::Fail(failure) => {
                // A new attempt under way withdraws the failure: keep waiting.
                if let Some(failure) = settle(failure, rx, delivery) {
                    return Err(failure);
                }
            }
        }
    }
}

/// Conclude a channel that left `Joining` from what the worker says next.
///
/// The same `NotJoined` can mean a refused JOIN or a socket that never got
/// that far: the channel registry reports a lost connection's channels
/// before the run loop reports the connection. Reading on tells them apart —
/// a refusal is followed by the server's numeric, a lost connection by its
/// own verdict, and a new attempt by `Connecting`, in which case the send
/// keeps waiting. Returns the failure to report, or `None` to keep waiting.
fn settle(
    failure: SendFailure,
    rx: &mpsc::Receiver<IrcEvent>,
    delivery: &mut Delivery,
) -> Option<SendFailure> {
    if !matches!(failure, SendFailure::NotJoined { .. }) {
        return Some(failure);
    }
    let mut failure = failure;
    let deadline = Instant::now() + REFUSAL_GRACE;
    while matches!(failure, SendFailure::NotJoined { reason: None, .. }) {
        let event = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(event) => event,
            Err(RecvTimeoutError::Disconnected) => return Some(delivery.not_connected()),
            // Nobody said why; the generic reason stands.
            Err(RecvTimeoutError::Timeout) => break,
        };
        if matches!(&event, IrcEvent::Connection(server, ConnectionState::Connecting) if delivery.ours(server))
        {
            return None;
        }
        // The channel was lost with its connection: that is the real
        // failure, status text included.
        if let Step::Fail(found @ SendFailure::NotConnected { .. }) = delivery.on_event(&event) {
            return Some(found);
        }
        if let SendFailure::NotJoined { reason, .. } = &mut failure {
            reason.clone_from(&server_reason(&event));
        }
    }
    Some(failure)
}

/// The server's wording of an error numeric, `473: Cannot join channel
/// (invite only)`; `None` for anything that is not one.
fn server_reason(event: &IrcEvent) -> Option<String> {
    let IrcEvent::Message(message) = event else {
        return None;
    };
    let MessageKind::Error { code, reason, .. } = &message.content.kind else {
        return None;
    };
    Some(if reason.is_empty() {
        code.to_string()
    } else {
        format!("{code}: {reason}")
    })
}

/// Wait for the worker to finish `close()` (QUIT and drain). Every sender
/// is gone exactly when the worker's loop has ended, so `recv` failing is
/// the completion signal; whatever it emitted on the way out is not read.
fn wait_for_shutdown(rx: &mpsc::Receiver<IrcEvent>, label: &str) -> Result<(), SendFailure> {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(_) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {
                return Err(SendFailure::ShutdownTimeout {
                    server: label.to_owned(),
                });
            }
        }
    }
}

/// What the send is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// A usable connection: registration for a nickname, the JOIN
    /// confirmation for a channel.
    Waiting,
    /// The worker's verdict on the queued message.
    Queued,
}

/// What one worker event means for the pending message.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing decisive: keep reading events.
    Wait,
    /// The connection is ready: queue the message now.
    Enqueue,
    /// The worker took the message; only its exit is left to wait for.
    Done,
    /// The message will not be delivered.
    Fail(SendFailure),
}

/// The event → decision step of a one-shot send. Pure: the same events in
/// the same order always give the same steps, which is what the unit tests
/// pin without a socket.
struct Delivery {
    /// The config key every event of this worker carries.
    label: String,
    /// The channel being joined; `None` for a nickname.
    channel: Option<String>,
    phase: Phase,
    /// The last `Error`/`Status` text the worker produced: the reason a
    /// connection failure reports.
    note: Option<String>,
}

impl Delivery {
    fn new(label: &str, channel: Option<&str>) -> Self {
        Self {
            label: label.to_owned(),
            channel: channel.map(str::to_owned),
            phase: Phase::Waiting,
            note: None,
        }
    }

    fn ours(&self, server: &str) -> bool {
        server.eq_ignore_ascii_case(&self.label)
    }

    fn is_channel(&self, channel: &str) -> bool {
        // Identities compare ASCII case-insensitively, like everywhere else.
        self.channel
            .as_deref()
            .is_some_and(|target| target.eq_ignore_ascii_case(channel))
    }

    fn not_connected(&self) -> SendFailure {
        SendFailure::NotConnected {
            server: self.label.clone(),
            reason: self.note.clone(),
        }
    }

    fn ack_timeout(&self) -> SendFailure {
        SendFailure::AckTimeout {
            server: self.label.clone(),
        }
    }

    /// Read one worker event. Saying [`Step::Enqueue`] moves to
    /// [`Phase::Queued`]: from then on every event is news about the queued
    /// message.
    fn on_event(&mut self, event: &IrcEvent) -> Step {
        match event {
            IrcEvent::Error(server, text) | IrcEvent::Status(server, text) => {
                if self.ours(server) {
                    self.note = Some(text.clone());
                }
                Step::Wait
            }
            IrcEvent::Connection(server, state) if self.ours(server) => self.on_connection(*state),
            IrcEvent::Channel(server, channel, status)
                if self.ours(server) && self.is_channel(channel) =>
            {
                self.on_channel(status.state, channel)
            }
            IrcEvent::OutgoingAccepted(server)
                if self.phase == Phase::Queued && self.ours(server) =>
            {
                Step::Done
            }
            IrcEvent::OutgoingDropped(_) if self.phase == Phase::Queued => {
                Step::Fail(SendFailure::Dropped {
                    server: self.label.clone(),
                })
            }
            _ => Step::Wait,
        }
    }

    fn on_connection(&mut self, state: ConnectionState) -> Step {
        match (self.phase, state) {
            // A queued message never survives a change of socket, and the
            // worker never re-announces the state it is already in, so any
            // transition by now means the message is lost.
            (Phase::Queued, _) => Step::Fail(self.not_connected()),
            (Phase::Waiting, ConnectionState::Stopped) => Step::Fail(self.not_connected()),
            // A nickname needs registration only; a channel keeps waiting
            // for its JOIN confirmation.
            (Phase::Waiting, ConnectionState::Connected) if self.channel.is_none() => self.queue(),
            (Phase::Waiting, _) => Step::Wait,
        }
    }

    fn on_channel(&mut self, state: ChannelState, channel: &str) -> Step {
        if self.phase == Phase::Queued {
            // The verdict on a queued message is the worker's to report,
            // and it follows; channel traffic meanwhile decides nothing.
            return Step::Wait;
        }
        match state {
            ChannelState::Joined => self.queue(),
            // The worker's own progress towards the confirmation.
            ChannelState::Joining => Step::Wait,
            // Refused (a 4xx numeric put the channel back to `NotJoined`),
            // unconfirmed when the JOIN deadline expired (`Uncertain`), or
            // lost.
            ChannelState::NotJoined | ChannelState::Parting | ChannelState::Uncertain => {
                Step::Fail(SendFailure::NotJoined {
                    server: self.label.clone(),
                    channel: channel.to_owned(),
                    reason: None,
                })
            }
        }
    }

    /// Move to [`Phase::Queued`]: from here on, events describe the queued
    /// message.
    fn queue(&mut self) -> Step {
        self.phase = Phase::Queued;
        Step::Enqueue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{BufferKind, ChannelStatus, MessageContent, RoutedMessage, ServerId};

    const TEXT: &str = "hello world";

    fn config() -> Config {
        Config::parse(
            r#"
[servers.libera]
username = "me"
nickname = "me"
password = ""
server = "127.0.0.1"
port = 6667
channels = []
"#,
        )
        .unwrap()
    }

    /// The refusal `prepare` must produce. A prepared request is a test
    /// failure rather than something to debug-print: it carries the text.
    fn refusal(config: &Config, buffer: &str, message: &str) -> SendFailure {
        match prepare(config, buffer, message) {
            Ok(_) => panic!("{buffer:?} must be refused before a worker exists"),
            Err(failure) => failure,
        }
    }

    fn connection(state: ConnectionState) -> IrcEvent {
        IrcEvent::Connection("srv".to_string(), state)
    }

    fn channel(state: ChannelState) -> IrcEvent {
        IrcEvent::Channel(
            "srv".to_string(),
            "#Test".to_string(),
            ChannelStatus {
                state,
                desired: true,
            },
        )
    }

    /// A routed error numeric, as the worker decodes a 4xx reply.
    fn error_message(code: u16, reason: &str) -> IrcEvent {
        IrcEvent::Message(RoutedMessage {
            server: ServerId::new("srv"),
            target: BufferKind::Server,
            content: MessageContent {
                kind: MessageKind::Error {
                    code,
                    target: None,
                    reason: reason.to_owned(),
                },
                ..MessageContent::console("")
            },
        })
    }

    /// A delivery whose message is already queued.
    fn queued(channel: Option<&str>) -> Delivery {
        let mut delivery = Delivery::new("srv", channel);
        let ready = if channel.is_some() {
            self::channel(ChannelState::Joined)
        } else {
            connection(ConnectionState::Connected)
        };
        assert_eq!(delivery.on_event(&ready), Step::Enqueue);
        delivery
    }

    /// A channel whose JOIN ended badly mid-wait: the state `settle` starts
    /// from, with the failure the channel event produced.
    fn unjoined() -> (Delivery, SendFailure) {
        let mut delivery = Delivery::new("srv", Some("#test"));
        assert_eq!(
            delivery.on_event(&channel(ChannelState::Joining)),
            Step::Wait
        );
        let Step::Fail(failure) = delivery.on_event(&channel(ChannelState::NotJoined)) else {
            panic!("leaving Joining without joining must fail the send");
        };
        (delivery, failure)
    }

    /// Settle `failure` against the events already waiting on a channel.
    fn settle_with(
        events: Vec<IrcEvent>,
        delivery: &mut Delivery,
        failure: SendFailure,
    ) -> Option<SendFailure> {
        let (tx, rx) = mpsc::channel();
        for event in events {
            tx.send(event).unwrap();
        }
        settle(failure, &rx, delivery)
    }

    fn not_connected(reason: Option<&str>) -> SendFailure {
        SendFailure::NotConnected {
            server: "srv".to_string(),
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn the_one_shot_policy_never_retries_and_keeps_the_worker_defaults() {
        let policy = one_shot_policy();
        assert_eq!(policy.max_retries, 0);
        assert_eq!(policy.delay, RetryPolicy::default().delay);
        assert_eq!(policy.timeout, RetryPolicy::default().timeout);
    }

    #[test]
    fn a_valid_request_is_trimmed_like_the_composer_and_keeps_its_destination() {
        // Arrange
        let config = config();

        // Act
        let to_channel = prepare(&config, "LIBERA/#Test", "  hello world  ").unwrap();
        let to_nickname = prepare(&config, "libera/Alice", "hi").unwrap();

        // Assert: the config key as written is the label, the destination
        // keeps its case, and the text is trimmed exactly as the composer
        // sends it.
        assert_eq!(to_channel.label, "libera");
        assert_eq!(to_channel.channel.as_deref(), Some("#Test"));
        assert!(matches!(
            &to_channel.message,
            OutgoingMessage::Privmsg { server, target, text }
                if server == "libera" && target == "#Test" && text == TEXT
        ));
        assert_eq!(to_nickname.channel, None);
        assert!(matches!(
            &to_nickname.message,
            OutgoingMessage::Privmsg { target, .. } if target == "Alice"
        ));
    }

    #[test]
    fn invalid_requests_are_refused_before_a_worker_exists() {
        // Arrange: every local rejection class, each a script's exit 2.
        let config = config();
        let too_long = "x".repeat(600);
        let cases = [
            (
                "libera",
                TEXT,
                SendFailure::NoTarget {
                    buffer: "libera".to_string(),
                },
            ),
            (
                "nowhere/#test",
                TEXT,
                SendFailure::UnknownServer {
                    server: "nowhere".to_string(),
                },
            ),
            (
                "libera/#bad,name",
                TEXT,
                SendFailure::InvalidTarget {
                    target: "#bad,name".to_string(),
                },
            ),
            (
                "libera/bad nick",
                TEXT,
                SendFailure::InvalidTarget {
                    target: "bad nick".to_string(),
                },
            ),
            (
                "libera/",
                TEXT,
                SendFailure::InvalidTarget {
                    target: String::new(),
                },
            ),
            ("libera/#chan", " \t ", SendFailure::EmptyMessage),
            (
                "libera/#chan",
                "a\nb",
                SendFailure::Rejected(SendError::InvalidCharacters),
            ),
            (
                "libera/#chan",
                too_long.as_str(),
                SendFailure::Rejected(SendError::TooLong),
            ),
        ];

        // Act / Assert
        for (buffer, message, expected) in cases {
            let failure = refusal(&config, buffer, message);
            assert_eq!(failure, expected, "buffer {buffer:?}");
            assert_eq!(failure.exit_code(), EXIT_LOCAL, "buffer {buffer:?}");
        }
    }

    #[test]
    fn a_channel_target_queues_only_once_its_join_is_confirmed() {
        // Arrange
        let mut delivery = Delivery::new("srv", Some("#test"));

        // Act / Assert: every step towards the JOIN is progress, not a
        // decision; the confirmation queues, compared case-insensitively.
        for event in [
            connection(ConnectionState::Connecting),
            connection(ConnectionState::Connected),
            channel(ChannelState::Joining),
        ] {
            assert_eq!(delivery.on_event(&event), Step::Wait, "event: {event:?}");
        }
        assert_eq!(
            delivery.on_event(&channel(ChannelState::Joined)),
            Step::Enqueue
        );
        assert_eq!(delivery.phase, Phase::Queued);
        assert_eq!(
            delivery.on_event(&IrcEvent::OutgoingAccepted("srv".to_string())),
            Step::Done
        );
    }

    #[test]
    fn a_nickname_target_queues_on_registration() {
        // Arrange
        let mut delivery = Delivery::new("srv", None);

        // Act / Assert
        assert_eq!(
            delivery.on_event(&connection(ConnectionState::Connecting)),
            Step::Wait
        );
        assert_eq!(
            delivery.on_event(&connection(ConnectionState::Connected)),
            Step::Enqueue
        );
        assert_eq!(
            delivery.on_event(&IrcEvent::OutgoingAccepted("srv".to_string())),
            Step::Done
        );
    }

    #[test]
    fn a_channel_that_leaves_joining_without_joining_fails_with_its_name() {
        for state in [
            ChannelState::NotJoined,
            ChannelState::Parting,
            ChannelState::Uncertain,
        ] {
            // Arrange
            let mut delivery = Delivery::new("srv", Some("#test"));
            assert_eq!(
                delivery.on_event(&channel(ChannelState::Joining)),
                Step::Wait
            );

            // Act
            let step = delivery.on_event(&channel(state));

            // Assert
            assert_eq!(
                step,
                Step::Fail(SendFailure::NotJoined {
                    server: "srv".to_string(),
                    channel: "#Test".to_string(),
                    reason: None,
                }),
                "state: {state:?}"
            );
            assert_eq!(
                match step {
                    Step::Fail(failure) => failure.exit_code(),
                    step => panic!("a lost channel must fail the send, got {step:?}"),
                },
                EXIT_NOT_SENT,
                "state: {state:?}"
            );
        }
    }

    #[test]
    fn a_stopped_connection_fails_with_the_last_worker_status() {
        // Arrange
        let mut delivery = Delivery::new("srv", Some("#test"));
        assert_eq!(
            delivery.on_event(&connection(ConnectionState::Connecting)),
            Step::Wait
        );
        assert_eq!(
            delivery.on_event(&IrcEvent::Error(
                "srv".to_string(),
                "sasl authentication failed".to_string()
            )),
            Step::Wait
        );
        assert_eq!(
            delivery.on_event(&IrcEvent::Error(
                "srv".to_string(),
                "connection or registration failed".to_string()
            )),
            Step::Wait
        );

        // Act
        let step = delivery.on_event(&connection(ConnectionState::Stopped));

        // Assert: the worker's own words, the latest ones, are the reason.
        let Step::Fail(failure) = step else {
            panic!("a stopped worker must fail the send");
        };
        assert_eq!(
            failure,
            not_connected(Some("connection or registration failed"))
        );
        assert_eq!(failure.exit_code(), EXIT_NOT_SENT);

        // A worker that never said anything leaves the failure reasonless.
        let mut delivery = Delivery::new("srv", None);
        let Step::Fail(failure) = delivery.on_event(&connection(ConnectionState::Stopped)) else {
            panic!("a stopped worker must fail the send");
        };
        assert_eq!(failure, not_connected(None));
    }

    #[test]
    fn once_queued_only_the_verdict_or_a_connection_change_decides() {
        // Only the worker's verdict is good news.
        let mut delivery = queued(Some("#test"));
        assert_eq!(
            delivery.on_event(&IrcEvent::OutgoingAccepted("srv".to_string())),
            Step::Done
        );

        // A discard is the worker's own negative verdict.
        let mut delivery = queued(Some("#test"));
        assert_eq!(
            delivery.on_event(&IrcEvent::OutgoingDropped("srv".to_string())),
            Step::Fail(SendFailure::Dropped {
                server: "srv".to_string()
            })
        );

        // A queued message never survives a connection transition, whichever
        // way it goes.
        for state in [
            ConnectionState::Connecting,
            ConnectionState::Connected,
            ConnectionState::Stopped,
        ] {
            let mut delivery = queued(None);
            let Step::Fail(failure) = delivery.on_event(&connection(state)) else {
                panic!("a connection change must fail a queued message");
            };
            assert_eq!(failure, not_connected(None), "state: {state:?}");
        }

        // Everything else is noise: the verdict is still on its way.
        let mut delivery = queued(Some("#test"));
        for event in [
            channel(ChannelState::NotJoined),
            error_message(473, "Invite only"),
            IrcEvent::Nickname("srv".to_string(), "new".to_string()),
        ] {
            assert_eq!(delivery.on_event(&event), Step::Wait, "event: {event:?}");
        }
        assert_eq!(
            delivery.on_event(&IrcEvent::OutgoingAccepted("srv".to_string())),
            Step::Done
        );
    }

    #[test]
    fn another_servers_events_and_another_channels_state_decide_nothing() {
        // Arrange / Act / Assert
        let mut delivery = Delivery::new("srv", Some("#test"));
        for event in [
            IrcEvent::Connection("other".to_string(), ConnectionState::Stopped),
            IrcEvent::Channel(
                "other".to_string(),
                "#test".to_string(),
                ChannelStatus::default(),
            ),
            IrcEvent::Channel(
                "srv".to_string(),
                "#other".to_string(),
                ChannelStatus::default(),
            ),
            IrcEvent::OutgoingAccepted("other".to_string()),
            IrcEvent::OutgoingDropped("other".to_string()),
        ] {
            assert_eq!(delivery.on_event(&event), Step::Wait, "event: {event:?}");
        }

        // A nickname send has no channel to wait for, so no channel state
        // is ever a decision for it.
        let mut delivery = Delivery::new("srv", None);
        assert_eq!(
            delivery.on_event(&channel(ChannelState::Joined)),
            Step::Wait
        );
        assert_eq!(
            delivery.on_event(&channel(ChannelState::NotJoined)),
            Step::Wait
        );
    }

    #[test]
    fn exit_codes_separate_locally_refused_requests_from_undelivered_messages() {
        let locally_refused = [
            SendFailure::ListenConflict,
            SendFailure::StdinUnreadable,
            SendFailure::NoTarget {
                buffer: String::new(),
            },
            SendFailure::UnknownServer {
                server: String::new(),
            },
            SendFailure::InvalidTarget {
                target: String::new(),
            },
            SendFailure::EmptyMessage,
            SendFailure::Rejected(SendError::TooLong),
        ];
        let not_delivered = [
            not_connected(None),
            SendFailure::NotJoined {
                server: String::new(),
                channel: String::new(),
                reason: None,
            },
            SendFailure::Dropped {
                server: String::new(),
            },
            SendFailure::AckTimeout {
                server: String::new(),
            },
            SendFailure::ShutdownTimeout {
                server: String::new(),
            },
        ];
        // The values themselves are the contract with scripts.
        assert_eq!(EXIT_LOCAL, 2);
        assert_eq!(EXIT_NOT_SENT, 3);
        for failure in locally_refused {
            assert_eq!(failure.exit_code(), EXIT_LOCAL, "{failure:?}");
        }
        for failure in not_delivered {
            assert_eq!(failure.exit_code(), EXIT_NOT_SENT, "{failure:?}");
        }
    }

    #[test]
    fn failures_render_through_the_catalog_with_their_parameters() {
        for (failure, expected) in [
            (
                SendFailure::ListenConflict,
                "--listen never sends, so it cannot be combined with send".to_string(),
            ),
            (
                SendFailure::StdinUnreadable,
                "could not read the message from stdin".to_string(),
            ),
            (
                SendFailure::NoTarget {
                    buffer: "libera".to_string(),
                },
                "expected server/target, got \"libera\"".to_string(),
            ),
            (
                SendFailure::UnknownServer {
                    server: "libera".to_string(),
                },
                "server \"libera\" is not in the config".to_string(),
            ),
            (
                SendFailure::InvalidTarget {
                    target: "#bad".to_string(),
                },
                "\"#bad\" is not a valid #channel or nickname".to_string(),
            ),
            (
                SendFailure::EmptyMessage,
                "the message is empty".to_string(),
            ),
            (
                SendFailure::Rejected(SendError::TooLong),
                "the message was rejected: message exceeds the 512-byte IRC line limit".to_string(),
            ),
            (
                not_connected(Some("connection refused")),
                "srv is not connected: connection refused".to_string(),
            ),
            (not_connected(None), "srv is not connected".to_string()),
            (
                SendFailure::NotJoined {
                    server: "libera".to_string(),
                    channel: "#test".to_string(),
                    reason: Some("473: Invite only".to_string()),
                },
                "could not join #test on libera: 473: Invite only".to_string(),
            ),
            (
                SendFailure::NotJoined {
                    server: "libera".to_string(),
                    channel: "#test".to_string(),
                    reason: None,
                },
                "could not join #test on libera: no reason given".to_string(),
            ),
            (
                SendFailure::Dropped {
                    server: "libera".to_string(),
                },
                "libera discarded the message without sending it".to_string(),
            ),
            (
                SendFailure::AckTimeout {
                    server: "libera".to_string(),
                },
                format!(
                    "libera did not confirm the message within {} seconds",
                    ACK_TIMEOUT.as_secs()
                ),
            ),
            (
                SendFailure::ShutdownTimeout {
                    server: "libera".to_string(),
                },
                format!(
                    "libera did not finish disconnecting within {} seconds",
                    SHUTDOWN_TIMEOUT.as_secs()
                ),
            ),
        ] {
            assert_eq!(failure.to_string(), expected, "{failure:?}");
        }
    }

    #[test]
    fn the_servers_numeric_becomes_the_refusal_reason() {
        assert_eq!(
            server_reason(&error_message(473, "Cannot join channel (invite only)")).as_deref(),
            Some("473: Cannot join channel (invite only)")
        );
        // A numeric with no text still names its code.
        assert_eq!(
            server_reason(&error_message(473, "")).as_deref(),
            Some("473")
        );
        // Console traffic and worker status are not the server's wording.
        assert_eq!(
            server_reason(&IrcEvent::Message(RoutedMessage::console("srv", "noise"))),
            None
        );
        assert_eq!(server_reason(&connection(ConnectionState::Stopped)), None);
    }

    #[test]
    fn a_refused_join_takes_the_servers_numeric_as_its_reason() {
        // Arrange
        let (mut delivery, failure) = unjoined();

        // Act
        let settled = settle_with(
            vec![error_message(473, "Cannot join channel (invite only)")],
            &mut delivery,
            failure,
        );

        // Assert
        assert_eq!(
            settled,
            Some(SendFailure::NotJoined {
                server: "srv".to_string(),
                channel: "#Test".to_string(),
                reason: Some("473: Cannot join channel (invite only)".to_string()),
            })
        );
    }

    #[test]
    fn a_channel_lost_with_its_connection_reports_the_connection_instead() {
        // Arrange: the run loop's own verdict follows the registry's state.
        let (mut delivery, failure) = unjoined();

        // Act
        let settled = settle_with(
            vec![
                IrcEvent::Error(
                    "srv".to_string(),
                    "connection or registration failed".to_string(),
                ),
                connection(ConnectionState::Stopped),
            ],
            &mut delivery,
            failure,
        );

        // Assert: the connection is what failed, and the message says so.
        assert_eq!(
            settled,
            Some(not_connected(Some("connection or registration failed")))
        );
    }

    #[test]
    fn a_new_attempt_withdraws_the_channel_failure_and_the_send_keeps_waiting() {
        // Arrange
        let (mut delivery, failure) = unjoined();

        // Act
        let settled = settle_with(
            vec![
                IrcEvent::Error(
                    "srv".to_string(),
                    "connection or registration failed".to_string(),
                ),
                connection(ConnectionState::Connecting),
            ],
            &mut delivery,
            failure,
        );

        // Assert: nothing failed yet, and the next attempt can still queue.
        assert_eq!(settled, None);
        assert_eq!(delivery.phase, Phase::Waiting);
        assert_eq!(
            delivery.on_event(&channel(ChannelState::Joining)),
            Step::Wait
        );
        assert_eq!(
            delivery.on_event(&channel(ChannelState::Joined)),
            Step::Enqueue
        );
    }

    #[test]
    fn a_join_nobody_answered_is_reported_generic_after_the_grace() {
        // Arrange: the worker's own timeout notice is console traffic, not
        // a server numeric, so nothing names a reason.
        let (mut delivery, failure) = unjoined();

        // Act
        let started = Instant::now();
        let settled = settle_with(
            vec![IrcEvent::Message(RoutedMessage::console(
                "srv",
                "JOIN timed out",
            ))],
            &mut delivery,
            failure,
        );

        // Assert: the grace is waited out for a late numeric, then the
        // generic reason stands.
        assert_eq!(
            settled,
            Some(SendFailure::NotJoined {
                server: "srv".to_string(),
                channel: "#Test".to_string(),
                reason: None,
            })
        );
        assert!(
            started.elapsed() >= REFUSAL_GRACE,
            "must leave a late numeric the grace window"
        );
    }

    #[test]
    fn a_failure_that_is_not_a_channel_loss_is_not_settled_further() {
        // Arrange
        let (mut delivery, _) = unjoined();

        // Act
        let settled = settle_with(
            vec![],
            &mut delivery,
            SendFailure::Dropped {
                server: "srv".to_string(),
            },
        );

        // Assert: returned at once, untouched.
        assert_eq!(
            settled,
            Some(SendFailure::Dropped {
                server: "srv".to_string()
            })
        );
    }

    #[test]
    fn listen_mode_cannot_wrap_a_send() {
        let conflict = listen_conflict(true).unwrap();
        assert_eq!(conflict, SendFailure::ListenConflict);
        assert_eq!(conflict.exit_code(), EXIT_LOCAL);
        assert_eq!(listen_conflict(false), None);
    }
}
