//! Headless bidirectional frontend: `--stdio` reads one send request per
//! stdin line, queues it with the same workers `--listen` spawns, and
//! prints one rendered event line per stdout line.
//!
//! Layer boundary: depends on `send` (the shared stdin grammar, validation,
//! and failure texts) and `listen` (the line rendering, under the [`Routed`]
//! [`listen::Scope`]), plus `core` and `connection` types. It owns no
//! `Session`, no buffers, and no history: nothing it sends or receives is
//! recorded, and the log never learns a message text.
//!
//! Readiness rules, enforced from `IrcEvent::Connection` and
//! `IrcEvent::Channel`: chat to a channel goes out only once that server is
//! `Connected` **and** that channel is `Joined`; chat to a nickname and raw
//! commands go out once the server is `Connected`. A line whose readiness
//! is not decided yet is held until a worker event decides it — the
//! connection coming up, a channel joining, or a state that rules the line
//! out (the server stopping, or its channel reported not joined once the
//! worker's joins have settled). A ruled-out line is refused on stderr,
//! never retried. EOF on stdin does not abandon held lines: the loop waits
//! (bounded — `HELD_TIMEOUT` for readiness, `ACK_TIMEOUT` for verdicts)
//! for the deciding events, then drops the handles and waits (bounded) for
//! the workers to exit; a held line refused after EOF makes the session
//! exit 3.

use std::{
    collections::HashMap,
    io::{BufRead, Write},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    time::{Duration, Instant, SystemTime},
};

use crate::{
    connection::{ConnectionHandle, ConnectionState, IrcEvent},
    core::{ChannelState, OutgoingMessage},
    i18n,
    listen::{self, Scope},
    send::{self, Parsed, SendFailure},
};

/// How long a queued message may take to report its verdict once stdin has
/// closed. It answers within one loop turn, so this only bounds a wedged
/// worker.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a held line may wait for the worker event that decides its
/// readiness once stdin has closed. Readiness is decided by events, not by
/// the verdict clock — a slow server's JOIN may still land — so this is an
/// order of magnitude above [`ACK_TIMEOUT`]; past it, the line is refused.
const HELD_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the workers may take to finish their QUIT and exit once the
/// handles are dropped.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the event loop wakes while stdin is still open.
const POLL: Duration = Duration::from_millis(50);

/// How a stdio session ended. Scripts branch on [`StdioExit::code`]: a
/// clean session exits 0, and so does one that refused lines while stdin
/// was still open — but if stdin had already closed when a held line was
/// refused for good, the caller's input did not all reach the wire and the
/// session exits 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioExit {
    /// Every held line went out (or nothing was refused after EOF).
    Clean,
    /// stdin had already closed when a held line was refused for good.
    Undelivered,
}

impl StdioExit {
    /// The process exit code the session's outcome maps to.
    pub fn code(self) -> i32 {
        match self {
            Self::Clean => 0,
            Self::Undelivered => 3,
        }
    }
}

/// One line of stdin as the reader thread reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A line with its newline stripped; bytes, because UTF-8 is validated
    /// (and refused per line) here rather than aborting the whole stream.
    Line(Vec<u8>),
    /// The stream ended: the session finishes after the verdict drain.
    Eof,
    /// Reading stdin failed; the session ends with an error.
    Failed(String),
}

/// Run a stdio session over the real stdin, stdout, and stderr.
pub fn run(
    workers: HashMap<String, ConnectionHandle>,
    rx: Receiver<IrcEvent>,
) -> anyhow::Result<StdioExit> {
    let (line_tx, lines) = mpsc::channel();
    std::thread::spawn(move || read_lines(std::io::stdin().lock(), &line_tx));
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    run_with(workers, rx, lines, &mut out, &mut err)
}

/// The drain's three time bounds, [`run_with`]'s production values: how
/// long queued messages may wait for their verdicts, how long held lines
/// may wait for the deciding worker events, and how long the workers may
/// take to exit once the handles drop.
#[derive(Debug, Clone, Copy)]
struct Bounds {
    ack: Duration,
    held: Duration,
    shutdown: Duration,
}

/// [`run`] over injected input and output streams: the seam the unit and
/// integration tests drive with an already-connected worker and buffers.
pub fn run_with(
    workers: HashMap<String, ConnectionHandle>,
    rx: Receiver<IrcEvent>,
    lines: Receiver<Input>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<StdioExit> {
    run_with_timeouts(
        workers,
        rx,
        lines,
        out,
        err,
        Bounds {
            ack: ACK_TIMEOUT,
            held: HELD_TIMEOUT,
            shutdown: SHUTDOWN_TIMEOUT,
        },
    )
}

/// [`run_with`] over explicit bounds, the seam the deadline tests drive:
/// how long queued messages may wait for their verdicts, how long held
/// lines may wait for the worker events that decide their readiness, and
/// how long the workers may take to exit once the handles drop.
fn run_with_timeouts(
    workers: HashMap<String, ConnectionHandle>,
    rx: Receiver<IrcEvent>,
    lines: Receiver<Input>,
    out: &mut dyn Write,
    err: &mut dyn Write,
    bounds: Bounds,
) -> anyhow::Result<StdioExit> {
    let mut handles = Some(workers);
    // Keyed by the config label lowercased, so lookups compare the way
    // `ServerId` does: ASCII case-insensitively.
    let mut servers: HashMap<String, ServerView> = handles
        .as_ref()
        .expect("handles outlive the view keys")
        .keys()
        .map(|label| {
            (
                label.to_ascii_lowercase(),
                ServerView {
                    // The worker has not reported yet; hold, don't refuse.
                    connection: ConnectionState::Connecting,
                    ..ServerView::default()
                },
            )
        })
        .collect();
    let mut held: Vec<Held> = Vec::new();
    let mut drain = Drain::default();
    // Set once stdin closed with queued messages still awaiting a verdict.
    let mut verdict_deadline: Option<Instant> = None;
    // Set once stdin closed with lines still held for readiness: readiness
    // is decided by worker events, not by the verdict clock, so it gets the
    // longer bound.
    let mut held_deadline: Option<Instant> = None;
    // Set once the handles are dropped: only the workers' exit remains.
    let mut shutdown_at: Option<Instant> = None;
    loop {
        if !drain.eof {
            while let Ok(input) = lines.try_recv() {
                match input {
                    Input::Line(bytes) => handle_line(
                        &mut servers,
                        &mut held,
                        handles.as_ref().expect("pre-shutdown"),
                        &bytes,
                        out,
                        err,
                    )?,
                    Input::Eof => {
                        drain.eof = true;
                        break;
                    }
                    Input::Failed(reason) => return Err(anyhow::anyhow!("{reason}")),
                }
            }
            if drain.eof {
                let now = Instant::now();
                verdict_deadline = servers
                    .values()
                    .any(|server| server.pending > 0)
                    .then(|| now + bounds.ack);
                held_deadline = (!held.is_empty()).then(|| now + bounds.held);
            }
        }
        let timeout = [verdict_deadline, held_deadline, shutdown_at]
            .into_iter()
            .flatten()
            .min()
            .map_or(POLL, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
        match rx.recv_timeout(timeout) {
            Ok(event) => {
                handle_event(
                    &mut servers,
                    &mut held,
                    handles.as_ref(),
                    &event,
                    &mut drain,
                    out,
                    err,
                )?;
                // A held line that queued during the drain takes on a
                // verdict debt after EOF: open the verdict window now, so
                // the QUIT does not overtake the message — or its reply.
                if drain.eof
                    && verdict_deadline.is_none()
                    && servers.values().any(|server| server.pending > 0)
                {
                    verdict_deadline = Some(Instant::now() + bounds.ack);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(drain.exit()),
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                if verdict_deadline.is_some_and(|deadline| now >= deadline) {
                    // A verdict that never came cannot be waited out forever:
                    // forfeit the debt and continue the lifecycle — held
                    // lines keep their own, longer bound.
                    verdict_deadline = None;
                    for server in servers.values_mut() {
                        server.pending = 0;
                    }
                }
                if held_deadline.is_some_and(|deadline| now >= deadline) {
                    // Readiness that never decided cannot be waited out
                    // forever either: the held window stays shut — lines
                    // still undecided are refused now, with the closest
                    // reason the state offers.
                    held_deadline = None;
                    let mut expired = Vec::new();
                    held.retain(|line| {
                        let decided = servers
                            .get(&line.label.to_ascii_lowercase())
                            .map(|view| view.decide(&line.label, line.channel.as_deref()))
                            .unwrap_or(Decision::Hold);
                        expired.push(match decided {
                            Decision::Refuse(failure) => failure.to_string(),
                            _ if line.channel.is_some() => SendFailure::NotJoined {
                                server: line.label.clone(),
                                channel: line.channel.clone().expect("checked above"),
                                reason: None,
                            }
                            .to_string(),
                            _ => SendFailure::NotConnected {
                                server: line.label.clone(),
                                reason: None,
                            }
                            .to_string(),
                        });
                        false
                    });
                    for reason in expired {
                        refuse(err, &reason)?;
                    }
                    drain.undelivered = true;
                }
                if let Some(deadline) = shutdown_at.filter(|deadline| now >= *deadline) {
                    let _ = deadline;
                    return Ok(drain.exit());
                }
            }
        }
        // Nothing is held any more, so the held window closes at once — a
        // prompt refusal must not wait out its bound. The verdict window,
        // by contrast, stays open until it expires: it doubles as the
        // bounded grace that lets last-second traffic (a WHOIS fired just
        // before EOF) land before the QUIT goes out.
        if held.is_empty() {
            held_deadline = None;
        }
        // Every verdict arrived (or the windows shut) and every held line
        // decided: close the handles and let the workers send their QUIT
        // and exit.
        if drain.eof
            && shutdown_at.is_none()
            && verdict_deadline.is_none()
            && held_deadline.is_none()
            && !still_owed(&servers, &held)
        {
            drop(handles.take());
            shutdown_at = Some(Instant::now() + bounds.shutdown);
        }
    }
}

/// One worker's observed state: what may be sent to it now, and how many
/// queued messages still owe a verdict.
#[derive(Debug, Default)]
struct ServerView {
    connection: ConnectionState,
    /// Channel state by lowercased name; empty until the worker says
    /// otherwise, so an unknown channel is never considered joined.
    channels: HashMap<String, ChannelState>,
    pending: usize,
}

impl ServerView {
    /// Whether the worker may still be on its way somewhere: no channel has
    /// been reported yet (the registration burst publishes its auto-joins
    /// just after `Connected`), or a join or part is in flight. Until the
    /// burst settles, an unreported channel may still be about to join.
    fn join_in_flight(&self) -> bool {
        self.channels.is_empty()
            || self
                .channels
                .values()
                .any(|state| matches!(state, ChannelState::Joining | ChannelState::Parting))
    }

    /// What a chat target (a channel name, or `None` for a nickname) or a
    /// raw command may do right now. A refusal is final: the outcome is
    /// decided and nothing will change it. `Hold` covers the undecided
    /// window — the server is still connecting, or a known channel is still
    /// joining — so a line piped in before the worker's first event is
    /// judged on the state that arrives, not on the absence of one.
    fn decide(&self, label: &str, target: Option<&str>) -> Decision {
        match self.connection {
            ConnectionState::Stopped => Decision::Refuse(SendFailure::NotConnected {
                server: label.to_owned(),
                reason: None,
            }),
            ConnectionState::Connecting => Decision::Hold,
            ConnectionState::Connected => {
                let Some(target) = target else {
                    return Decision::Send;
                };
                match self.channels.get(&target.to_ascii_lowercase()) {
                    Some(ChannelState::Joined) => Decision::Send,
                    // Still joining, or never reported yet (the JOIN is in
                    // flight but its state has not arrived): hold.
                    None | Some(ChannelState::Joining) if self.join_in_flight() => Decision::Hold,
                    _ => Decision::Refuse(SendFailure::NotJoined {
                        server: label.to_owned(),
                        channel: target.to_owned(),
                        reason: None,
                    }),
                }
            }
        }
    }
}

/// The outcome of a readiness check: send now, hold until the state
/// decides, or refuse for good.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Send,
    Hold,
    Refuse(SendFailure),
}

/// A parsed, validated line whose readiness is not decided yet.
#[derive(Debug)]
struct Held {
    label: String,
    message: OutgoingMessage,
    channel: Option<String>,
}

/// Parse, validate, and queue one stdin line — or refuse it on stderr.
fn handle_line(
    servers: &mut HashMap<String, ServerView>,
    held: &mut Vec<Held>,
    workers: &HashMap<String, ConnectionHandle>,
    bytes: &[u8],
    _out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let Ok(line) = std::str::from_utf8(bytes) else {
        return refuse(err, i18n::t("stdio.non_utf8"));
    };
    // Blank lines are noise, not errors.
    if line.trim().is_empty() {
        return Ok(());
    }
    let parsed = match send::parse_line(line) {
        Ok(parsed) => parsed,
        Err(rejection) => return refuse(err, &rejection.to_string()),
    };
    let (name, message, channel) = match parsed {
        Parsed::Chat {
            server,
            target,
            text,
        } => {
            let (message, channel) = match send::chat_outgoing(&server, &target, &text) {
                Ok(pair) => pair,
                Err(failure) => return refuse(err, &failure.to_string()),
            };
            (server, message, channel)
        }
        Parsed::Raw { server, command } => {
            let message = match send::raw_outgoing(&server, &command) {
                Ok(message) => message,
                Err(failure) => return refuse(err, &failure.to_string()),
            };
            (server, message, None)
        }
    };
    let Some((label, handle)) = lookup(workers, &name) else {
        return refuse(
            err,
            &SendFailure::UnknownServer {
                server: name.clone(),
            }
            .to_string(),
        );
    };
    let view = servers
        .get_mut(&label.to_ascii_lowercase())
        .expect("views are built from the same keys");
    match view.decide(&label, channel.as_deref()) {
        Decision::Send => {}
        Decision::Hold => {
            held.push(Held {
                label,
                message,
                channel,
            });
            return Ok(());
        }
        Decision::Refuse(failure) => return refuse(err, &failure.to_string()),
    }
    // The queue holds dozens of messages; a refusal can only mean the
    // worker is already gone.
    if handle.outgoing.try_send(message).is_err() {
        return refuse(
            err,
            &SendFailure::NotConnected {
                server: label,
                reason: None,
            }
            .to_string(),
        );
    }
    servers
        .get_mut(&label.to_ascii_lowercase())
        .expect("views are built from the same keys")
        .pending += 1;
    Ok(())
}

/// Whether any queued message still owes a verdict, or any held line still
/// awaits its readiness decision.
fn still_owed(servers: &HashMap<String, ServerView>, held: &[Held]) -> bool {
    !held.is_empty() || servers.values().any(|server| server.pending > 0)
}

/// Find a worker by config key, ASCII case-insensitively, returning the key
/// as written (the label its events carry).
fn lookup<'a>(
    workers: &'a HashMap<String, ConnectionHandle>,
    name: &str,
) -> Option<(String, &'a ConnectionHandle)> {
    workers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(key, handle)| (key.clone(), handle))
}

/// Report one refused line. Nothing is queued, nothing retried.
fn refuse(err: &mut dyn Write, reason: &str) -> anyhow::Result<()> {
    writeln!(err, "termirc stdio: {reason}")?;
    Ok(err.flush()?)
}

/// Whether stdin has closed, and whether a held line was refused for good
/// after that: together the raw material of the exit code. Refusals while
/// stdin is still open are ordinary validation feedback, not lost input.
#[derive(Debug, Default)]
struct Drain {
    eof: bool,
    undelivered: bool,
}

impl Drain {
    /// The session outcome the drain state adds up to.
    fn exit(&self) -> StdioExit {
        if self.undelivered {
            StdioExit::Undelivered
        } else {
            StdioExit::Clean
        }
    }
}

/// Fold one worker event into the views, print what belongs on stdout,
/// account the verdicts the EOF drain waits for, and re-judge any held
/// lines against the new state.
fn handle_event(
    servers: &mut HashMap<String, ServerView>,
    held: &mut Vec<Held>,
    workers: Option<&HashMap<String, ConnectionHandle>>,
    event: &IrcEvent,
    drain: &mut Drain,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if let Some(line) = listen::render_scoped_at(event, SystemTime::now(), Scope::Routed) {
        writeln!(out, "{line}")?;
        out.flush()?;
    }
    match event {
        IrcEvent::Connection(server, state) => {
            if let Some(view) = view(servers, server) {
                // A queued message never survives a change of socket, and
                // neither does channel membership; the worker re-reports
                // its channels on the way back up.
                if *state != ConnectionState::Connected {
                    view.pending = 0;
                    view.channels.clear();
                }
                view.connection = *state;
            }
        }
        IrcEvent::Channel(server, channel, status) => {
            if let Some(view) = view(servers, server) {
                view.channels
                    .insert(channel.to_ascii_lowercase(), status.state);
            }
        }
        // The verdict of a queued message: the ordinary pair for chat and
        // plain raw commands, the control acknowledgement for a raw
        // JOIN/PART. One verdict retires one queued message.
        IrcEvent::OutgoingAccepted(server)
        | IrcEvent::OutgoingDropped(server)
        | IrcEvent::ChannelControlApplied(server, _) => {
            if let Some(view) = view(servers, server)
                && view.pending > 0
            {
                view.pending -= 1;
                if matches!(event, IrcEvent::OutgoingDropped(_)) {
                    refuse(
                        err,
                        &SendFailure::Dropped {
                            server: server.clone(),
                        }
                        .to_string(),
                    )?;
                }
            }
        }
        _ => {}
    }
    if let (true, Some(workers)) = (
        matches!(event, IrcEvent::Connection(..) | IrcEvent::Channel(..)),
        workers,
    ) {
        resolve_held(servers, held, workers, drain, err)?;
    }
    Ok(())
}

/// Re-judge every held line against the just-updated state: decided ones
/// queue or are refused, the rest keep waiting.
fn resolve_held(
    servers: &mut HashMap<String, ServerView>,
    held: &mut Vec<Held>,
    workers: &HashMap<String, ConnectionHandle>,
    drain: &mut Drain,
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    let mut resolved = Vec::new();
    let mut refused = Vec::new();
    held.retain(|line| {
        let view = servers
            .get_mut(&line.label.to_ascii_lowercase())
            .expect("views are built from the same keys");
        let d = view.decide(&line.label, line.channel.as_deref());
        match d {
            Decision::Send => {
                resolved.push((line.label.clone(), line.message.clone()));
                false
            }
            Decision::Hold => true,
            Decision::Refuse(failure) => {
                refused.push(failure.to_string());
                false
            }
        }
    });
    // A refusal after stdin closed means the caller's line is known to
    // have gone nowhere — that is what the exit code reports.
    drain.undelivered |= drain.eof && !refused.is_empty();
    for failure in refused {
        refuse(err, &failure)?;
    }
    for (label, message) in resolved {
        let Some((_, handle)) = lookup(workers, &label) else {
            continue;
        };
        let ok = handle.outgoing.try_send(message).is_ok();
        if !ok {
            refuse(
                err,
                &SendFailure::NotConnected {
                    server: label,
                    reason: None,
                }
                .to_string(),
            )?;
            continue;
        }
        if let Some(view) = servers.get_mut(&label.to_ascii_lowercase()) {
            view.pending += 1;
        }
    }
    Ok(())
}

/// The view of a server event, matched the way its label is carried.
fn view<'a>(
    servers: &'a mut HashMap<String, ServerView>,
    label: &str,
) -> Option<&'a mut ServerView> {
    servers.get_mut(&label.to_ascii_lowercase())
}
/// Read stdin line by line until EOF or failure, reporting each line as an
/// [`Input`] without ever blocking the event loop on a slow consumer: the
/// channel simply fills, and the loop drains it first.
fn read_lines(mut reader: impl BufRead, tx: &mpsc::Sender<Input>) {
    loop {
        let mut bytes = Vec::new();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) => {
                let _ = tx.send(Input::Eof);
                return;
            }
            Ok(_) => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                }
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                if tx.send(Input::Line(bytes)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = tx.send(Input::Failed(error.to_string()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        BufferKind, ChannelStatus, MessageContent, MessageKind, RoutedMessage, ServerId,
    };
    use std::thread;
    use std::time::Duration;

    fn view() -> ServerView {
        ServerView {
            connection: ConnectionState::Connecting,
            channels: HashMap::new(),
            pending: 0,
        }
    }

    fn servers() -> HashMap<String, ServerView> {
        let mut servers = HashMap::new();
        servers.insert("libera".to_string(), view());
        servers
    }

    fn joined(label: &str) -> IrcEvent {
        IrcEvent::Channel(
            label.to_string(),
            "#test".to_string(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        )
    }

    /// 2026-01-01T12:34:56Z, so rendered lines are exact.
    fn modern() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_270_896)
    }

    fn message(target: BufferKind, kind: MessageKind) -> IrcEvent {
        IrcEvent::Message(RoutedMessage {
            server: ServerId::new("libera"),
            target,
            content: MessageContent {
                kind,
                received_at: modern(),
                ..MessageContent::chat("alice", "text")
            },
        })
    }

    #[test]
    fn a_channel_target_needs_connected_and_joined() {
        // Arrange
        let mut server = view();

        // Assert: while the connection is still undecided, the line holds.
        assert_eq!(server.decide("libera", Some("#test")), Decision::Hold);
        assert_eq!(server.decide("libera", None), Decision::Hold);

        // A stopped server is decided for good: nothing can arrive.
        server.connection = ConnectionState::Stopped;
        assert_eq!(
            server.decide("libera", None),
            Decision::Refuse(SendFailure::NotConnected {
                server: "libera".to_string(),
                reason: None,
            })
        );

        // Connected but not joined: a channel never reported yet holds —
        // its JOIN may still be in flight — and the nickname goes through.
        server.connection = ConnectionState::Connected;
        assert_eq!(server.decide("libera", None), Decision::Send);
        assert_eq!(server.decide("libera", Some("#test")), Decision::Hold);

        // Once the worker reports the channel, the state decides.
        server
            .channels
            .insert("#test".to_string(), ChannelState::NotJoined);
        assert_eq!(
            server.decide("libera", Some("#test")),
            Decision::Refuse(SendFailure::NotJoined {
                server: "libera".to_string(),
                channel: "#test".to_string(),
                reason: None,
            })
        );
        server
            .channels
            .insert("#test".to_string(), ChannelState::Joining);
        assert_eq!(server.decide("libera", Some("#test")), Decision::Hold);

        // Act: the JOIN is confirmed.
        let mut servers = servers();
        servers.insert("libera".to_string(), server);
        resolve_held(
            &mut servers,
            &mut vec![],
            &HashMap::new(),
            &mut Drain::default(),
            &mut Vec::new(),
        )
        .unwrap();
        let mut held = Vec::new();
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &joined("libera"),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        // Assert: the channel is sendable now, compared case-insensitively.
        assert_eq!(
            servers["libera"].decide("libera", Some("#TEST")),
            Decision::Send
        );
    }

    #[test]
    fn a_held_line_resolves_once_its_channel_joins() {
        // Arrange: the line arrived while the worker was still connecting.
        let mut servers = servers();
        let mut held = vec![Held {
            label: "libera".to_string(),
            message: OutgoingMessage::Privmsg {
                server: "libera".to_string(),
                target: "#test".to_string(),
                text: "hi".to_string(),
            },
            channel: Some("#test".to_string()),
        }];

        // Act: the connection registers, then the JOIN confirmation arrives.
        let connected = IrcEvent::Connection("libera".to_string(), ConnectionState::Connected);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &connected,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &joined("libera"),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        // Assert: the line resolved and is accounted as queued — the EOF
        // drain owes it a verdict.
        assert!(held.is_empty());
        assert!(!still_owed(&servers, &held));
        assert_eq!(servers["libera"].pending, 0);
    }

    #[test]
    fn a_state_change_revokes_readiness_and_clears_the_verdict_debt() {
        // Arrange: joined, connected, one message awaiting its verdict.
        let mut servers = servers();
        let mut held = Vec::new();
        let connected = IrcEvent::Connection("libera".to_string(), ConnectionState::Connected);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &connected,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &joined("libera"),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        servers.get_mut("libera").unwrap().pending = 1;

        // Act: the connection drops.
        let dropped = IrcEvent::Connection("libera".to_string(), ConnectionState::Connecting);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &dropped,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        // Assert: nothing is sendable and nothing is awaited any more — the
        // worker drained its queue on the way down.
        let server = &servers["libera"];
        assert_eq!(
            server.decide("libera", Some("#test")),
            Decision::Hold,
            "a connecting server has not decided yet"
        );
        assert_eq!(server.pending, 0);
    }

    #[test]
    fn one_verdict_retires_one_queued_message() {
        // Arrange
        let mut servers = servers();
        servers.get_mut("libera").unwrap().pending = 2;
        let mut held = Vec::new();

        // Act / Assert: accepted, applied — each retires exactly one queued
        // message.
        for verdict in [
            IrcEvent::OutgoingAccepted("libera".to_string()),
            IrcEvent::ChannelControlApplied("libera".to_string(), "#test".to_string()),
        ] {
            handle_event(
                &mut servers,
                &mut held,
                Some(&HashMap::new()),
                &verdict,
                &mut Drain::default(),
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .unwrap();
        }
        assert_eq!(servers["libera"].pending, 0);

        // A verdict with nothing queued is noise, never a debt.
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &IrcEvent::OutgoingAccepted("libera".to_string()),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(servers["libera"].pending, 0);
    }

    #[test]
    fn a_dropped_message_is_reported_on_stderr_and_retires_its_debt() {
        // Arrange
        let mut servers = servers();
        servers.get_mut("libera").unwrap().pending = 1;
        let mut held = Vec::new();
        let mut err = Vec::new();

        // Act
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &IrcEvent::OutgoingDropped("libera".to_string()),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();

        // Assert
        assert_eq!(servers["libera"].pending, 0);
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.starts_with("termirc stdio: libera discarded the message"),
            "stderr: {text:?}"
        );
    }

    #[test]
    fn routed_scope_prints_info_error_and_system_but_not_console() {
        // Info restates the routed text under its numeric code, addressed
        // where it routed.
        assert_eq!(
            listen::render_scoped_at(
                &message(BufferKind::Server, MessageKind::Info { code: 311 }),
                modern(),
                Scope::Routed
            ),
            Some("[12:34:56][libera] 311 text".to_string())
        );
        assert_eq!(
            listen::render_scoped_at(
                &message(
                    BufferKind::Channel("#test".to_string()),
                    MessageKind::Info { code: 324 },
                ),
                modern(),
                Scope::Routed
            ),
            Some("[12:34:56][libera/#test] 324 text".to_string())
        );
        // Error keeps listen's exact shape.
        assert_eq!(
            listen::render_scoped_at(
                &message(
                    BufferKind::Server,
                    MessageKind::Error {
                        code: 433,
                        target: None,
                        reason: "in use".to_string(),
                    }
                ),
                modern(),
                Scope::Routed
            ),
            Some("[12:34:56][libera] error 433: in use".to_string())
        );
        // System is restated under Routed and suppressed under Chat.
        assert_eq!(
            listen::render_scoped_at(
                &message(BufferKind::Server, MessageKind::System),
                modern(),
                Scope::Routed
            ),
            Some("[12:34:56][libera] text".to_string())
        );
        assert_eq!(
            listen::render_scoped_at(
                &message(BufferKind::Server, MessageKind::System),
                modern(),
                Scope::Chat
            ),
            None
        );
        // Console stays suppressed under both scopes.
        for scope in [Scope::Chat, Scope::Routed] {
            assert_eq!(
                listen::render_scoped_at(
                    &message(BufferKind::Server, MessageKind::Console),
                    modern(),
                    scope
                ),
                None,
                "scope: {scope:?}"
            );
        }
    }

    #[test]
    fn the_reader_strips_one_newline_and_reports_the_end() {
        let (tx, rx) = mpsc::channel();
        let input = b"libera/#test hi\r\nraw line\n\nlast";
        read_lines(&input[..], &tx);
        drop(tx);
        let lines: Vec<Input> = rx.iter().collect();
        assert_eq!(
            lines,
            vec![
                Input::Line(b"libera/#test hi".to_vec()),
                Input::Line(b"raw line".to_vec()),
                Input::Line(Vec::new()),
                Input::Line(b"last".to_vec()),
                Input::Eof,
            ]
        );
    }

    #[test]
    fn a_line_is_refused_as_soon_as_its_channel_state_or_server_says_no() {
        // Arrange: the line arrived while the worker was still connecting.
        let mut servers = servers();
        let mut held = vec![Held {
            label: "libera".to_string(),
            message: OutgoingMessage::Privmsg {
                server: "libera".to_string(),
                target: "#test".to_string(),
                text: "hi".to_string(),
            },
            channel: Some("#test".to_string()),
        }];
        let mut err = Vec::new();

        // Act: the server connects, then reports the channel as not joined.
        let connected = IrcEvent::Connection("libera".to_string(), ConnectionState::Connected);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &connected,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();
        assert!(
            !held.is_empty() && err.is_empty(),
            "the Connected event alone must not decide an unreported channel"
        );
        let refused = IrcEvent::Channel(
            "libera".to_string(),
            "#test".to_string(),
            ChannelStatus {
                state: ChannelState::NotJoined,
                desired: true,
            },
        );
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &refused,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();

        // Assert: refused on stderr at once, with no EOF and no timeout.
        assert!(held.is_empty(), "the line is still held");
        let text = String::from_utf8(err).unwrap();
        assert!(text.contains("could not join #test"), "stderr: {text:?}");

        // A held raw command meets the same fate the moment the server
        // stops for good.
        let mut held = vec![Held {
            label: "libera".to_string(),
            message: OutgoingMessage::Raw {
                server: "libera".to_string(),
                line: "WHOIS alice".to_string(),
            },
            channel: None,
        }];
        let mut err = Vec::new();
        let stopped = IrcEvent::Connection("libera".to_string(), ConnectionState::Stopped);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &stopped,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();
        assert!(held.is_empty(), "the raw line is still held");
        assert!(!String::from_utf8(err).unwrap().is_empty());
    }

    #[test]
    fn a_never_reported_channel_is_refused_once_the_workers_joins_settle() {
        // Arrange: a chat line for a channel the worker has never reported,
        // held from before the connection was ready.
        let mut servers = servers();
        let mut held = vec![Held {
            label: "libera".to_string(),
            message: OutgoingMessage::Privmsg {
                server: "libera".to_string(),
                target: "#nope".to_string(),
                text: "hi".to_string(),
            },
            channel: Some("#nope".to_string()),
        }];
        let mut err = Vec::new();

        // Act: the worker connects and settles its own channel — the
        // registration burst is over, and no join for `#nope` is in flight.
        let connected = IrcEvent::Connection("libera".to_string(), ConnectionState::Connected);
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &connected,
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();
        handle_event(
            &mut servers,
            &mut held,
            Some(&HashMap::new()),
            &joined("libera"),
            &mut Drain::default(),
            &mut Vec::new(),
            &mut err,
        )
        .unwrap();

        // Assert: refused on stderr now, not at EOF and not after a
        // timeout — nothing will ever name `#nope` again.
        assert!(held.is_empty(), "the line is still held: {held:?}");
        let text = String::from_utf8(err).unwrap();
        assert!(text.contains("could not join #nope"), "stderr: {text:?}");
    }

    fn worker() -> (
        ConnectionHandle,
        tokio::sync::mpsc::Receiver<OutgoingMessage>,
    ) {
        let (outgoing, out_rx) = tokio::sync::mpsc::channel(8);
        let (control, _control_rx) = tokio::sync::mpsc::channel(8);
        (ConnectionHandle { outgoing, control }, out_rx)
    }

    /// A stdio session's input, pre-written and closed.
    fn input(lines: &[&str]) -> mpsc::Receiver<Input> {
        let (tx, rx) = mpsc::channel();
        for line in lines {
            tx.send(Input::Line(line.as_bytes().to_vec())).unwrap();
        }
        tx.send(Input::Eof).unwrap();
        drop(tx);
        rx
    }

    #[test]
    fn held_lines_survive_eof_past_the_verdict_timeout() {
        // Arrange: the line and the EOF arrive while the server is still
        // connecting; the JOIN confirmation is slow — slower than the
        // queued-message verdict bound (1 s here, 5 s in production), but
        // inside the held-line bound (2 s here, 60 s in production).
        let (handle, mut out_rx) = worker();
        let mut workers = HashMap::new();
        workers.insert("libera".to_string(), handle);
        let (tx, rx) = mpsc::channel();
        let decide = thread::Builder::new()
            .spawn(move || {
                thread::sleep(Duration::from_millis(1_200));
                tx.send(IrcEvent::Connection(
                    "libera".to_string(),
                    ConnectionState::Connected,
                ))
                .unwrap();
                tx.send(joined("libera")).unwrap();
            })
            .unwrap();
        let mut out = Vec::new();
        let mut err = Vec::new();

        // Act
        run_with_timeouts(
            workers,
            rx,
            input(&["libera/#test hi"]),
            &mut out,
            &mut err,
            Bounds {
                ack: Duration::from_secs(1),
                held: Duration::from_secs(2),
                shutdown: Duration::from_secs(2),
            },
        )
        .unwrap();
        decide.join().unwrap();

        // Assert: the held line went out once the JOIN landed — the EOF
        // drain did not give up at the verdict timeout.
        let sent = out_rx.try_recv().expect("the held line was never queued");
        assert_eq!(
            sent,
            OutgoingMessage::Privmsg {
                server: "libera".to_string(),
                target: "#test".to_string(),
                text: "hi".to_string(),
            }
        );
        let err = String::from_utf8(err).unwrap();
        assert!(err.is_empty(), "the held line was refused: {err:?}");
    }

    #[test]
    fn a_held_line_past_its_bound_is_refused_and_ends_the_wait() {
        // Arrange: EOF with a held line, and no worker event ever arrives
        // (the event sender stays with the test); the held bound is short.
        let (handle, mut out_rx) = worker();
        let mut workers = HashMap::new();
        workers.insert("libera".to_string(), handle);
        let (tx, rx) = mpsc::channel();
        let mut out = Vec::new();
        let mut err = Vec::new();

        // Act
        let outcome = run_with_timeouts(
            workers,
            rx,
            input(&["libera/#nope hi"]),
            &mut out,
            &mut err,
            Bounds {
                ack: Duration::from_secs(1),
                held: Duration::from_millis(300),
                shutdown: Duration::from_millis(300),
            },
        );

        // Assert: the session gives up after the bound instead of waiting
        // forever, refuses the line on stderr, and still ends — nothing
        // was queued, so the wire stays silent. The refusal happened after
        // EOF, so the exit code says input went undelivered.
        assert_eq!(outcome.unwrap(), StdioExit::Undelivered);
        drop(tx);
        assert!(out_rx.try_recv().is_err(), "a refused line was queued");
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains("could not join #nope"), "stderr: {err:?}");
    }

    #[test]
    fn the_exit_code_branches_on_undelivered_held_lines() {
        // Assert: a clean session exits 0; stdin closed with a held line
        // refused afterwards exits 3.
        assert_eq!(StdioExit::Clean.code(), 0);
        assert_eq!(StdioExit::Undelivered.code(), 3);
    }

    #[test]
    fn an_undecodable_line_is_refused_and_the_session_continues() {
        // Arrange: one server, connected; a bad line arrives for it.
        let mut servers = servers();
        servers.get_mut("libera").unwrap().connection = ConnectionState::Connected;
        let mut held = Vec::new();
        let mut out = Vec::new();
        let mut err = Vec::new();

        // Act
        handle_line(
            &mut servers,
            &mut held,
            &HashMap::new(),
            &[0xff, b' '],
            &mut out,
            &mut err,
        )
        .unwrap();

        // Assert: the refusal names the problem, stdout stays empty.
        assert!(String::from_utf8(err).unwrap().contains("not valid UTF-8"));
        assert!(out.is_empty());
    }
}
