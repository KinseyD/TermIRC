use std::collections::{BTreeSet, HashSet};

use termirc::core::{
    BufferId, BufferKind, DeliveryState, Direction, Draft, MessageContent, MessageKind,
    RoutedMessage, ServerId,
};
use termirc::history::HistoryStore;

#[test]
fn server_identity_uses_ascii_case_insensitive_equality_hashing_and_ordering() {
    let first = ServerId::new("LiBeRa");
    let second = ServerId::from(String::from("libera"));
    assert_eq!(first.as_str(), "libera");
    assert_eq!(first.to_string(), "libera");
    assert_eq!(first, second);
    assert_eq!(HashSet::from([first.clone(), second.clone()]).len(), 1);
    assert_eq!(BTreeSet::from([first, second]).len(), 1);
    assert_ne!(ServerId::from("Ä"), ServerId::from("ä"));
}

#[test]
fn buffer_kinds_do_not_conflate_server_channel_and_private_conversations() {
    let channel = BufferKind::Channel("#RuSt".into());
    assert!(channel.matches(&BufferKind::Channel("#rust".into())));
    assert_eq!(channel.channel(), Some("#RuSt"));
    assert!(BufferKind::Query("Alice".into()).matches(&BufferKind::Query("alice".into())));
    assert!(!BufferKind::Channel("Alice".into()).matches(&BufferKind::Query("Alice".into())));
    assert!(!BufferKind::Server.matches(&BufferKind::Channel(String::new())));
    assert_eq!(BufferKind::Server.channel(), None);
    assert_eq!(BufferKind::Query("Alice".into()).channel(), None);
}

#[test]
fn routed_messages_preserve_server_and_target_identity_and_received_state() {
    let message = RoutedMessage::chat("Network", "#chat", "Alice", "hello");
    assert_eq!(message.server, ServerId::from("network"));
    assert_eq!(message.target, BufferKind::Channel("#chat".into()));
    assert_eq!(message.content.nick, "Alice");
    assert_eq!(message.content.text, "hello");
    assert_eq!(message.content.kind, MessageKind::Chat);
    assert_eq!(message.content.direction, Direction::Incoming);
    assert_eq!(message.content.delivery, DeliveryState::Received);
    let console = RoutedMessage::console("Network", "welcome");
    assert_eq!(console.target, BufferKind::Server);
    assert!(console.content.nick.is_empty());
    assert_eq!(console.content.kind, MessageKind::Console);
}

#[test]
fn history_evicts_only_from_the_target_buffer_and_never_reuses_message_ids() {
    let mut history = HistoryStore::new(2);
    let channel_a = BufferId(1);
    let channel_b = BufferId(2);
    let first = history.append(channel_a, MessageContent::chat("alice", "old"));
    let other = history.append(channel_b, MessageContent::chat("bob", "other server"));
    let second = history.append(channel_a, MessageContent::chat("alice", "middle"));
    let third = history.append(channel_a, MessageContent::chat("alice", "new"));

    assert!(first.evicted.is_empty());
    assert_eq!(third.evicted, vec![first.inserted]);
    assert!(first.inserted < other.inserted);
    assert!(other.inserted < second.inserted);
    assert!(second.inserted < third.inserted);
    assert_eq!(history.messages(channel_a).len(), 2);
    assert_eq!(history.messages(channel_a)[0].text, "middle");
    assert_eq!(history.messages(channel_a)[0].buffer, channel_a);
    assert_eq!(history.messages(channel_a)[1].id, third.inserted);
    assert_eq!(history.messages(channel_b)[0].id, other.inserted);
    assert_eq!(history.messages(channel_b)[0].text, "other server");
    assert!(history.messages(BufferId(99)).is_empty());
}

#[test]
fn zero_capacity_history_reports_the_inserted_message_as_evicted() {
    let mut history = HistoryStore::new(0);
    let buffer = BufferId(3);
    let change = history.append(buffer, MessageContent::console("test"));
    assert_eq!(change.evicted, vec![change.inserted]);
    assert!(history.messages(buffer).is_empty());
    let next = history.append(buffer, MessageContent::console("again"));
    assert!(next.inserted > change.inserted);
}

#[test]
fn default_history_retains_the_newest_five_thousand_messages() {
    let mut history = HistoryStore::default();
    let buffer = BufferId(1);
    let first = history.append(buffer, MessageContent::console("first"));
    for number in 1..5_000 {
        let change = history.append(buffer, MessageContent::console(number.to_string()));
        assert!(change.evicted.is_empty());
    }
    assert_eq!(history.messages(buffer).len(), 5_000);
    let last = history.append(buffer, MessageContent::console("last"));
    assert_eq!(last.evicted, vec![first.inserted]);
    assert_eq!(history.messages(buffer).len(), 5_000);
    assert_eq!(history.messages(buffer).front().unwrap().text, "1");
    assert_eq!(history.messages(buffer).back().unwrap().text, "last");
}

#[test]
fn history_preserves_typed_message_metadata() {
    let mut history = HistoryStore::default();
    let mut content = MessageContent::chat("alice", "waves");
    content.kind = MessageKind::Action;
    content.direction = Direction::Outgoing;
    content.delivery = DeliveryState::Unconfirmed;
    content.tags = vec![("msgid".into(), Some("123".into())), ("flag".into(), None)];
    let received_at = content.received_at;
    history.append(BufferId(1), content);
    let message = &history.messages(BufferId(1))[0];
    assert_eq!(message.kind, MessageKind::Action);
    assert_eq!(message.direction, Direction::Outgoing);
    assert_eq!(message.delivery, DeliveryState::Unconfirmed);
    assert_eq!(message.received_at, received_at);
    assert_eq!(message.tags[0], ("msgid".into(), Some("123".into())));
    assert_eq!(message.tags[1], ("flag".into(), None));
}

#[test]
fn history_preserves_structured_server_error_target_and_reason() {
    let mut history = HistoryStore::default();
    let mut content = MessageContent::console("475 #chat bad channel key");
    content.kind = MessageKind::Error {
        code: 475,
        target: Some("#chat".into()),
        reason: "bad channel key".into(),
    };
    history.append(BufferId(1), content);
    assert_eq!(
        history.messages(BufferId(1))[0].kind,
        MessageKind::Error {
            code: 475,
            target: Some("#chat".into()),
            reason: "bad channel key".into(),
        }
    );
}

#[test]
fn draft_edits_unicode_by_character_and_clamps_cursor_at_boundaries() {
    let mut draft = Draft::default();
    draft.backspace();
    draft.delete();
    draft.left();
    draft.right();
    for character in "a中文b".chars() {
        draft.insert(character);
    }
    assert_eq!(draft.cursor(), 4);
    draft.left();
    draft.backspace();
    assert_eq!(draft.text(), "a中b");
    assert_eq!(draft.cursor(), 2);
    draft.insert('文');
    draft.delete();
    assert_eq!(draft.text(), "a中文");
    draft.home();
    draft.delete();
    assert_eq!(draft.text(), "中文");
    assert_eq!(draft.cursor(), 0);
    draft.end();
    draft.right();
    assert_eq!(draft.cursor(), 2);
    draft.clear();
    assert_eq!(draft.text(), "");
    assert_eq!(draft.cursor(), 0);
}

#[test]
fn draft_limit_counts_unicode_scalars_instead_of_utf8_bytes() {
    let mut draft = Draft::default();
    for _ in 0..513 {
        draft.insert('中');
    }
    assert_eq!(draft.text().chars().count(), 512);
    assert_eq!(draft.cursor(), 512);
    draft.home();
    draft.insert('a');
    assert_eq!(draft.cursor(), 0);
    assert!(draft.text().starts_with('中'));
}

#[test]
fn draft_restore_keeps_complete_input_and_clamps_invalid_cursor() {
    let mut draft = Draft::default();
    draft.restore("中a文".into(), 1);
    assert_eq!(draft.text(), "中a文");
    assert_eq!(draft.cursor(), 1);
    let saved = draft.clone();
    draft.clear();
    assert_eq!(saved.text(), "中a文");
    assert_eq!(saved.cursor(), 1);
    draft.restore("中a文".into(), 99);
    assert_eq!(draft.cursor(), 3);
    let long_input = "中".repeat(513);
    draft.restore(long_input.clone(), 512);
    assert_eq!(draft.text(), long_input);
    assert_eq!(draft.cursor(), 512);
}
