//! Session-level persistence tests: drain-on-open, reminted IDs, unread,
//! capacity trim, rename merge, and exclude, all through real SQLite sinks
//! on temp directories (the `logging.rs` temp-dir pattern).

use super::*;
use crate::core::{DeliveryState, Direction, MessageKind};
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
        // One archived line plus the restoration system note.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "hello");
        assert_eq!(messages[1].kind, MessageKind::System);
        (messages[0].id, messages[0].delivery)
    };
    // The drain itself left unread untouched; only the live push after it
    // may set the flag.
    assert!(session.buffer(id).is_some_and(|buffer| !buffer.unread));
    let change = session.push_message(incoming("alice", "live")).unwrap();
    // The restoration note consumed one reminted ID between replay and live.
    assert_eq!(change.inserted, MessageId(restored_id.0 + 2));
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

    // Assert: newest kept, direction preserved, delivery Received; the note
    // is appended after the trim, so it in turn evicts the older survivor.
    let messages = session.messages_for(id);
    let texts: Vec<&str> = messages
        .iter()
        .map(|message| message.text.as_str())
        .collect();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[0], "three");
    assert!(texts[1].starts_with("Restored 3 messages through "));
    assert_eq!(messages[0].direction, Direction::Outgoing);
    assert_eq!(messages[0].delivery, DeliveryState::Received);
    assert_eq!(messages[1].kind, MessageKind::System);
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
    // One archived line plus the restoration system note, unread untouched.
    assert_eq!(session.messages_for(channel.id).len(), 2);
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
    // One archived line plus the restoration note per conversation.
    assert_eq!(session.messages_for(alicia).len(), 2);
    assert_eq!(session.messages_for(alicia)[0].text, "alice's line");
    assert_eq!(session.messages_for(bob).len(), 2);
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
    // Restored line plus its note; the excluded buffer stays empty.
    assert_eq!(session.messages_for(kept).len(), 2);
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
    // One archived line plus the note; nothing was duplicated or archived.
    assert_eq!(third.messages_for(id).len(), 2);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restoring_records_ends_with_a_system_note_naming_count_and_cutoff() {
    // Arrange: two archived lines timestamped relative to now, newest
    // last. One day old keeps both records inside the 7-day retention
    // window on any run date (fixed epochs would age out eventually).
    let root = temp_root("cutoff");
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let older_at = std::time::SystemTime::now() - day;
    let newer_at = older_at + std::time::Duration::from_secs(83);
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        for (text, at) in [("older", older_at), ("newer", newer_at)] {
            let mut message = incoming("alice", text);
            message.content.received_at = at;
            session.push_message(message);
        }
    }

    // Act
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    let id = session.open_query("srv", "alice");

    // Assert: the note is the last line, counts both records, and reports
    // the newest record's timestamp at seconds precision.
    let messages = session.messages_for(id);
    let note = messages.iter().next_back().unwrap();
    assert_eq!(note.kind, MessageKind::System);
    assert_eq!(
        note.text,
        format!(
            "Restored 2 messages through {}",
            crate::persist::format_cutoff(newer_at)
        )
    );
    assert!(session.buffer(id).is_some_and(|buffer| !buffer.unread));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn restoring_an_empty_buffer_adds_no_system_note() {
    // Arrange: an archive that never saw this conversation.
    let root = temp_root("empty-note");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.push_message(incoming("alice", "hello"));
    }

    // Act
    let mut session = Session::with_persist(
        5_000,
        HistorySink::open(&root, &history_config(&[])).unwrap(),
    );
    let id = session.open_query("srv", "bob");

    // Assert
    assert!(session.messages_for(id).is_empty());
    assert!(session.buffer(id).is_some_and(|buffer| !buffer.unread));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_restoration_note_is_not_written_back_to_the_archive() {
    // Arrange: one archived line, then a restore-only session (its note and
    // replay must not reach the database).
    let root = temp_root("note-no-writeback");
    let server = crate::core::ServerId::new("srv");
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        session.open_server("srv");
        session.push_message(incoming("alice", "once"));
    }
    let before = HistorySink::open(&root, &history_config(&[]))
        .unwrap()
        .load(&server)
        .iter()
        .map(|(_, messages)| messages.len())
        .sum::<usize>();
    {
        let mut session = Session::with_persist(
            5_000,
            HistorySink::open(&root, &history_config(&[])).unwrap(),
        );
        let id = session.open_query("srv", "alice");
        assert_eq!(session.messages_for(id).len(), 2);
        assert!(session.buffer(id).is_some_and(|buffer| !buffer.unread));
    }

    // Act: reload the archive after the restoring session closed.
    let after = HistorySink::open(&root, &history_config(&[]))
        .unwrap()
        .load(&server)
        .iter()
        .map(|(_, messages)| messages.len())
        .sum::<usize>();

    // Assert: event count unchanged by the replay and its note.
    assert_eq!(before, 1);
    assert_eq!(after, before);
    let _ = std::fs::remove_dir_all(&root);
}
