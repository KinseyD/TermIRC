//! Headless read-only frontend: `--listen` prints incoming IRC traffic
//! to stdout, one line per rendered event.
//!
//! Layer boundary: depends only on `core` message types and `connection`
//! worker events (plus std and anyhow at the `run` boundary). It owns no
//! session, no buffers, no history — and above all it never sends: the
//! connection handles are held solely to keep the worker threads alive,
//! and nothing ever touches their outgoing or control channels.
//! Timestamps are UTC; chat lines use the message's own `received_at`,
//! status events are stamped when rendered.

use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc;
use std::time::SystemTime;

use crate::connection::{ConnectionHandle, ConnectionState, IrcEvent};
use crate::core::{BufferKind, MessageKind, RoutedMessage, ServerId};

/// Print every rendered event until all workers have exited.
///
/// Owning `workers` keeps the connection threads alive; `recv` returns
/// only when every sender (one per worker) is gone, so a spent retry
/// budget on all servers ends the loop cleanly.
pub fn run(
    workers: HashMap<String, ConnectionHandle>,
    rx: mpsc::Receiver<IrcEvent>,
) -> anyhow::Result<()> {
    while let Ok(event) = rx.recv() {
        if let Some(line) = render_event(&event) {
            println!("{line}");
            // Piped stdout is block-buffered; flush per line so tailing
            // stays live. `println!` already panics on write failure, so
            // there is no failure mode left to report here.
            let _ = std::io::stdout().flush();
        }
    }
    drop(workers);
    Ok(())
}

/// Render one event as a listen line, stamping status events with the
/// current time (message lines carry their own `received_at`).
pub fn render_event(event: &IrcEvent) -> Option<String> {
    render_event_at(event, SystemTime::now())
}

/// How much of the routed traffic a headless frontend prints. [`Scope::Chat`]
/// is `--listen`'s read-only view, byte for byte; [`Scope::Routed`] is stdio
/// mode's, which also restates Info, Error, and System messages as status
/// lines so an interactive peer sees the server's numerics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Chat,
    Routed,
}

/// [`render_event_at`] under a [`Scope`]: the pure core the unit tests pin
/// and both headless frontends drive.
pub(crate) fn render_scoped_at(event: &IrcEvent, now: SystemTime, scope: Scope) -> Option<String> {
    match event {
        IrcEvent::Message(message) => render_message_scoped(message, scope),
        // The state word stays English, matching the worker's own status
        // text ("connected to ..."): it is transport vocabulary.
        IrcEvent::Connection(server, state) => Some(format!(
            "[{}][{server}][{}]",
            format_time(now),
            state_word(*state)
        )),
        IrcEvent::Status(server, text) | IrcEvent::Error(server, text) => {
            Some(format!("[{}][{server}] {text}", format_time(now)))
        }
        // Session bookkeeping for the TUI (buffer state, nick tracking) and
        // the worker's verdicts on outgoing messages, which a listener never
        // queues: there are no buffers to reconcile, so none of it prints.
        IrcEvent::ControlApplied(_)
        | IrcEvent::Channel(..)
        | IrcEvent::ChannelControlApplied(..)
        | IrcEvent::Nickname(..)
        | IrcEvent::PeerNickname(..)
        | IrcEvent::Away(..)
        | IrcEvent::OutgoingAccepted(_)
        | IrcEvent::OutgoingDropped(_) => None,
    }
}

/// [`render_event_at`] is the [`Scope::Chat`] spelling.
fn render_event_at(event: &IrcEvent, now: SystemTime) -> Option<String> {
    render_scoped_at(event, now, Scope::Chat)
}

/// Render a routed message under a [`Scope`]. Chat and Action keep the one
/// chat shape everywhere; the difference is the server-facing kinds, which
/// the [`Scope::Routed`] scope restates as `[time][server(/target)] text`.
fn render_message_scoped(message: &RoutedMessage, scope: Scope) -> Option<String> {
    let content = &message.content;
    let time = format_time(content.received_at);
    match (&message.target, &content.kind) {
        // Errors are restated as bare status lines whichever buffer they
        // routed to: the buffer is a TUI concern, headless needs the
        // code and reason.
        (_, MessageKind::Error { code, reason, .. }) => Some(format!(
            "[{time}][{}] error {code}: {reason}",
            message.server
        )),
        // Info (topic, mode, whois numerics) carries its numeric code so a
        // scripted peer can parse the reply; System notices are noise for a
        // listener but the server's own voice for stdio: the routed text
        // prints as is, addressed to the buffer it routed to (the server
        // console when no known channel claimed it).
        (target, MessageKind::Info { code, .. }) if scope == Scope::Routed => Some(format!(
            "[{time}][{}] {code} {}",
            addressed_line(&message.server, target),
            content.text
        )),
        (target, MessageKind::System) if scope == Scope::Routed => Some(format!(
            "[{time}][{}] {}",
            addressed_line(&message.server, target),
            content.text
        )),
        // Console/system traffic (MOTD, numerics, local notices) and
        // anything still aimed at the server buffer is not restated:
        // connection flow arrives as its own events.
        (_, MessageKind::Console | MessageKind::Info { .. } | MessageKind::System)
        | (BufferKind::Server, _) => None,
        // Chat and actions keep the same shape in channels and queries.
        (BufferKind::Channel(target) | BufferKind::Query(target), MessageKind::Chat) => Some(
            chat_line(&time, &message.server, target, &content.nick, &content.text),
        ),
        (BufferKind::Channel(target) | BufferKind::Query(target), MessageKind::Action) => {
            Some(chat_line(
                &time,
                &message.server,
                target,
                &content.nick,
                &format!("* {}", content.text),
            ))
        }
    }
}

/// `server` for console traffic, `server/target` once a buffer claimed it.
fn addressed_line(server: &ServerId, target: &BufferKind) -> String {
    match target {
        BufferKind::Channel(name) | BufferKind::Query(name) => format!("{server}/{name}"),
        BufferKind::Server => server.to_string(),
    }
}

/// `[HH:MM:SS][server/target][nick] text` — the one chat output shape.
fn chat_line(time: &str, server: &ServerId, target: &str, nick: &str, text: &str) -> String {
    format!("[{time}][{server}/{target}][{nick}] {text}")
}

/// The short word for a connection state transition, mirroring the
/// example output shape `[server][connected]`.
fn state_word(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connecting => "connecting",
        ConnectionState::Connected => "connected",
        ConnectionState::Stopped => "disconnected",
    }
}

/// Format a timestamp as UTC `HH:MM:SS`. Only the time of day is shown,
/// so no calendar conversion is needed: seconds since the epoch reduce
/// modulo one day. Pre-epoch clocks clamp to midnight rather than panic.
fn format_time(time: SystemTime) -> String {
    let secs = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let seconds_of_day = secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds_of_day / 3_600,
        (seconds_of_day / 60) % 60,
        seconds_of_day % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::core::MessageContent;

    /// A chat message with a fixed clock, so rendered lines are exact.
    fn message_at(
        target: BufferKind,
        kind: MessageKind,
        nick: &str,
        text: &str,
        received_at: SystemTime,
    ) -> IrcEvent {
        IrcEvent::Message(RoutedMessage {
            server: ServerId::new("libera"),
            target,
            content: MessageContent {
                nick: nick.to_string(),
                text: text.to_string(),
                kind,
                direction: crate::core::Direction::Incoming,
                received_at,
                tags: Vec::new(),
                delivery: crate::core::DeliveryState::Received,
            },
        })
    }

    /// 2026-01-01T12:34:56Z — a modern, post-epoch timestamp.
    fn modern() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_270_896)
    }

    #[test]
    fn format_time_is_midnight_at_the_epoch() {
        assert_eq!(format_time(SystemTime::UNIX_EPOCH), "00:00:00");
    }

    #[test]
    fn format_time_computes_hours_minutes_seconds_from_epoch_seconds() {
        // 3661s past midnight: 01:01:01.
        assert_eq!(
            format_time(SystemTime::UNIX_EPOCH + Duration::from_secs(3_661)),
            "01:01:01"
        );
    }

    #[test]
    fn format_time_handles_a_large_modern_timestamp() {
        // 2026-01-01T12:34:56Z; the date is not shown, only the clock.
        assert_eq!(format_time(modern()), "12:34:56");
    }

    #[test]
    fn format_time_clamps_pre_epoch_clocks_to_midnight() {
        assert_eq!(
            format_time(SystemTime::UNIX_EPOCH - Duration::from_secs(1)),
            "00:00:00"
        );
    }

    #[test]
    fn channel_chat_renders_with_server_channel_and_nick_columns() {
        let event = message_at(
            BufferKind::Channel("#termirc".to_string()),
            MessageKind::Chat,
            "alice",
            "hello",
            modern(),
        );
        assert_eq!(
            render_event(&event),
            Some("[12:34:56][libera/#termirc][alice] hello".to_string())
        );
    }

    #[test]
    fn query_chat_renders_with_the_peer_as_target() {
        let event = message_at(
            BufferKind::Query("bob".to_string()),
            MessageKind::Chat,
            "bob",
            "hello",
            modern(),
        );
        assert_eq!(
            render_event(&event),
            Some("[12:34:56][libera/bob][bob] hello".to_string())
        );
    }

    #[test]
    fn actions_render_with_a_star_prefix_on_the_text() {
        let event = message_at(
            BufferKind::Channel("#termirc".to_string()),
            MessageKind::Action,
            "bob",
            "waves",
            modern(),
        );
        assert_eq!(
            render_event(&event),
            Some("[12:34:56][libera/#termirc][bob] * waves".to_string())
        );
    }

    #[test]
    fn console_lines_are_filtered_out() {
        let event = message_at(
            BufferKind::Server,
            MessageKind::Console,
            "",
            "End of /MOTD command.",
            modern(),
        );
        assert_eq!(render_event(&event), None);
    }

    #[test]
    fn connection_states_render_as_bracketed_status_words() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3_661);
        assert_eq!(
            render_event_at(
                &IrcEvent::Connection("libera".to_string(), ConnectionState::Connected),
                now
            ),
            Some("[01:01:01][libera][connected]".to_string())
        );
    }

    #[test]
    fn worker_status_and_error_events_render_as_status_lines() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3_661);
        assert_eq!(
            render_event_at(
                &IrcEvent::Status(
                    "libera".to_string(),
                    "connected to irc.libera.chat".to_string(),
                ),
                now
            ),
            Some("[01:01:01][libera] connected to irc.libera.chat".to_string())
        );
        assert_eq!(
            render_event_at(
                &IrcEvent::Error("libera".to_string(), "connection refused".to_string()),
                now
            ),
            Some("[01:01:01][libera] connection refused".to_string())
        );
    }

    #[test]
    fn routed_errors_render_as_status_lines_with_code_and_reason() {
        let event = message_at(
            BufferKind::Server,
            MessageKind::Error {
                code: 433,
                target: Some("me".to_string()),
                reason: "Nickname is already in use".to_string(),
            },
            "",
            "",
            modern(),
        );
        assert_eq!(
            render_event(&event),
            Some("[12:34:56][libera] error 433: Nickname is already in use".to_string())
        );
    }

    #[test]
    fn utf8_nicknames_and_text_survive_rendering() {
        let event = message_at(
            BufferKind::Channel("#termirc".to_string()),
            MessageKind::Chat,
            "Ünïcødé_昵称",
            "こんにちは — naïve café ☕",
            modern(),
        );
        assert_eq!(
            render_event(&event),
            Some(
                "[12:34:56][libera/#termirc][Ünïcødé_昵称] こんにちは — naïve café ☕".to_string()
            )
        );
    }

    #[test]
    fn non_message_session_events_render_nothing() {
        let now = SystemTime::UNIX_EPOCH;
        let events = [
            IrcEvent::ControlApplied("libera".to_string()),
            IrcEvent::Channel(
                "libera".to_string(),
                "#termirc".to_string(),
                crate::core::ChannelStatus::default(),
            ),
            IrcEvent::ChannelControlApplied("libera".to_string(), "#termirc".to_string()),
            IrcEvent::Nickname("libera".to_string(), "me".to_string()),
            IrcEvent::PeerNickname("libera".to_string(), "old".to_string(), "new".to_string()),
            IrcEvent::Away("libera".to_string(), true),
            IrcEvent::OutgoingAccepted("libera".to_string()),
            IrcEvent::OutgoingDropped("libera".to_string()),
        ];
        for event in &events {
            assert_eq!(render_event_at(event, now), None, "event: {event:?}");
        }
    }
}
