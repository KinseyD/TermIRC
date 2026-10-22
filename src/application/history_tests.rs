//! Session-level persistence tests: drain-on-open, reminted IDs, unread,
//! capacity trim, rename merge, and exclude, all through real SQLite sinks
//! on temp directories (the `logging.rs` temp-dir pattern).

use super::*;
use crate::core::{DeliveryState, Direction};
use crate::persist::HistorySink;

fn temp_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("termirc-history-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn history_config(exclude: &[&str]) -> crate::config::HistoryConfig {
    crate::config::HistoryConfig {
        persist: true,
        retention_days: 7,
        exclude: exclude.iter().map(|entry| entry.to_string()).collect(),
    }
}

fn incoming(nick: &str, text: &str) -> RoutedMessage {
    RoutedMessage {
        server: "srv".into(),
        target: BufferKind::Query(nick.into()),
        content: MessageContent::chat(nick, text),
    }
}

#[test]
fn opening_a_buffer_drains_matching_records_with_reminted_ids_and_unread_untouched() {
    // Arrange: a first session persists one query message.
    let root = temp_root("drain");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.push_message(incoming("alice", "hello")).unwrap();
    }

    // Act: a fresh session reopens the conversation.
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    session.open_server("srv");
    let id = session.open_query("srv", "alice");

    // Assert: restored, IDs reminted by this session's store (the next live
    // message continues the sequence), unread untouched.
    let (restored_id, restored_delivery) = {
        let messages = session.messages_for(id);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "hello");
        (messages[0].id, messages[0].delivery)
    };
    // The drain itself left unread untouched; only the live push after it
    // may set the flag.
    assert!(session.buffer(id).is_some_and(|buffer| !buffer.unread));
    let change = session.push_message(incoming("alice", "live")).unwrap();
    assert_eq!(change.inserted, MessageId(restored_id.0 + 1));
    assert_eq!(restored_delivery, DeliveryState::Received);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn drain_restores_outgoing_direction_as_received_and_trims_to_capacity() {
    // Arrange: three outgoing echoes archived, restored into a capacity-2
    // session.
    let root = temp_root("trim");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        for text in ["one", "two", "three"] {
            let mut message = incoming("me", text);
            message.content.direction = Direction::Outgoing;
            session.push_message(message);
        }
    }

    // Act
    let mut session =
        Session::with_persist(2, HistorySink::open(&root, &history_config(&[])).unwrap());
    let id = session.open_query("srv", "me");

    // Assert: newest kept, direction preserved, delivery Received.
    let texts: Vec<&str> = session
        .messages_for(id)
        .iter()
        .map(|message| message.text.as_str())
        .collect();
    assert_eq!(texts, vec!["two", "three"]);
    assert!(
        session
            .messages_for(id)
            .iter()
            .all(|message| message.direction == Direction::Outgoing
                && message.delivery == DeliveryState::Received)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn register_config_drains_configured_buffers_at_startup() {
    // Arrange
    let root = temp_root("register");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.open_channel("srv", "#room");
        session.push_message(RoutedMessage::chat("srv", "#room", "a", "archived line"));
    }
    let config = Config::parse(
        r##"
[servers.srv]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=["#room"]
queries=[]
"##,
    )
    .unwrap();
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    session.register_config(&config);

    // Assert
    let channel = session
        .buffers
        .iter()
        .find(|buffer| matches!(&buffer.kind, BufferKind::Channel(name) if name == "#room"))
        .unwrap();
    assert_eq!(session.messages_for(channel.id).len(), 1);
    assert!(!channel.unread);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn clean_rename_path_merges_archives_and_collision_path_writes_nothing() {
    // Arrange: alice renames to alicia (clean), then bob collides with alice.
    let root = temp_root("rename");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.open_query("srv", "alice");
        session.open_query("srv", "bob");
        session.push_message(incoming("alice", "alice's line"));
        session.push_message(RoutedMessage {
            server: "srv".into(),
            target: BufferKind::Query("bob".into()),
            content: MessageContent::chat("bob", "bob's line"),
        });
        session.rename_query("srv", "alice", "alicia");
        // Collision: an existing alicia buffer blocks bob's rename.
        session.open_query("srv", "alicia");
        session.rename_query("srv", "bob", "alicia");
    }

    // Act
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    let alicia = session.open_query("srv", "alicia");
    let bob = session.open_query("srv", "bob");

    // Assert: alice's pre-rename line joined alicia; bob stayed separate
    // because the collision path wrote no rename record.
    assert_eq!(session.messages_for(alicia).len(), 1);
    assert_eq!(session.messages_for(alicia)[0].text, "alice's line");
    assert_eq!(session.messages_for(bob).len(), 1);
    assert_eq!(session.messages_for(bob)[0].text, "bob's line");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn excluded_buffers_are_never_written_nor_restored() {
    // Arrange
    let root = temp_root("excluded");
    let config = history_config(&["#huge"]);
    {
        let mut session = Session::with_persist(5_000, HistorySink::open(&root, &config).unwrap());
        session.open_server("srv");
        session.open_channel("srv", "#huge");
        session.open_channel("srv", "#kept");
        session.push_message(RoutedMessage::chat("srv", "#huge", "a", "not archived"));
        session.push_message(RoutedMessage::chat("srv", "#kept", "a", "archived"));
    }

    // Act
    let mut session = Session::with_persist(5_000, HistorySink::open(&root, &config).unwrap());
    let huge = session.open_channel("srv", "#huge");
    let kept = session.open_channel("srv", "#kept");

    // Assert
    assert!(session.messages_for(huge).is_empty());
    assert_eq!(session.messages_for(kept).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restored_messages_are_not_written_back_to_the_archive() {
    // Arrange: drain twice from the same root; the second session must not
    // duplicate records.
    let root = temp_root("idempotent");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.push_message(incoming("alice", "once"));
    }

    // Act
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    session.open_query("srv", "alice");
    let mut third = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    let id = third.open_query("srv", "alice");

    // Assert
    assert_eq!(third.messages_for(id).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}
