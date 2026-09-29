//! Application coordination and session state.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::command::{
    CommandAction, CommandError, SlashCommand, SlashParseError, parse_slash_command,
};
use crate::config::Config;
use crate::connection::{ConnectionHandle, ConnectionState, IrcEvent};
use crate::core::{
    BufferId, BufferKind, ChannelState, ChannelStatus, ConnectionCommand, DeliveryState, Direction,
    Draft, Message, MessageContent, MessageId, MessageKind, OutgoingMessage, RoutedMessage,
    ServerId,
};
use crate::history::{HistoryChange, HistoryStore};
use crate::protocol::{SendError, channel_control_from_raw, encode_outgoing, validate_control};

mod channels;
mod queries;

#[cfg(test)]
mod query_tests;

#[cfg(test)]
mod channel_tests;

/// Registered conversations retain their display names separately from identity.
pub struct Buffer {
    pub id: BufferId,
    pub server: ServerId,
    pub server_label: String,
    pub kind: BufferKind,
    pub draft: Draft,
    pub hidden: bool,
    pub unread: bool,
    pub send_blocked: bool,
    pub channel_status: ChannelStatus,
    pending_channel_changes: usize,
}

#[derive(Default)]
struct ServerState {
    label: String,
    pending_changes: usize,
    connection: ConnectionState,
    nickname: Option<String>,
    away: bool,
}

/// Frontend-independent conversation state and application command coordination.
pub struct Session {
    pub(crate) buffers: Vec<Buffer>,
    pub(crate) active: Option<BufferId>,
    pub(crate) history: HistoryStore,
    servers: HashMap<ServerId, ServerState>,
    welcome_draft: Draft,
    next_buffer_id: u64,
    pending_self_echoes: HashMap<BufferId, HashSet<MessageId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubmissionEffect {
    #[default]
    None,
    Activate(BufferId),
}

impl Default for Session {
    fn default() -> Self {
        Self::new(5_000)
    }
}

/// A prepared submission owns its payload but never consumes the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputSubmission {
    Outgoing(OutgoingMessage),
    Slash(SlashCommand),
    InvalidSlash(SlashParseError),
}

impl Session {
    pub fn register_config(&mut self, config: &Config) {
        for (name, server) in &config.servers {
            self.open_server(name);
            for channel in &server.channels {
                self.open_channel(name, channel);
                if let Some(index) = self.channel_index(name, channel) {
                    self.buffers[index].channel_status.desired = true;
                }
            }
            for nickname in &server.queries {
                self.open_query(name, nickname);
            }
            self.apply_connection_event(&IrcEvent::Connection(
                name.clone(),
                ConnectionState::Connecting,
            ));
        }
    }

    pub fn new(message_capacity: usize) -> Self {
        Self {
            buffers: Vec::new(),
            active: None,
            history: HistoryStore::new(message_capacity),
            servers: HashMap::new(),
            welcome_draft: Draft::default(),
            next_buffer_id: 1,
            pending_self_echoes: HashMap::new(),
        }
    }

    /// Registration is idempotent and does not switch the active conversation.
    pub fn open_buffer(&mut self, server: &str, kind: BufferKind) -> BufferId {
        let server_id = ServerId::new(server);
        if let Some(buffer) = self
            .buffers
            .iter()
            .find(|b| b.server == server_id && b.kind.matches(&kind))
        {
            return buffer.id;
        }
        let state = self
            .servers
            .entry(server_id.clone())
            .or_insert_with(|| ServerState {
                label: server.to_owned(),
                ..ServerState::default()
            });
        let id = BufferId(self.next_buffer_id);
        self.next_buffer_id = self
            .next_buffer_id
            .checked_add(1)
            .expect("buffer identity space exhausted");
        self.buffers.push(Buffer {
            id,
            server: server_id,
            server_label: state.label.clone(),
            channel_status: ChannelStatus::default(),
            kind,
            draft: Draft::default(),
            hidden: false,
            unread: false,
            send_blocked: false,
            pending_channel_changes: 0,
        });
        id
    }

    pub fn open_channel(&mut self, server: &str, channel: &str) -> BufferId {
        self.open_buffer(server, BufferKind::Channel(channel.to_owned()))
    }

    pub fn open_server(&mut self, server: &str) -> BufferId {
        self.open_buffer(server, BufferKind::Server)
    }
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }
    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.buffers.iter().find(|b| Some(b.id) == self.active)
    }
    pub fn select_buffer(&mut self, index: usize) {
        if let Some(buffer) = self.buffers.get(index) {
            self.active = Some(buffer.id);
        }
    }

    pub fn messages(&self) -> &VecDeque<Message> {
        self.history.messages(self.active.unwrap_or(BufferId(0)))
    }
    pub fn messages_for(&self, buffer: BufferId) -> &VecDeque<Message> {
        self.history.messages(buffer)
    }

    /// Known conversations receive exactly one history entry. Errors with no
    /// registered target remain visible in the originating server's console.
    pub fn push_message(&mut self, mut message: RoutedMessage) -> Option<HistoryChange> {
        self.route_query_error(&mut message);
        if matches!(message.target, BufferKind::Query(_)) {
            if self.consume_self_echo(&message) {
                return None;
            }
            let id = self.query_message_destination(&message);
            let self_echo = message.content.direction == Direction::Outgoing
                && matches!(&message.target, BufferKind::Query(nickname) if nickname.eq_ignore_ascii_case(&message.content.nick));
            let change = self.history.append(id, message.content);
            if self_echo && !change.evicted.contains(&change.inserted) {
                self.pending_self_echoes
                    .entry(id)
                    .or_default()
                    .insert(change.inserted);
            }
            if let Some(pending) = self.pending_self_echoes.get_mut(&id) {
                for removed in &change.evicted {
                    pending.remove(removed);
                }
            }
            return Some(change);
        }
        let known = self
            .buffers
            .iter()
            .find(|b| b.server == message.server && b.kind.matches(&message.target))
            .map(|b| b.id);
        let id = match known {
            Some(id) => id,
            None if matches!(message.content.kind, MessageKind::Error { .. }) => {
                let label = self
                    .servers
                    .get(&message.server)
                    .map_or_else(|| message.server.to_string(), |state| state.label.clone());
                self.open_server(&label)
            }
            None if self.buffers.is_empty() => {
                let id = self.open_buffer(message.server.as_str(), message.target);
                self.active = Some(id);
                id
            }
            None => return None,
        };
        Some(self.history.append(id, message.content))
    }

    fn draft(&self) -> &Draft {
        self.active_buffer()
            .map_or(&self.welcome_draft, |b| &b.draft)
    }
    fn draft_mut(&mut self) -> &mut Draft {
        match self.buffers.iter_mut().find(|b| Some(b.id) == self.active) {
            Some(buffer) => &mut buffer.draft,
            None => &mut self.welcome_draft,
        }
    }
    pub fn input(&self) -> &str {
        self.draft().text()
    }
    pub fn input_cursor(&self) -> usize {
        self.draft().cursor()
    }
    pub fn type_char(&mut self, character: char) {
        self.draft_mut().insert(character);
    }
    pub fn backspace(&mut self) {
        self.draft_mut().backspace();
    }
    pub fn delete(&mut self) {
        self.draft_mut().delete();
    }
    pub fn cursor_left(&mut self) {
        self.draft_mut().left();
    }
    pub fn cursor_right(&mut self) {
        self.draft_mut().right();
    }
    pub fn cursor_home(&mut self) {
        self.draft_mut().home();
    }
    pub fn cursor_end(&mut self) {
        self.draft_mut().end();
    }
    pub fn clear_input(&mut self) {
        self.draft_mut().clear();
    }
    pub fn restore_input_at(&mut self, text: String, cursor: usize) {
        self.draft_mut().restore(text, cursor);
    }
    pub fn restore_input(&mut self, text: String) {
        let cursor = text.chars().count();
        self.restore_input_at(text, cursor);
    }

    pub fn prepare_input(&self) -> Option<InputSubmission> {
        if let Some(parsed) = parse_slash_command(self.input()) {
            return Some(match parsed {
                Ok(command) => InputSubmission::Slash(command),
                Err(error) => InputSubmission::InvalidSlash(error),
            });
        }
        let buffer = self.active_buffer()?;
        if self.input().trim().is_empty() {
            return None;
        }
        let server = buffer.server_label.clone();
        let outgoing = match &buffer.kind {
            BufferKind::Server => OutgoingMessage::Raw {
                server,
                line: self.input().trim_start().to_owned(),
            },
            BufferKind::Channel(target) | BufferKind::Query(target) => OutgoingMessage::Privmsg {
                server,
                target: target.clone(),
                text: self.input().trim().to_owned(),
            },
        };
        Some(InputSubmission::Outgoing(outgoing))
    }

    fn server_state_mut(&mut self, server: &str) -> &mut ServerState {
        self.servers
            .entry(ServerId::new(server))
            .or_insert_with(|| ServerState {
                label: server.to_owned(),
                ..ServerState::default()
            })
    }

    fn update_connection_state(&mut self, server: &str, connection: ConnectionState) {
        self.server_state_mut(server).connection = connection;
        if connection != ConnectionState::Connected {
            let server = ServerId::new(server);
            self.clear_self_echoes(&server);
            for buffer in &mut self.buffers {
                if buffer.server == server && matches!(buffer.kind, BufferKind::Channel(_)) {
                    buffer.channel_status.state = if connection == ConnectionState::Connecting
                        && buffer.channel_status.desired
                    {
                        ChannelState::Joining
                    } else {
                        ChannelState::NotJoined
                    };
                }
            }
        }
    }

    /// Immediately disable sending until all queued changes reach the worker.
    pub fn begin_connection_change(&mut self, server: &str, connection: ConnectionState) {
        self.update_connection_state(server, connection);
        self.server_state_mut(server).pending_changes += 1;
    }

    fn has_pending_connection_change(&self, server: &str) -> bool {
        self.servers
            .get(&ServerId::new(server))
            .is_some_and(|state| state.pending_changes > 0)
    }

    pub fn apply_connection_event(&mut self, event: &IrcEvent) {
        match event {
            IrcEvent::ControlApplied(server) => {
                let state = self.server_state_mut(server);
                state.pending_changes = state.pending_changes.saturating_sub(1);
            }
            IrcEvent::Connection(server, connection)
                if !self.has_pending_connection_change(server) =>
            {
                self.update_connection_state(server, *connection)
            }
            IrcEvent::ChannelControlApplied(server, channel) => {
                if let Some(index) = self.channel_index(server, channel) {
                    let buffer = &mut self.buffers[index];
                    buffer.pending_channel_changes =
                        buffer.pending_channel_changes.saturating_sub(1);
                }
            }
            IrcEvent::Channel(server, channel, status)
                if !self.has_pending_connection_change(server) =>
            {
                if let Some(index) = self.channel_index(server, channel) {
                    let buffer = &mut self.buffers[index];
                    if buffer.pending_channel_changes == 0 {
                        buffer.channel_status = *status;
                    }
                } else if crate::core::valid_channel_name(channel) {
                    let id = self.open_channel(server, channel);
                    self.buffers
                        .iter_mut()
                        .find(|buffer| buffer.id == id)
                        .unwrap()
                        .channel_status = *status;
                }
            }
            IrcEvent::Nickname(server, nickname) => {
                if self
                    .nickname(server)
                    .is_some_and(|previous| !previous.eq_ignore_ascii_case(nickname))
                {
                    self.clear_self_echoes(&ServerId::new(server));
                }
                self.server_state_mut(server).nickname = Some(nickname.clone())
            }
            IrcEvent::PeerNickname(server, previous, nickname) => {
                self.rename_query(server, previous, nickname)
            }
            IrcEvent::Away(server, away) => self.server_state_mut(server).away = *away,
            _ => {}
        }
    }

    /// Route worker events without involving terminal state or layout.
    pub fn handle_event(&mut self, event: IrcEvent) {
        match event {
            IrcEvent::Message(message) => {
                self.push_message(message);
            }
            IrcEvent::Status(..) | IrcEvent::Error(..) => {
                tracing::debug!(target: "termirc::slash", "connection attempt ended");
            }
            event => self.apply_connection_event(&event),
        }
    }

    pub fn connection_state(&self, server: &str, channel: Option<&str>) -> ConnectionState {
        match channel {
            Some(channel) => match self.channel_status(server, channel).state {
                ChannelState::Joined
                    if self.connection_state(server, None) == ConnectionState::Connected =>
                {
                    ConnectionState::Connected
                }
                ChannelState::Joining | ChannelState::Parting => ConnectionState::Connecting,
                ChannelState::Joined | ChannelState::NotJoined | ChannelState::Uncertain => {
                    ConnectionState::Stopped
                }
            },
            None => self
                .servers
                .get(&ServerId::new(server))
                .map_or(ConnectionState::Stopped, |s| s.connection),
        }
    }
    pub fn nickname(&self, server: &str) -> Option<&str> {
        self.servers
            .get(&ServerId::new(server))
            .and_then(|s| s.nickname.as_deref())
    }
    pub fn is_away(&self, server: &str) -> bool {
        self.servers
            .get(&ServerId::new(server))
            .is_some_and(|s| s.away)
    }
}

/// Validate, enqueue, then commit the draft and local echo. Any failure leaves
/// both text and cursor untouched. Slash feedback is always redacted DEBUG.
pub fn submit_composer(
    session: &mut Session,
    config: &Config,
    connections: &HashMap<String, ConnectionHandle>,
    status: &mut String,
) -> SubmissionEffect {
    let source = session.active;
    let invalid_characters = session.input().contains(['\r', '\n', '\0']);
    let Some(submission) = session.prepare_input() else {
        if session.active_buffer().is_some() && session.input().trim().is_empty() {
            if invalid_characters {
                *status = format!("failed to send ({})", SendError::InvalidCharacters);
            } else {
                session.clear_input();
            }
        }
        return SubmissionEffect::None;
    };
    let outgoing = match submission {
        InputSubmission::Slash(command) => {
            let result = if invalid_characters {
                Err("invalid_characters")
            } else {
                command
                    .action()
                    .map_err(|error| match error {
                        CommandError::Unsupported => "unsupported",
                        CommandError::InvalidArguments => "invalid_arguments",
                    })
                    .and_then(|action| execute_command(session, connections, action))
            };
            match result {
                Ok(effect) => {
                    session.clear_source_draft(source);
                    tracing::debug!(target: "termirc::slash", outcome = "queued", "command accepted");
                    return effect;
                }
                Err(reason) => {
                    tracing::debug!(target: "termirc::slash", outcome = "rejected", reason, "command rejected")
                }
            }
            return SubmissionEffect::None;
        }
        InputSubmission::InvalidSlash(SlashParseError::MissingName) => {
            tracing::debug!(target: "termirc::slash", outcome = "rejected", reason = "missing_name", "slash input parse failed");
            return SubmissionEffect::None;
        }
        InputSubmission::Outgoing(message) => message,
    };
    let validation = if invalid_characters {
        Err(SendError::InvalidCharacters)
    } else {
        encode_outgoing(&outgoing).map(|_| ())
    };
    if let Err(error) = validation {
        *status = format!("failed to send ({error})");
        return SubmissionEffect::None;
    }
    if let OutgoingMessage::Raw { line, .. } = &outgoing {
        match channel_control_from_raw(line) {
            Ok(Some(command)) => {
                let action = match command {
                    ConnectionCommand::Join(channel) => CommandAction::Join(channel),
                    ConnectionCommand::Part { channel, reason } => CommandAction::Part {
                        channel: Some(channel),
                        reason,
                    },
                    _ => return SubmissionEffect::None,
                };
                return match execute_command(session, connections, action) {
                    Ok(effect) => {
                        session.clear_source_draft(source);
                        effect
                    }
                    Err(reason) => {
                        tracing::debug!(target: "termirc::slash", outcome = "rejected", reason, "channel command rejected");
                        SubmissionEffect::None
                    }
                };
            }
            Err(error) => {
                *status = format!("failed to send ({error})");
                return SubmissionEffect::None;
            }
            Ok(None) => {}
        }
    }
    let (server, target, text) = match &outgoing {
        OutgoingMessage::Privmsg {
            server,
            target,
            text,
        } => (
            server.clone(),
            session.active_buffer().map_or_else(
                || BufferKind::Channel(target.clone()),
                |buffer| buffer.kind.clone(),
            ),
            text.clone(),
        ),
        OutgoingMessage::Raw { server, line } => (server.clone(), BufferKind::Server, line.clone()),
    };
    if session
        .active_buffer()
        .is_some_and(|buffer| buffer.send_blocked)
    {
        *status = "query target changed; use /query to select a nickname".into();
        return SubmissionEffect::None;
    }
    let ready = session.connection_state(&server, None) == ConnectionState::Connected
        && target.channel().is_none_or(|channel| {
            session.connection_state(&server, Some(channel)) == ConnectionState::Connected
        });
    let handle = connections
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&server))
        .map(|(_, handle)| handle);
    if ready && handle.is_some_and(|handle| handle.outgoing.try_send(outgoing).is_ok()) {
        let nickname = session
            .nickname(&server)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                config
                    .servers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case(&server))
                    .map_or_else(|| server.clone(), |(_, config)| config.nickname.clone())
            });
        let kind = if target == BufferKind::Server {
            MessageKind::Console
        } else {
            MessageKind::Chat
        };
        session.push_message(RoutedMessage {
            server: ServerId::new(&server),
            target,
            content: MessageContent {
                kind,
                direction: Direction::Outgoing,
                delivery: DeliveryState::Unconfirmed,
                ..MessageContent::chat(nickname, text)
            },
        });
        session.clear_input();
    } else {
        tracing::warn!("send failed on {server}: disconnected or busy");
        *status = format!("failed to send ({server} disconnected or busy)");
    }
    SubmissionEffect::None
}

fn execute_command(
    session: &mut Session,
    connections: &HashMap<String, ConnectionHandle>,
    action: CommandAction,
) -> Result<SubmissionEffect, &'static str> {
    match &action {
        CommandAction::Query(nickname) => {
            let server = session
                .active_buffer()
                .ok_or("no_server")?
                .server_label
                .clone();
            return Ok(SubmissionEffect::Activate(
                session.open_query(&server, nickname),
            ));
        }
        CommandAction::Close => {
            let current = session.active.ok_or("no_query")?;
            return session
                .close_query(current)
                .map(SubmissionEffect::Activate)
                .ok_or("not_query");
        }
        _ => {}
    }
    let explicit = match &action {
        CommandAction::Connect(server) | CommandAction::Reconnect(server) => server.as_deref(),
        _ => None,
    };
    let requested = explicit
        .or_else(|| {
            session
                .active_buffer()
                .map(|buffer| buffer.server_label.as_str())
        })
        .ok_or("no_server")?;
    let (server, handle) = connections
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(requested))
        .ok_or("unknown_server")?;
    let current = session.connection_state(server, None);
    let (command, next) = match action {
        CommandAction::Connect(_) => (ConnectionCommand::Connect, None),
        CommandAction::Reconnect(_) => (
            ConnectionCommand::Reconnect,
            Some(ConnectionState::Connecting),
        ),
        CommandAction::Disconnect(reason) => (
            ConnectionCommand::Disconnect(reason),
            Some(ConnectionState::Stopped),
        ),
        _ if current != ConnectionState::Connected => return Err("not_connected"),
        CommandAction::Nick(nick) => (ConnectionCommand::Nick(nick), None),
        CommandAction::Away(reason) => (ConnectionCommand::Away(reason), None),
        CommandAction::Back => (ConnectionCommand::Back, None),
        CommandAction::Join(channel) => return session.join_channel(server, &channel, handle),
        CommandAction::Part { channel, reason } => {
            let channel = channel
                .or_else(|| {
                    session
                        .active_buffer()
                        .and_then(|buffer| buffer.kind.channel())
                        .map(str::to_owned)
                })
                .ok_or("no_channel")?;
            return session.part_channel(server, &channel, reason, handle);
        }
        CommandAction::Query(_) | CommandAction::Close => return Err("invalid_command"),
    };
    validate_control(&command).map_err(|_| "invalid_control")?;
    handle
        .control
        .try_send(command)
        .map_err(|_| "disconnected_or_busy")?;
    if let Some(state) = next {
        session.begin_connection_change(server, state);
    }
    Ok(SubmissionEffect::None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn supported_slash_commands_route_without_echo_or_ui_feedback() {
        use super::*;
        for console in [false, true] {
            for (input, expected) in [
                ("/nick Alice", ConnectionCommand::Nick("Alice".into())),
                ("/away lunch", ConnectionCommand::Away(Some("lunch".into()))),
                ("/away", ConnectionCommand::Away(None)),
                ("/back", ConnectionCommand::Back),
                ("/connect SRV", ConnectionCommand::Connect),
                ("/reconnect", ConnectionCommand::Reconnect),
                (
                    "/disconnect bye",
                    ConnectionCommand::Disconnect("bye".into()),
                ),
                (
                    "/quit bye all",
                    ConnectionCommand::Disconnect("bye all".into()),
                ),
            ] {
                let mut app = Session::default();
                if console {
                    app.open_server("srv");
                } else {
                    app.open_channel("srv", "#a");
                }
                app.select_buffer(0);
                app.apply_connection_event(&IrcEvent::Connection(
                    "srv".into(),
                    ConnectionState::Connected,
                ));
                app.restore_input(input.into());
                let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
                let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel(8);
                let connections = std::collections::HashMap::from([(
                    "srv".into(),
                    ConnectionHandle {
                        outgoing: out_tx,
                        control: ctrl_tx,
                    },
                )]);
                let mut status = "existing status".into();
                submit_composer(
                    &mut app,
                    &Config {
                        servers: Default::default(),
                    },
                    &connections,
                    &mut status,
                );
                assert_eq!(ctrl_rx.try_recv().unwrap(), expected, "{input}");
                assert!(out_rx.try_recv().is_err());
                assert!(app.messages().is_empty());

                assert_eq!(status, "existing status");
                assert_eq!(app.input(), "");
            }
        }
    }

    #[test]
    fn rejected_commands_preserve_input_cursor_and_status() {
        use super::*;
        for input in [
            "/nick",
            "/nick Alice",
            "/connect absent",
            "/back extra",
            "/raw QUIT",
        ] {
            let mut app = Session::default();
            app.open_server("srv");
            app.select_buffer(0);
            app.restore_input(input.into());
            app.cursor_home();
            let mut status = "unchanged".into();
            submit_composer(
                &mut app,
                &Config {
                    servers: Default::default(),
                },
                &Default::default(),
                &mut status,
            );
            assert_eq!(app.input(), input);
            assert_eq!(app.input_cursor(), 0);
            assert!(app.messages().is_empty());
            assert_eq!(status, "unchanged");
        }
    }

    #[test]
    fn explicit_server_can_connect_without_active_view_and_does_not_touch_other_servers() {
        use super::*;
        let mut app = Session::default();
        let (out_a, _rx_a) = tokio::sync::mpsc::channel(8);
        let (ctrl_a, mut controls_a) = tokio::sync::mpsc::channel(8);
        let (out_b, _rx_b) = tokio::sync::mpsc::channel(8);
        let (ctrl_b, mut controls_b) = tokio::sync::mpsc::channel(8);
        let connections = std::collections::HashMap::from([
            (
                "alpha".into(),
                ConnectionHandle {
                    outgoing: out_a,
                    control: ctrl_a,
                },
            ),
            (
                "beta".into(),
                ConnectionHandle {
                    outgoing: out_b,
                    control: ctrl_b,
                },
            ),
        ]);
        app.restore_input("/connect BETA".into());
        submit_composer(
            &mut app,
            &Config {
                servers: Default::default(),
            },
            &connections,
            &mut String::new(),
        );
        assert_eq!(controls_b.try_recv().unwrap(), ConnectionCommand::Connect);
        assert!(controls_a.try_recv().is_err());
        app.apply_connection_event(&IrcEvent::Connection(
            "beta".into(),
            ConnectionState::Connecting,
        ));
        assert_eq!(
            app.connection_state("beta", None),
            ConnectionState::Connecting
        );
        assert_eq!(
            app.connection_state("alpha", None),
            ConnectionState::Stopped
        );
    }

    #[test]
    fn connecting_or_stopped_connections_do_not_queue_ordinary_input() {
        use super::*;
        for state in [ConnectionState::Connecting, ConnectionState::Stopped] {
            let mut app = Session::default();
            app.open_server("srv");
            app.select_buffer(0);
            app.apply_connection_event(&IrcEvent::Connection("srv".into(), state));
            app.restore_input("WHOIS nick".into());
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let connections = std::collections::HashMap::from([(
                "srv".into(),
                ConnectionHandle {
                    outgoing: tx,
                    control: tokio::sync::mpsc::channel(8).0,
                },
            )]);
            submit_composer(
                &mut app,
                &Config {
                    servers: Default::default(),
                },
                &connections,
                &mut String::new(),
            );
            assert!(rx.try_recv().is_err());
            assert!(app.messages().is_empty());
            assert_eq!(app.input(), "WHOIS nick");
        }
    }

    #[test]
    fn stale_events_cannot_reopen_sending_after_disconnect_or_reconnect() {
        use super::*;
        for (input, expected) in [
            ("/disconnect", ConnectionState::Stopped),
            ("/reconnect", ConnectionState::Connecting),
        ] {
            let mut app = Session::default();
            app.open_channel("srv", "#a");
            app.select_buffer(0);
            app.apply_connection_event(&IrcEvent::Connection(
                "srv".into(),
                ConnectionState::Connected,
            ));
            app.apply_connection_event(&IrcEvent::Channel(
                "srv".into(),
                "#a".into(),
                ChannelStatus {
                    state: ChannelState::Joined,
                    desired: true,
                },
            ));
            let (outgoing, mut messages) = tokio::sync::mpsc::channel(8);
            let (control, _commands) = tokio::sync::mpsc::channel(8);
            let connections = std::collections::HashMap::from([(
                "srv".into(),
                ConnectionHandle { outgoing, control },
            )]);
            let config = Config {
                servers: Default::default(),
            };
            app.restore_input(input.into());
            submit_composer(&mut app, &config, &connections, &mut String::new());
            // These were already queued before the worker saw our command.
            app.apply_connection_event(&IrcEvent::Connection(
                "srv".into(),
                ConnectionState::Connected,
            ));
            app.apply_connection_event(&IrcEvent::Channel(
                "srv".into(),
                "#a".into(),
                ChannelStatus {
                    state: ChannelState::Joined,
                    desired: true,
                },
            ));
            assert_eq!(app.connection_state("srv", None), expected);
            assert_eq!(app.connection_state("srv", Some("#a")), expected);
            app.restore_input("must not send".into());
            submit_composer(&mut app, &config, &connections, &mut String::new());
            assert!(messages.try_recv().is_err());
            assert!(app.messages().is_empty());
            // The worker barrier releases fresh events from the new session.
            app.apply_connection_event(&IrcEvent::ControlApplied("srv".into()));
            app.apply_connection_event(&IrcEvent::Connection(
                "srv".into(),
                ConnectionState::Connected,
            ));
            app.apply_connection_event(&IrcEvent::Channel(
                "srv".into(),
                "#a".into(),
                ChannelStatus {
                    state: ChannelState::Joined,
                    desired: true,
                },
            ));
            assert_eq!(
                app.connection_state("srv", None),
                ConnectionState::Connected
            );
            assert_eq!(
                app.connection_state("srv", Some("#a")),
                ConnectionState::Connected
            );
        }
    }
    use super::*;

    #[derive(Clone, Default)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogCapture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn rejected_slash_submissions_only_produce_redacted_debug_feedback() {
        for console in [false, true] {
            for input in [
                "/join #new",
                "/quit",
                "/raw JOIN #new",
                "/unknown a b",
                "/MSG alice hello  世界",
                "  /nick newname  ",
                "//hello",
                "/private-command-name sensitive-payload-260909",
                "/",
                "/   ",
                "/ join",
            ] {
                let mut app = Session::default();
                if console {
                    app.open_server("srv");
                } else {
                    app.open_channel("srv", "#a");
                }
                app.select_buffer(0);
                for c in input.chars() {
                    app.type_char(c);
                }
                let old_cursor = app.input_cursor();
                let config = Config {
                    servers: Default::default(),
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                let outgoing = std::collections::HashMap::from([(
                    "srv".to_string(),
                    ConnectionHandle {
                        outgoing: tx,
                        control: tokio::sync::mpsc::channel(8).0,
                    },
                )]);
                let mut status = "previous connection status".to_string();
                let capture = LogCapture::default();
                let writer = capture.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .with_writer(move || writer.clone())
                    .finish();

                tracing::subscriber::with_default(subscriber, || {
                    submit_composer(&mut app, &config, &outgoing, &mut status);
                });
                let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();

                assert!(
                    matches!(
                        rx.try_recv(),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    ),
                    "slash input entered outgoing queue: {input:?}, console={console}"
                );
                assert!(app.messages().is_empty());

                assert_eq!(app.buffer_count(), 1);
                assert_eq!(status, "previous connection status");
                assert_eq!(logs.lines().count(), 1);
                assert!(logs.contains("DEBUG"));
                assert!(logs.contains("termirc::slash"));
                assert!(!logs.contains(input.trim()));
                assert!(!logs.contains("private-command-name"));
                assert!(!logs.contains("sensitive-payload-260909"));
                assert_eq!(app.input(), input);
                assert_eq!(app.input_cursor(), old_cursor);
                assert!(logs.contains("rejected"));
                if matches!(input, "/" | "/   " | "/ join") {
                    assert!(logs.contains("missing_name"));
                }
            }
        }
    }

    #[test]
    fn ordinary_submissions_send_echo_and_restore_on_failure() {
        for console in [false, true] {
            for connected in [false, true] {
                let input = if console { "WHOIS nick" } else { "hello /join" };
                let mut app = Session::default();
                if console {
                    app.open_server("srv");
                } else {
                    app.open_channel("srv", "#a");
                }
                app.select_buffer(0);
                if connected {
                    app.apply_connection_event(&IrcEvent::Connection(
                        "srv".into(),
                        ConnectionState::Connected,
                    ));
                    app.apply_connection_event(&IrcEvent::Channel(
                        "srv".into(),
                        "#a".into(),
                        ChannelStatus {
                            state: ChannelState::Joined,
                            desired: true,
                        },
                    ));
                    app.apply_connection_event(&IrcEvent::Nickname(
                        "srv".into(),
                        "confirmed_nick".into(),
                    ));
                }
                for c in input.chars() {
                    app.type_char(c);
                }
                let config = Config {
                    servers: Default::default(),
                };
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                let mut outgoing = std::collections::HashMap::new();
                if connected {
                    outgoing.insert(
                        "srv".to_string(),
                        ConnectionHandle {
                            outgoing: tx,
                            control: tokio::sync::mpsc::channel(8).0,
                        },
                    );
                }
                let mut status = String::new();
                submit_composer(&mut app, &config, &outgoing, &mut status);
                if connected {
                    let expected = if console {
                        OutgoingMessage::Raw {
                            server: "srv".into(),
                            line: input.into(),
                        }
                    } else {
                        OutgoingMessage::Privmsg {
                            server: "srv".into(),
                            target: "#a".into(),
                            text: input.into(),
                        }
                    };
                    assert_eq!(rx.try_recv().unwrap(), expected);
                    assert_eq!(app.messages().len(), 1);
                    assert_eq!(app.messages()[0].text, input);
                    assert_eq!(app.messages()[0].nick, "confirmed_nick");
                    assert_eq!(app.input(), "");
                } else {
                    assert!(app.messages().is_empty());
                    assert_eq!(app.input(), input);
                    assert!(status.contains("failed to send"));
                }
            }
        }
    }

    fn connected_session(
        console: bool,
        capacity: usize,
    ) -> (
        Session,
        HashMap<String, ConnectionHandle>,
        tokio::sync::mpsc::Receiver<OutgoingMessage>,
        tokio::sync::mpsc::Receiver<ConnectionCommand>,
    ) {
        let mut session = Session::default();
        if console {
            session.open_server("SRV");
        } else {
            session.open_channel("SRV", "#a");
        }
        session.select_buffer(0);
        session.apply_connection_event(&IrcEvent::Connection(
            "srv".into(),
            ConnectionState::Connected,
        ));
        session.apply_connection_event(&IrcEvent::Channel(
            "srv".into(),
            "#a".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        let (outgoing, messages) = tokio::sync::mpsc::channel(capacity);
        let (control, commands) = tokio::sync::mpsc::channel(capacity);
        (
            session,
            HashMap::from([("srv".into(), ConnectionHandle { outgoing, control })]),
            messages,
            commands,
        )
    }

    fn empty_config() -> Config {
        Config {
            servers: Default::default(),
        }
    }

    #[test]
    fn ordinary_send_rejects_oversized_bytes_without_changing_the_draft() {
        for console in [false, true] {
            let (mut session, connections, mut messages, _commands) = connected_session(console, 1);
            let input = if console {
                format!("PRIVMSG #a :{}", "中".repeat(170))
            } else {
                format!("  {}  ", "中".repeat(170))
            };
            session.restore_input_at(input.clone(), 4);
            let mut status = String::new();
            submit_composer(&mut session, &empty_config(), &connections, &mut status);
            assert!(messages.try_recv().is_err());
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 4);
            assert!(session.messages().is_empty());
            assert!(status.contains("512"));
        }
    }

    #[test]
    fn failed_queue_preserves_leading_spaces_and_exact_cursor() {
        let (mut session, connections, _messages, _commands) = connected_session(false, 1);
        connections["srv"]
            .outgoing
            .try_send(OutgoingMessage::Raw {
                server: "srv".into(),
                line: "PING :occupied".into(),
            })
            .unwrap();
        session.restore_input_at("  original 世界  ".into(), 3);
        let mut status = String::new();
        submit_composer(&mut session, &empty_config(), &connections, &mut status);
        assert_eq!(session.input(), "  original 世界  ");
        assert_eq!(session.input_cursor(), 3);
        assert!(session.messages().is_empty());
        assert!(status.contains("failed to send"));
    }

    #[test]
    fn raw_input_preserves_suffix_spaces_and_case_insensitive_queue_lookup() {
        let (mut session, connections, mut messages, _commands) = connected_session(true, 1);
        session.restore_input("  PRIVMSG #a :hello  world  ".into());
        submit_composer(
            &mut session,
            &empty_config(),
            &connections,
            &mut String::new(),
        );
        assert_eq!(
            messages.try_recv().unwrap(),
            OutgoingMessage::Raw {
                server: "SRV".into(),
                line: "PRIVMSG #a :hello  world  ".into()
            }
        );
        assert_eq!(session.input(), "");
        assert_eq!(session.input_cursor(), 0);
        assert_eq!(session.messages()[0].text, "PRIVMSG #a :hello  world  ");
        assert_eq!(session.messages()[0].direction, Direction::Outgoing);
        assert_eq!(session.messages()[0].delivery, DeliveryState::Unconfirmed);
    }

    #[test]
    fn preparation_never_consumes_draft() {
        let mut session = Session::default();
        session.open_channel("srv", "#a");
        session.select_buffer(0);
        for input in ["chat", "/connect", "/", "   "] {
            session.restore_input_at(input.into(), 1);
            let _ = session.prepare_input();
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 1);
        }
    }

    #[test]
    fn blank_submission_clears_only_active_valid_whitespace() {
        let (mut session, connections, mut messages, _commands) = connected_session(false, 1);
        session.restore_input_at("   ".into(), 1);
        submit_composer(
            &mut session,
            &empty_config(),
            &connections,
            &mut String::new(),
        );
        assert_eq!(session.input(), "");
        assert_eq!(session.input_cursor(), 0);
        assert!(messages.try_recv().is_err());

        for invalid in ["\r", "\n", "\0"] {
            session.restore_input_at(invalid.into(), 1);
            submit_composer(
                &mut session,
                &empty_config(),
                &connections,
                &mut String::new(),
            );
            assert_eq!(session.input(), invalid);
            assert_eq!(session.input_cursor(), 1);
        }
        let mut welcome = Session::default();
        welcome.restore_input_at("   ".into(), 1);
        submit_composer(
            &mut welcome,
            &empty_config(),
            &connections,
            &mut String::new(),
        );
        assert_eq!(welcome.input(), "   ");
        assert_eq!(welcome.input_cursor(), 1);
    }

    #[test]
    fn oversized_controls_and_trailing_crlf_do_not_enter_either_queue() {
        for input in [
            format!("/away {}", "中".repeat(170)),
            format!("/disconnect {}", "x".repeat(510)),
            "/away secret\r\n".into(),
            "/quit\0".into(),
        ] {
            let (mut session, connections, mut messages, mut commands) =
                connected_session(false, 1);
            session.restore_input_at(input.clone(), 2);
            let mut status = "unchanged".into();
            submit_composer(&mut session, &empty_config(), &connections, &mut status);
            assert!(commands.try_recv().is_err(), "control must be rejected");
            assert!(messages.try_recv().is_err());
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 2);
            assert_eq!(status, "unchanged");
            assert!(session.messages().is_empty());
            assert_eq!(
                session.connection_state("srv", None),
                ConnectionState::Connected
            );
        }
    }

    #[test]
    fn invalid_ordinary_control_characters_are_not_trimmed_away() {
        for input in ["hello\n", "hello\r", "hello\0"] {
            let (mut session, connections, mut messages, _commands) = connected_session(false, 1);
            session.restore_input_at(input.into(), 2);
            let mut status = String::new();
            submit_composer(&mut session, &empty_config(), &connections, &mut status);
            assert!(messages.try_recv().is_err());
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 2);
            assert!(status.contains("failed to send"));
        }
    }

    #[test]
    fn draft_ownership_tracks_typed_buffer_identity_and_welcome_page() {
        let mut session = Session::default();
        session.restore_input_at("/connect srv".into(), 3);
        let channel = session.open_channel("srv", "#a");
        let console = session.open_server("SRV");
        let query = session.open_buffer("srv", BufferKind::Query("#a".into()));
        assert_ne!(channel, console);
        assert_ne!(channel, query);
        assert_eq!(session.open_channel("SRV", "#A"), channel);
        assert_eq!(session.input(), "/connect srv");
        session.select_buffer(0);
        assert_eq!(session.input(), "");
        session.restore_input_at("channel draft".into(), 4);
        session.select_buffer(1);
        session.restore_input_at("server draft".into(), 2);
        session.select_buffer(0);
        assert_eq!(session.input(), "channel draft");
        assert_eq!(session.input_cursor(), 4);
        session.select_buffer(1);
        assert_eq!(session.input(), "server draft");
        assert_eq!(session.input_cursor(), 2);
    }

    #[test]
    fn routed_errors_display_once_and_fall_back_to_server_console() {
        let mut session = Session::default();
        let channel = session.open_channel("srv", "#a");
        let console = session.open_server("srv");
        let other = session.open_channel("other", "#a");
        for (target, code, reason) in [("#a", 475, "bad key"), ("#missing", 404, "cannot send")] {
            session.push_message(RoutedMessage {
                server: ServerId::new("SRV"),
                target: BufferKind::Channel(target.into()),
                content: MessageContent {
                    kind: MessageKind::Error {
                        code,
                        target: Some(target.into()),
                        reason: reason.into(),
                    },
                    ..MessageContent::console(format!("{code} {target}: {reason}"))
                },
            });
        }
        assert_eq!(session.messages_for(channel).len(), 1);
        assert_eq!(session.messages_for(channel)[0].text, "475 #a: bad key");
        assert_eq!(session.messages_for(console).len(), 1);
        assert_eq!(
            session.messages_for(console)[0].text,
            "404 #missing: cannot send"
        );
        assert!(session.messages_for(other).is_empty());
        assert!(session.messages_for(channel)[0].nick.is_empty());
    }

    #[test]
    fn multiple_pending_connection_changes_require_all_worker_barriers() {
        let mut session = Session::default();
        session.open_channel("SRV", "#a");
        session.apply_connection_event(&IrcEvent::Channel(
            "srv".into(),
            "#a".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        session.begin_connection_change("srv", ConnectionState::Stopped);
        session.begin_connection_change("SRV", ConnectionState::Connecting);
        session.apply_connection_event(&IrcEvent::ControlApplied("srv".into()));
        session.apply_connection_event(&IrcEvent::Connection(
            "SRV".into(),
            ConnectionState::Connected,
        ));
        session.apply_connection_event(&IrcEvent::Channel(
            "srv".into(),
            "#A".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        assert_eq!(
            session.connection_state("srv", None),
            ConnectionState::Connecting
        );
        assert_eq!(
            session.connection_state("SRV", Some("#a")),
            ConnectionState::Connecting
        );
        session.apply_connection_event(&IrcEvent::ControlApplied("SRV".into()));
        session.apply_connection_event(&IrcEvent::Connection(
            "srv".into(),
            ConnectionState::Connected,
        ));
        session.apply_connection_event(&IrcEvent::Channel(
            "SRV".into(),
            "#a".into(),
            ChannelStatus {
                state: ChannelState::Joined,
                desired: true,
            },
        ));
        assert_eq!(
            session.connection_state("SRV", None),
            ConnectionState::Connected
        );
        assert_eq!(
            session.connection_state("srv", Some("#A")),
            ConnectionState::Connected
        );
    }

    #[test]
    fn full_control_queue_keeps_draft_and_does_not_begin_connection_change() {
        let (mut session, connections, _messages, mut commands) = connected_session(false, 1);
        connections["srv"]
            .control
            .try_send(ConnectionCommand::Back)
            .unwrap();
        session.restore_input_at(" /disconnect bye  ".into(), 5);
        let mut status = "unchanged".into();
        submit_composer(&mut session, &empty_config(), &connections, &mut status);
        assert_eq!(session.input(), " /disconnect bye  ");
        assert_eq!(session.input_cursor(), 5);
        assert_eq!(status, "unchanged");
        assert_eq!(
            session.connection_state("srv", None),
            ConnectionState::Connected
        );
        assert_eq!(commands.try_recv().unwrap(), ConnectionCommand::Back);
        assert!(commands.try_recv().is_err());
        session.apply_connection_event(&IrcEvent::Connection(
            "srv".into(),
            ConnectionState::Stopped,
        ));
        assert_eq!(
            session.connection_state("srv", None),
            ConnectionState::Stopped
        );
    }

    #[test]
    fn session_routes_worker_messages_and_limits_each_server_history_independently() {
        let mut session = Session::new(2);
        let first = session.open_channel("First", "#shared");
        let second = session.open_channel("Second", "#shared");
        session.select_buffer(0);
        session.handle_event(IrcEvent::Message(RoutedMessage::chat(
            "FIRST", "#SHARED", "alice", "one",
        )));
        let oldest = session.messages()[0].id;
        session.handle_event(IrcEvent::Message(RoutedMessage::chat(
            "second",
            "#shared",
            "bob",
            "other server",
        )));
        session.handle_event(IrcEvent::Message(RoutedMessage::chat(
            "first", "#shared", "alice", "two",
        )));
        let change = session
            .push_message(RoutedMessage::chat("first", "#shared", "alice", "three"))
            .unwrap();
        assert_eq!(change.evicted, vec![oldest]);
        assert_eq!(
            session
                .messages_for(first)
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
        assert_eq!(session.messages_for(second)[0].text, "other server");
        assert_eq!(session.active_buffer().unwrap().server_label, "First");
        assert_eq!(session.active, Some(first));
    }
}
