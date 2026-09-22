//! Frontend-independent identities, messages, and draft editing.
//!
//! This module depends only on the standard library. Protocol adapters preserve
//! wire metadata here; terminal layout and transport state live elsewhere.

use std::fmt;
use std::ops::Deref;
use std::time::SystemTime;

/// A configuration key normalized for ASCII case-insensitive server lookup.
/// Original display names belong to the server registration, not its identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerId(String);

impl ServerId {
    pub fn new(name: &str) -> Self {
        Self(name.to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ServerId {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

impl From<String> for ServerId {
    fn from(name: String) -> Self {
        Self::new(&name)
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A stable conversation identity, independent of its name and screen position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferId(pub u64);

/// A process-local message identity; history never reuses an allocated value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferKind {
    Server,
    Channel(String),
    Query(String),
}

pub fn valid_query_nickname(nickname: &str) -> bool {
    !nickname.is_empty()
        && !nickname
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        && !nickname.contains([',', '*', '?', '!', '@', '.'])
        && !nickname.starts_with(['$', ':', '#', '&', '+', '%', '~'])
}

impl BufferKind {
    pub fn channel(&self) -> Option<&str> {
        match self {
            Self::Channel(name) => Some(name),
            Self::Server | Self::Query(_) => None,
        }
    }

    /// Match names using the application's current ASCII case mapping.
    pub fn matches(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Server, Self::Server) => true,
            (Self::Channel(left), Self::Channel(right))
            | (Self::Query(left), Self::Query(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    Chat,
    Action,
    Console,
    Error {
        code: u16,
        target: Option<String>,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Received,
    /// A local echo that has not been acknowledged by the server.
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageContent {
    pub nick: String,
    pub text: String,
    pub kind: MessageKind,
    pub direction: Direction,
    pub received_at: SystemTime,
    pub tags: Vec<(String, Option<String>)>,
    pub delivery: DeliveryState,
}

impl MessageContent {
    pub fn chat(nick: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            nick: nick.into(),
            text: text.into(),
            kind: MessageKind::Chat,
            direction: Direction::Incoming,
            received_at: SystemTime::now(),
            tags: Vec::new(),
            delivery: DeliveryState::Received,
        }
    }

    pub fn console(text: impl Into<String>) -> Self {
        Self {
            kind: MessageKind::Console,
            ..Self::chat("", text)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: MessageId,
    pub buffer: BufferId,
    pub content: MessageContent,
}

impl Deref for Message {
    type Target = MessageContent;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

/// A protocol message awaiting routing to a registered buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedMessage {
    pub server: ServerId,
    pub target: BufferKind,
    pub content: MessageContent,
}

impl RoutedMessage {
    pub fn chat(server: &str, channel: &str, nick: &str, text: &str) -> Self {
        Self {
            server: ServerId::new(server),
            target: BufferKind::Channel(channel.to_owned()),
            content: MessageContent::chat(nick, text),
        }
    }

    pub fn console(server: &str, text: impl Into<String>) -> Self {
        Self {
            server: ServerId::new(server),
            target: BufferKind::Server,
            content: MessageContent::console(text),
        }
    }
}

/// Editable input and its cursor, measured in Unicode scalar values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    text: String,
    cursor: usize,
}

impl Draft {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Keep the existing composer limit; wire byte limits are checked on send.
    pub fn insert(&mut self, character: char) {
        if self.text.chars().count() >= 512 {
            return;
        }
        self.text.insert(self.byte_offset(self.cursor), character);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.byte_offset(self.cursor));
        }
    }

    pub fn delete(&mut self) {
        let byte = self.byte_offset(self.cursor);
        if byte < self.text.len() {
            self.text.remove(byte);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// Restore complete saved input without truncation after a rejected send.
    pub fn restore(&mut self, text: String, cursor: usize) {
        self.cursor = cursor.min(text.chars().count());
        self.text = text;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn byte_offset(&self, character: usize) -> usize {
        self.text
            .char_indices()
            .nth(character)
            .map_or(self.text.len(), |(byte, _)| byte)
    }
}

/// A frontend request to the connection lifecycle, with no transport types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionCommand {
    Connect,
    Reconnect,
    Disconnect(String),
    Nick(String),
    Away(Option<String>),
    Back,
}

/// A message the user wants to send through one of our connections.
///
/// `server` is the config key used by the UI to pick the right connection.
/// `Privmsg` sends `text` to `target` (a channel); `Raw` sends `line` to the
/// server itself as a raw IRC command (e.g. `JOIN #foo`, `WHOIS nick`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutgoingMessage {
    Privmsg {
        server: String,
        target: String,
        text: String,
    },
    Raw {
        server: String,
        line: String,
    },
}
