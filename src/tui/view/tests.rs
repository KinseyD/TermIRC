use super::*;
use crate::application::InputSubmission;
use crate::command::SlashCommand;
use crate::connection::{ConnectionState, IrcEvent};
use crate::core::OutgoingMessage;
use crate::core::{BufferKind, ChannelState, ChannelStatus, MessageContent, RoutedMessage};

#[test]
fn dynamic_channel_insertion_keeps_queries_after_channels_and_tracks_sidebar_identity() {
    let mut app = App::new(40, 3);
    app.open_server("srv");
    let query = app.open_query("srv", "alice");
    app.activate_buffer(query);
    app.restore_input_at("query draft".into(), 3);
    app.focus_composer();
    app.tab();
    app.set_hover_sidebar(Some(1));
    app.session.handle_event(IrcEvent::Channel(
        "srv".into(),
        "#dynamic".into(),
        ChannelStatus {
            state: ChannelState::Joined,
            desired: true,
        },
    ));
    app.tick(std::time::Duration::ZERO);
    let rows = app.sidebar_rows();
    assert_eq!(
        rows.iter().map(|row| row.kind.clone()).collect::<Vec<_>>(),
        vec![
            BufferKind::Server,
            BufferKind::Channel("#dynamic".into()),
            BufferKind::Query("alice".into())
        ]
    );
    assert_eq!(app.sidebar_cursor(), Some(2));
    assert_eq!(app.sidebar_hovered(), Some(2));
    assert_eq!(app.active_buffer().unwrap().id, query);
    assert_eq!(app.input(), "query draft");
    assert_eq!(app.input_cursor(), 3);
    app.sidebar_up();
    app.sidebar_enter();
    assert_eq!(
        app.active_buffer().unwrap().kind,
        BufferKind::Channel("#dynamic".into())
    );
    assert_eq!(app.focus(), Focus::Composer);
}

#[test]
fn join_command_focuses_channel_and_reuses_saved_view_and_destination_draft() {
    use crate::application::submit_composer;
    use crate::connection::ConnectionHandle;
    use crate::core::ConnectionCommand;
    let mut app = App::new(40, 3);
    let console = app.open_server("srv");
    app.open_query("srv", "alice");
    app.activate_buffer(console);
    app.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Connected,
    ));
    let (outgoing, mut messages) = tokio::sync::mpsc::channel(8);
    let (control, mut commands) = tokio::sync::mpsc::channel(8);
    let connections =
        std::collections::HashMap::from([("srv".into(), ConnectionHandle { outgoing, control })]);
    let config = crate::config::Config {
        servers: Default::default(),
    };
    app.restore_input("/join #New".into());
    let effect = submit_composer(&mut app.session, &config, &connections, &mut String::new());
    app.apply_submission_effect(effect);
    let channel = app.active_buffer().unwrap().id;
    assert_eq!(
        commands.try_recv().unwrap(),
        ConnectionCommand::Join("#New".into())
    );
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.sidebar_rows()[1].id, Some(channel));
    assert_eq!(app.sidebar_cursor(), None);
    assert!(messages.try_recv().is_err());
    for index in 0..8 {
        app.push_message(RoutedMessage::chat(
            "srv",
            "#New",
            "alice",
            &format!("message {index}"),
        ));
    }
    app.click_message(5);
    app.restore_input_at("channel draft".into(), 4);
    let offset = app.scroll_offset();
    let selected = app.selected();
    app.apply_submission_effect(SubmissionEffect::Activate(console));
    app.restore_input("/join #NEW".into());
    let effect = submit_composer(&mut app.session, &config, &connections, &mut String::new());
    app.apply_submission_effect(effect);
    assert_eq!(app.active_buffer().unwrap().id, channel);
    assert_eq!(app.scroll_offset(), offset);
    assert_eq!(app.selected(), selected);
    assert_eq!(app.input(), "channel draft");
    assert_eq!(app.input_cursor(), 4);
    assert!(app.buffer(console).unwrap().draft.text().is_empty());
    assert!(commands.try_recv().is_err());
    assert_eq!(app.messages().len(), 8);
}

#[test]
fn sidebar_groups_interleaved_registrations_by_first_server() {
    let mut app = App::new(40, 10);
    app.open_channel("first", "#one");
    app.open_channel("second", "#two");
    app.open_channel("FIRST", "#three");
    let rows = app.sidebar_rows();
    assert_eq!(
        rows.iter()
            .map(|row| (row.server.as_str(), row.kind.channel()))
            .collect::<Vec<_>>(),
        vec![
            ("first", None),
            ("first", Some("#one")),
            ("first", Some("#three")),
            ("second", None),
            ("second", Some("#two")),
        ]
    );
}

#[test]
fn sidebar_cursor_tracks_identity_when_earlier_group_grows() {
    let mut app = App::new(40, 10);
    app.open_channel("first", "#one");
    app.open_channel("second", "#two");
    for _ in 0..3 {
        app.sidebar_down();
    }
    app.session.open_channel("first", "#new");
    app.sync_view();
    assert_eq!(app.sidebar_cursor(), Some(4));
    app.sidebar_enter();
    assert_eq!(active_target(&app), Some(("second", "#two")));
}

fn query_message(server: &str, nick: &str, text: &str) -> RoutedMessage {
    RoutedMessage {
        server: server.into(),
        target: BufferKind::Query(nick.into()),
        content: MessageContent::chat(nick, text),
    }
}

#[test]
fn queries_follow_channels_and_reopen_in_original_order() {
    let mut app = App::new(40, 10);
    let first = app.open_server("first");
    let alice = app.open_query("first", "Alice");
    let second = app.open_server("second");
    let other_alice = app.open_query("second", "Alice");
    let channel = app.open_channel("first", "#one");
    let bob = app.open_query("first", "Bob");
    assert_eq!(
        app.sidebar_rows()
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        [first, channel, alice, bob, second, other_alice].map(Some)
    );
    app.close_query(alice);
    assert!(!app.sidebar_rows().iter().any(|row| row.id == Some(alice)));
    app.open_query("FIRST", "ALICE");
    assert_eq!(app.sidebar_rows()[2].id, Some(alice));
    app.click_sidebar_row(5);
    assert_eq!(app.active_buffer().unwrap().id, other_alice);
}

#[test]
fn activation_effect_follows_first_visit_then_restores_query_view() {
    use crate::application::SubmissionEffect;
    let mut app = App::new(40, 3);
    let console = app.open_server("srv");
    let query = app.open_query("srv", "alice");
    app.activate_buffer(console);
    for _ in 0..8 {
        app.session
            .push_message(query_message("srv", "alice", "hello"));
    }
    app.apply_submission_effect(SubmissionEffect::Activate(query));
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.active_buffer().unwrap().id, query);
    assert_eq!(app.scroll_offset(), 14);
    app.scroll_lines(-5);
    app.click_message(5);
    app.apply_submission_effect(SubmissionEffect::Activate(console));
    app.session
        .push_message(query_message("srv", "alice", "later"));
    app.apply_submission_effect(SubmissionEffect::Activate(query));
    assert_eq!(app.scroll_offset(), 9);
    assert_eq!(app.selected(), Some(5));
    assert!(app.buffer(query).unwrap().unread);
    app.apply_submission_effect(SubmissionEffect::None);
    assert_eq!(app.scroll_offset(), 9);
}

#[test]
fn unread_clears_only_when_active_query_is_visible_at_bottom() {
    let mut app = App::new(40, 3);
    let console = app.open_server("srv");
    let query = app.open_query("srv", "alice");
    app.activate_buffer(console);
    for _ in 0..8 {
        app.session
            .push_message(query_message("srv", "alice", "hello"));
    }
    app.sync_view();
    assert!(app.buffer(query).unwrap().unread);
    app.resize(0, 3);
    app.activate_buffer(query);
    app.sync_view();
    assert!(app.buffer(query).unwrap().unread);
    app.resize(40, 0);
    app.sync_view();
    assert!(app.buffer(query).unwrap().unread);
    app.resize(40, 3);
    app.sync_view();
    assert!(!app.buffer(query).unwrap().unread);
    app.scroll_lines(-4);
    let offset = app.scroll_offset();
    app.session
        .push_message(query_message("srv", "alice", "new"));
    app.tick(std::time::Duration::ZERO);
    assert_eq!(app.scroll_offset(), offset);
    assert!(app.buffer(query).unwrap().unread);
    app.set_scroll_offset(usize::MAX);
    app.sync_view();
    assert!(!app.buffer(query).unwrap().unread);
    app.session
        .push_message(query_message("srv", "alice", "newest"));
    app.sync_view();
    assert!(app.is_at_bottom());
    assert!(!app.buffer(query).unwrap().unread);
}

#[test]
fn sidebar_viewport_reveals_navigation_and_explicit_activation() {
    let mut app = App::new(40, 3);
    app.set_sidebar_height(3);
    app.open_server("srv");
    let mut queries = Vec::new();
    for index in 0..8 {
        queries.push(app.open_query("srv", &format!("nick{index}")));
    }
    for _ in 0..8 {
        app.sidebar_down();
    }
    assert_eq!(app.sidebar_cursor(), Some(8));
    assert_eq!(app.sidebar_scroll_offset(), 6);
    app.apply_submission_effect(crate::application::SubmissionEffect::Activate(queries[0]));
    assert_eq!(app.sidebar_scroll_offset(), 1);
    app.activate_buffer(queries[7]);
    assert_eq!(app.sidebar_scroll_offset(), 6);
    app.set_sidebar_height(1);
    assert_eq!(app.sidebar_scroll_offset(), 8);
    app.set_sidebar_height(0);
    app.sync_view();
    app.set_sidebar_height(3);
    assert_eq!(app.sidebar_scroll_offset(), 6);
}

#[test]
fn query_command_effect_preserves_destination_draft_and_close_view() {
    let mut app = App::new(40, 3);
    let console = app.open_server("srv");
    let query = app.open_query("srv", "alice");
    app.activate_buffer(query);
    app.restore_input_at("saved draft".into(), 3);
    app.activate_buffer(console);
    app.restore_input("/query alice".into());
    let config = crate::config::Config {
        servers: Default::default(),
    };
    let effect = crate::application::submit_composer(
        &mut app.session,
        &config,
        &Default::default(),
        &mut String::new(),
    );
    assert_eq!(app.active_buffer().unwrap().id, console);
    assert_eq!(app.input(), "");
    app.apply_submission_effect(effect);
    assert_eq!(app.active_buffer().unwrap().id, query);
    assert_eq!(app.input(), "saved draft");
    assert_eq!(app.input_cursor(), 3);
    assert_eq!(app.focus(), Focus::Composer);
    app.restore_input("/close".into());
    let effect = crate::application::submit_composer(
        &mut app.session,
        &config,
        &Default::default(),
        &mut String::new(),
    );
    assert_eq!(app.active_buffer().unwrap().id, query);
    app.apply_submission_effect(effect);
    assert_eq!(app.active_buffer().unwrap().id, console);
    assert!(app.buffer(query).unwrap().hidden);
    assert!(!app.sidebar_rows().iter().any(|row| row.id == Some(query)));
}
fn active_target(app: &App) -> Option<(&str, &str)> {
    app.active_buffer()
        .map(|b| (b.server_label.as_str(), b.kind.channel().unwrap_or("")))
}

fn msg_to(server: &str, channel: &str, nick: &str, text: &str) -> RoutedMessage {
    RoutedMessage {
        server: server.into(),
        target: if channel.is_empty() {
            BufferKind::Server
        } else {
            BufferKind::Channel(channel.into())
        },
        content: MessageContent::chat(nick, text),
    }
}

#[test]
fn consecutive_connection_requests_wait_for_every_worker_acknowledgement() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.apply_connection_event(&IrcEvent::Channel(
        "srv".into(),
        "#a".into(),
        ChannelStatus {
            state: ChannelState::Joined,
            desired: true,
        },
    ));
    app.begin_connection_change("srv", ConnectionState::Stopped);
    app.begin_connection_change("srv", ConnectionState::Connecting);
    app.apply_connection_event(&IrcEvent::ControlApplied("srv".into()));
    app.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Stopped,
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
        ConnectionState::Connecting
    );
    assert_eq!(
        app.connection_state("srv", Some("#a")),
        ConnectionState::Connecting
    );
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

fn msg(nick: &str, text: &str) -> RoutedMessage {
    msg_to("srv", "#c", nick, text)
}

#[test]
fn new_app_is_at_bottom_with_zero_offset() {
    let app = App::new(40, 10);

    assert_eq!(app.scroll_offset(), 0);
    assert!(app.is_at_bottom());
    assert!(app.is_running());
    assert_eq!(app.total_height(), 0);
}

#[test]
fn push_message_while_at_bottom_follows_to_new_max_offset() {
    // Arrange: viewport of 3 rows; each short message is 1 content row.
    let mut app = App::new(40, 3);

    // Act & Assert: after every push the viewport stays glued to the bottom.
    for i in 0..6 {
        app.push_message(msg("u", &format!("m{i}")));
        assert!(app.is_at_bottom(), "not at bottom after push {i}");
        assert_eq!(app.scroll_offset(), app.max_offset());
    }
    // 6 messages = 6 rows + 5 separators + 2 framing rows = 13 rows;
    // max offset = 13 - 3.
    assert_eq!(app.total_height(), 13);
    assert_eq!(app.scroll_offset(), 10);
}

#[test]
fn push_message_while_scrolled_up_keeps_offset_unchanged() {
    // Arrange: overflowing content, viewport scrolled to the top.
    let mut app = App::new(40, 3);
    for i in 0..5 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    assert_eq!(app.max_offset(), 8);
    app.set_scroll_offset(0);
    assert!(!app.is_at_bottom());

    // Act
    app.push_message(msg("u", "new"));

    // Assert: newest message's last line was not visible -> view unmoved.
    assert_eq!(app.scroll_offset(), 0);
}

#[test]
fn push_message_with_last_line_exactly_visible_counts_as_at_bottom() {
    // Arrange: offset exactly at max — the last line is the bottom row.
    let mut app = App::new(40, 3);
    for i in 0..5 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    app.set_scroll_offset(app.max_offset());
    assert_eq!(app.scroll_offset(), 8);

    // Act
    app.push_message(msg("u", "new"));

    // Assert: boundary counts as "at bottom" -> follows to the new max.
    assert_eq!(app.max_offset(), 10);
    assert_eq!(app.scroll_offset(), 10);
}

#[test]
fn content_smaller_than_viewport_always_at_bottom() {
    let mut app = App::new(40, 10);

    app.push_message(msg("u", "hi"));

    assert_eq!(app.max_offset(), 0);
    assert!(app.is_at_bottom());
    assert_eq!(app.scroll_offset(), 0);
}

#[test]
fn page_up_decrements_by_one_third_viewport() {
    // Arrange: viewport height 9 -> step 3; 8 messages -> 8 rows + 7
    // separators + 2 framing rows = 17 -> max offset 8.
    let mut app = App::new(40, 9);
    for i in 0..8 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    assert_eq!(app.max_offset(), 8);

    // Act
    app.scroll_page_up();

    // Assert
    assert_eq!(app.scroll_offset(), 5);
}

#[test]
fn page_up_clamps_at_zero() {
    // Arrange
    let mut app = App::new(40, 9);
    for i in 0..8 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    app.set_scroll_offset(1);

    // Act: step is 3, which would go below zero.
    app.scroll_page_up();

    // Assert
    assert_eq!(app.scroll_offset(), 0);
}

#[test]
fn page_down_clamps_at_max_offset() {
    // Arrange
    let mut app = App::new(40, 9);
    for i in 0..8 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    app.set_scroll_offset(7);

    // Act: step 3 would exceed max offset 8.
    app.scroll_page_down();

    // Assert
    assert_eq!(app.scroll_offset(), 8);
}

#[test]
fn page_step_is_at_least_one_for_tiny_viewport() {
    // Arrange: height 2 -> 2/3 = 0, floored to a step of 1.
    let mut app = App::new(40, 2);
    for i in 0..5 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    let start = app.max_offset();
    app.set_scroll_offset(start);

    // Act
    app.scroll_page_up();

    // Assert
    assert_eq!(app.scroll_offset(), start - 1);
}

#[test]
fn resize_while_at_bottom_stays_at_bottom() {
    // Arrange: 2 messages in a tall viewport, at the bottom.
    let mut app = App::new(40, 10);
    app.push_message(msg("alice", "hi"));
    app.push_message(msg("bob", "hiya"));
    assert!(app.is_at_bottom());

    // Act: narrow the window so both bodies wrap to 2 rows
    // (alice: indent 7 -> body width 1; bob: indent 5 -> body width 3, "hiya" splits).
    app.resize(8, 2);

    // Assert: still glued to the bottom, at the recomputed max.
    assert!(app.is_at_bottom());
    assert_eq!(app.scroll_offset(), app.max_offset());
    assert_eq!(app.total_height(), 7); // 2+2 rows + 1 separator + 2 framing
}

#[test]
fn resize_while_scrolled_up_preserves_offset_clamped() {
    // Arrange: 8 short messages -> height 17; with h=10 max offset is 7.
    let mut app = App::new(30, 10);
    for _ in 0..8 {
        app.push_message(msg("u", "x"));
    }
    app.set_scroll_offset(2);
    assert!(!app.is_at_bottom());

    // Act & Assert: growing the viewport shrinks max to 5, still >= 2.
    app.resize(30, 12);
    assert_eq!(app.max_offset(), 5);
    assert_eq!(app.scroll_offset(), 2);

    // Act & Assert: growing further shrinks max to 1 -> offset clamps down.
    app.resize(30, 16);
    assert_eq!(app.max_offset(), 1);
    assert_eq!(app.scroll_offset(), 1);
}

#[test]
fn quit_sets_running_false() {
    let mut app = App::new(40, 10);

    app.quit();

    assert!(!app.is_running());
}

#[test]
fn message_cap_drops_oldest_beyond_limit() {
    // Arrange: small cap so the test stays fast.
    let mut app = App::with_caps(40, 10, 3, MAX_CACHED_LINES);

    // Act
    for i in 0..4 {
        app.push_message(msg("u", &format!("m{i}")));
    }

    // Assert: oldest ("m0") is gone, three remain.
    assert_eq!(app.messages().len(), 3);
    assert_eq!(app.messages().front().unwrap().text, "m1");
}

#[test]
fn layout_cache_budget_never_evicts_history() {
    let mut app = App::with_caps(10, 5, 1000, 8);
    for i in 0..10 {
        app.push_message(msg("u", &format!("aa bb cc {i}")));
    }
    assert_eq!(app.total_height(), 31);
    assert_eq!(app.messages().len(), 10);
    assert_eq!(app.messages().front().unwrap().text, "aa bb cc 0");
}

#[test]
fn eviction_while_scrolled_up_keeps_offset_clamped() {
    let mut app = App::with_caps(10, 3, 3, 11);
    for i in 0..3 {
        app.push_message(msg("u", &format!("aa bb cc {i}")));
    }
    app.set_scroll_offset(0);
    app.push_message(msg("u", "aa bb cc 3"));
    assert_eq!(app.messages().front().unwrap().text, "aa bb cc 1");
    assert_eq!(app.total_height(), 10);
    assert_eq!(app.scroll_offset(), 0);
}

#[test]
fn type_char_inserts_at_cursor_and_advances() {
    // Arrange
    let mut app = App::new(40, 10);

    // Act
    app.type_char('h');
    app.type_char('i');

    // Assert
    assert_eq!(app.input(), "hi");
    assert_eq!(app.input_cursor(), 2);
}

#[test]
fn type_char_inserts_in_the_middle_at_cursor() {
    // Arrange
    let mut app = App::new(40, 10);
    app.type_char('a');
    app.type_char('c');
    app.cursor_left(); // cursor between a and c

    // Act
    app.type_char('b');

    // Assert
    assert_eq!(app.input(), "abc");
    assert_eq!(app.input_cursor(), 2);
}

#[test]
fn backspace_deletes_behind_cursor() {
    // Arrange
    let mut app = App::new(40, 10);
    app.type_char('h');
    app.type_char('i');

    // Act
    app.backspace();

    // Assert
    assert_eq!(app.input(), "h");
    assert_eq!(app.input_cursor(), 1);
}

#[test]
fn backspace_at_start_is_a_noop() {
    let mut app = App::new(40, 10);
    app.backspace();
    assert_eq!(app.input(), "");
    assert_eq!(app.input_cursor(), 0);
}

#[test]
fn delete_removes_char_at_cursor() {
    // Arrange
    let mut app = App::new(40, 10);
    app.type_char('a');
    app.type_char('b');
    app.type_char('c');
    app.cursor_home(); // cursor before 'a'

    // Act
    app.delete();

    // Assert
    assert_eq!(app.input(), "bc");
    assert_eq!(app.input_cursor(), 0);
}

#[test]
fn cursor_left_right_clamp_at_bounds() {
    let mut app = App::new(40, 10);
    app.type_char('a');
    app.type_char('b');
    // at end (2): left -> 1, left -> 0, left -> 0 (clamp)
    app.cursor_left();
    assert_eq!(app.input_cursor(), 1);
    app.cursor_left();
    assert_eq!(app.input_cursor(), 0);
    app.cursor_left();
    assert_eq!(app.input_cursor(), 0);
    // right -> 1, right -> 2, right -> 2 (clamp)
    app.cursor_right();
    assert_eq!(app.input_cursor(), 1);
    app.cursor_right();
    assert_eq!(app.input_cursor(), 2);
    app.cursor_right();
    assert_eq!(app.input_cursor(), 2);
}

#[test]
fn cursor_home_and_end() {
    let mut app = App::new(40, 10);
    app.type_char('a');
    app.type_char('b');
    app.type_char('c');
    app.cursor_home();
    assert_eq!(app.input_cursor(), 0);
    app.cursor_end();
    assert_eq!(app.input_cursor(), 3);
}

#[test]
fn type_beyond_max_input_is_capped() {
    // Arrange
    let mut app = App::new(40, 10);
    for _ in 0..(512 + 10) {
        app.type_char('x');
    }

    // Assert: never exceeds the cap; cursor sits at the end.
    assert_eq!(app.input().chars().count(), 512);
    assert_eq!(app.input_cursor(), 512);
}

// ----- sending -----

#[test]
fn slash_submission_without_active_view_is_local() {
    let mut app = App::new(40, 10);
    for c in "/join #new".chars() {
        app.type_char(c);
    }
    assert_eq!(
        app.prepare_input(),
        Some(InputSubmission::Slash(SlashCommand {
            name: "join".into(),
            arguments: "#new".into(),
        }))
    );
    assert_eq!(app.buffer_count(), 0);
    assert!(app.messages().is_empty());
    assert_eq!(app.input(), "/join #new");
    assert_eq!(app.input_cursor(), 10);
    assert!(app.is_running());
}

#[test]
fn submit_input_prepares_active_channel_without_clearing_the_composer() {
    // Arrange: viewing #a with text typed in the composer.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    app.type_char('h');
    app.type_char('i');

    // Act
    let outgoing = app.prepare_input();

    // Assert: the message targets the viewed channel and the composer
    // is reset.
    assert_eq!(
        outgoing,
        Some(InputSubmission::Outgoing(OutgoingMessage::Privmsg {
            server: "srv".to_string(),
            target: "#a".to_string(),
            text: "hi".to_string(),
        }))
    );
    assert!(!app.input().is_empty());
    assert_eq!(app.input_cursor(), app.input().chars().count());
}
#[test]
fn submit_input_in_console_returns_a_raw_send() {
    // Arrange: the server console is the viewed "channel" with a raw
    // command typed into the composer.
    let mut app = App::new(40, 10);
    app.open_server("srv");
    app.select_buffer(0);
    for c in "WHOIS nick".chars() {
        app.type_char(c);
    }

    // Act
    let outgoing = app.prepare_input();

    // Assert: the line goes out raw (no target), composer reset.
    assert_eq!(
        outgoing,
        Some(InputSubmission::Outgoing(OutgoingMessage::Raw {
            server: "srv".to_string(),
            line: "WHOIS nick".to_string(),
        }))
    );
    assert!(!app.input().is_empty());
    assert_eq!(app.input_cursor(), app.input().chars().count());
}

#[test]
fn submit_input_trims_and_blanks_send_nothing() {
    // Arrange: whitespace-only input is not a message.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    app.type_char(' ');
    app.type_char(' ');

    // Act
    let outgoing = app.prepare_input();

    // Assert: nothing is sent, but the blank input is cleared.
    assert_eq!(outgoing, None);
    assert!(!app.input().is_empty());
}

#[test]
fn submit_input_without_a_channel_keeps_the_input() {
    // Arrange: the welcome page has no channel to send to.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.type_char('h');

    // Act
    let outgoing = app.prepare_input();

    // Assert: no message, and the typed text survives.
    assert_eq!(outgoing, None);
    assert_eq!(app.input(), "h");
}

#[test]
fn restore_input_puts_text_back_with_cursor_at_end() {
    // Arrange: a submitted message whose send failed.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    app.type_char('h');
    app.type_char('i');
    let outgoing = app.prepare_input().unwrap();
    assert_eq!(app.input(), "hi");
    app.clear_input();

    // Act: the UI puts the text back after a failed send.
    let InputSubmission::Outgoing(OutgoingMessage::Privmsg { text, .. }) = outgoing else {
        panic!("channel submit must produce a Privmsg");
    };
    app.restore_input(text);

    // Assert
    assert_eq!(app.input(), "hi");
    assert_eq!(app.input_cursor(), 2);
}

// ----- multi-channel routing -----

fn chan_msg(server: &str, channel: &str, text: &str) -> RoutedMessage {
    msg_to(server, channel, "u", text)
}

#[test]
fn push_routes_status_to_the_server_console_ignoring_case() {
    // Arrange: a registered console plus one channel, viewing the console
    // (status lines arrive with an empty channel and nick).
    let mut app = App::new(40, 10);
    app.open_server("srv");
    app.open_channel("srv", "#a");
    app.select_buffer(0);

    // Act
    app.push_message(msg_to("SRV", "", "", "connected to host"));

    // Assert: the status landed in the console view, not in #a.
    assert_eq!(app.messages().len(), 1);
    app.select_buffer(1);
    assert_eq!(app.messages().len(), 0);
}

#[test]
fn pushed_status_lands_in_that_servers_console_and_is_dropped_without_one() {
    // Arrange: one server has a console view registered; another does
    // not (only possible in hand-built apps — real startup always
    // registers one).
    let mut app = App::new(40, 10);
    app.open_server("s1");

    // Act
    app.push_message(msg_to("s1", "", "", "connected to a"));
    app.push_message(msg_to("s2", "", "", "connected to b"));

    // Assert: s1's console holds its status; s2's status is dropped
    // without opening a view for it.
    app.select_buffer(0);
    assert_eq!(app.messages().len(), 1);
    assert_eq!(app.buffer_count(), 1);
}

#[test]
fn open_channel_registers_and_routes() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#osu");
    app.select_buffer(0);

    // Act
    app.push_message(chan_msg("srv", "#osu", "hi"));

    // Assert
    assert_eq!(app.buffer_count(), 1);
    assert_eq!(app.messages().len(), 1);
    assert_eq!(active_target(&app), Some(("srv", "#osu")));
}

#[test]
fn open_channel_is_idempotent_ignoring_case() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#osu");
    app.open_channel("srv", "#OSU");
    assert_eq!(app.buffer_count(), 1);
}

#[test]
fn push_routes_to_channel_ignoring_case() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#osu");
    app.open_channel("srv", "#chinese");
    app.select_buffer(0);

    // Act / Assert: "#OSU" from "SRV" hits the active #osu channel.
    app.push_message(chan_msg("SRV", "#OSU", "a"));
    assert_eq!(app.messages().len(), 1);
    // #chinese is background: its history grows, the view stays #osu.
    app.push_message(chan_msg("srv", "#chinese", "b"));
    assert_eq!(app.messages().len(), 1);
    app.select_buffer(1);
    assert_eq!(app.messages().len(), 1);
}

#[test]
fn push_to_unopened_channel_is_dropped() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#osu");
    app.push_message(chan_msg("srv", "#other", "x"));
    assert_eq!(app.messages().len(), 0);
}

#[test]
fn first_view_of_a_channel_snaps_to_its_newest_history() {
    // Arrange: history accumulates in the background while the welcome
    // page is up (no channel viewed yet); 5 one-line messages lay out to
    // 11 stream rows, so a 3-row viewport has max offset 8.
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }

    // Act: open the channel from the sidebar for the first time.
    app.select_buffer(0);

    // Assert: the view lands on the newest messages, with the trailing
    // framing separator as the pane's bottom row (blank row above the
    // composer) - not on the top of the history.
    assert_eq!(app.max_offset(), 8);
    assert_eq!(app.scroll_offset(), app.max_offset());
    assert!(app.messages().back().unwrap().text == "m4");
}

#[test]
fn revisiting_a_scrolled_channel_preserves_its_position() {
    // Arrange: view #a, scroll up, switch away and back.
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(0);
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    app.scroll_page_up();
    let offset = app.scroll_offset();
    assert!(offset < app.max_offset());

    // Act
    app.select_buffer(1);
    app.select_buffer(0);

    // Assert: the scrolled position survives the round trip.
    assert_eq!(app.scroll_offset(), offset);
}

#[test]
fn switching_channel_switches_history_and_back_preserves_scroll() {
    // Arrange
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(0);
    for i in 0..6 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    app.scroll_page_up();
    let offset = app.scroll_offset();
    assert!(offset < app.max_offset());

    // Act
    app.select_buffer(1);
    assert_eq!(active_target(&app).map(|(_, c)| c), Some("#b"));
    assert_eq!(app.messages().len(), 0);
    app.push_message(chan_msg("srv", "#b", "bee"));
    app.select_buffer(0);

    // Assert: #a's history and scroll position are intact.
    assert_eq!(active_target(&app).map(|(_, c)| c), Some("#a"));
    assert_eq!(app.messages().len(), 6);
    assert_eq!(app.scroll_offset(), offset);
}

#[test]
fn background_push_leaves_active_layout_stable() {
    // Arrange
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(0);
    app.push_message(chan_msg("srv", "#a", "m"));
    let height = app.total_height();

    // Act: pushes to the background channel.
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#b", &format!("b{i}")));
    }

    // Assert
    assert_eq!(app.total_height(), height);
}

// ----- focus & sidebar -----

#[test]
fn tab_cycles_sidebar_messages_composer() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");

    // Fresh start: sidebar focused, no channel viewed.
    assert_eq!(app.focus(), Focus::Sidebar);
    assert_eq!(active_target(&app), None);
    app.tab();
    assert_eq!(app.focus(), Focus::Messages);
    app.tab();
    assert_eq!(app.focus(), Focus::Composer);
    app.tab();
    assert_eq!(app.focus(), Focus::Sidebar);
}

#[test]
fn startup_focuses_sidebar_with_cursor_on_first_row() {
    // Fresh start: no channel open -> cursor on the first row.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    assert_eq!(app.focus(), Focus::Sidebar);
    assert_eq!(app.sidebar_cursor(), Some(0));
}

#[test]
fn tab_into_sidebar_snaps_cursor_to_active_channel_row() {
    // Arrange: open both channels, view #b (row 2), focus ends on Composer.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(1); // view #b
    app.tab(); // Sidebar -> Messages
    app.tab(); // Messages -> Composer
    assert_eq!(app.focus(), Focus::Composer);

    // Act: tab back into the sidebar.
    app.tab();

    // Assert: the cursor sits on #b's row.
    assert_eq!(app.focus(), Focus::Sidebar);
    assert_eq!(app.sidebar_cursor(), Some(2));
}

#[test]
fn sidebar_jk_moves_cursor_and_clamps() {
    // Arrange: one server with two channels -> 3 visible rows.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");

    // Act / Assert (focus starts on the sidebar)
    app.sidebar_down();
    assert_eq!(app.sidebar_cursor(), Some(1));
    app.sidebar_down();
    assert_eq!(app.sidebar_cursor(), Some(2));
    app.sidebar_down();
    assert_eq!(app.sidebar_cursor(), Some(2)); // clamped
    app.sidebar_up();
    app.sidebar_up();
    app.sidebar_up();
    assert_eq!(app.sidebar_cursor(), Some(0)); // clamped
}

#[test]
fn sidebar_jk_ignored_when_sidebar_not_focused() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.tab(); // Sidebar -> Messages: the sidebar is no longer focused
    let before = app.sidebar_cursor();
    app.sidebar_down();
    assert_eq!(app.sidebar_cursor(), before);
}

#[test]
fn sidebar_enter_on_server_row_opens_the_console_and_focuses_the_composer() {
    // Arrange: rows = [srv, #a, #b]; cursor on the server row (row 0).
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");

    // Act
    app.sidebar_enter();

    // Assert: the console view (empty channel) opens with focus handed
    // to the composer, like a channel row does.
    assert_eq!(active_target(&app), Some(("srv", "")));
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.sidebar_cursor(), None);
}

#[test]
fn open_server_is_idempotent_ignoring_case() {
    // Arrange
    let mut app = App::new(40, 10);

    // Act: register the same server twice with different case.
    app.open_server("srv");
    app.open_server("SRV");

    // Assert: exactly one console view, and registration alone does not
    // view it.
    assert_eq!(app.buffer_count(), 1);
    assert_eq!(active_target(&app), None);
}

#[test]
fn tab_into_sidebar_snaps_to_the_server_row_when_console_view_is_active() {
    // Arrange: rows = [s1, #a, s2] with s2's console (row 2) viewed; the
    // composer holds focus.
    let mut app = App::new(40, 10);
    app.open_server("s1");
    app.open_channel("s1", "#a");
    app.open_server("s2");
    app.select_buffer(2);
    app.focus_composer();

    // Act: Composer -> Sidebar.
    app.tab();

    // Assert: the cursor snaps onto s2's server row (the console's row),
    // not onto row 0.
    assert_eq!(app.sidebar_cursor(), Some(2));
}

#[test]
fn sidebar_enter_on_channel_switches_view_and_focuses_composer() {
    // Arrange: rows = [srv, #a, #b]; cursor on #b.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.push_message(chan_msg("srv", "#a", "hello a"));
    app.sidebar_down();
    app.sidebar_down();

    // Act
    app.sidebar_enter();

    // Assert: view switched to #b, focus returned to the composer.
    assert_eq!(active_target(&app), Some(("srv", "#b")));
    assert_eq!(app.messages().len(), 0);
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.sidebar_cursor(), None);
}

// ----- message selection -----

#[test]
fn no_selection_when_channel_is_empty() {
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.tab();
    assert_eq!(app.focus(), Focus::Messages);
    assert_eq!(app.selected(), None);
}

#[test]
fn focusing_messages_selects_lowest_fully_visible() {
    // Arrange: 5 one-line messages in a 3-row viewport (total 11 rows,
    // max 8). Spans (framing row shifts everything down by one):
    // m0=(1) m1=(3) m2=(5) m3=(7) m4=(9); at the bottom (off=8) the only
    // fully-visible message is m4, so it is selected.
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    assert_eq!(app.scroll_offset(), app.max_offset());

    // Act
    app.tab();

    // Assert
    assert_eq!(app.focus(), Focus::Messages);
    assert_eq!(app.selected(), Some(4));
    assert_eq!(app.selected_span(), Some((9, 1)));
}

#[test]
fn select_prev_moves_up_and_reveals_with_minimal_scroll() {
    // Arrange: as above, selected m4 (span (9,1)), offset 8.
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    app.tab();
    assert_eq!(app.selected(), Some(4));

    // Act / Assert: each step scrolls up just enough to reveal the
    // separator row above the newly selected message too (offset lands on
    // the block top, one row above the span start).
    app.select_prev(); // -> m3 (span 7, block top 6)
    assert_eq!(app.selected(), Some(3));
    assert_eq!(app.scroll_offset(), 6);
    app.select_prev(); // -> m2 (span 5, block top 4)
    assert_eq!(app.selected(), Some(2));
    assert_eq!(app.scroll_offset(), 4);
    app.select_prev(); // -> m1
    assert_eq!(app.selected(), Some(1));
    assert_eq!(app.scroll_offset(), 2);
    app.select_prev(); // -> m0: the leading framing row enters view
    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.scroll_offset(), 0);
    app.select_prev(); // clamped at oldest
    assert_eq!(app.selected(), Some(0));
}

#[test]
fn select_next_moves_down_and_clamps_at_newest() {
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    for i in 0..5 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    app.tab();
    for _ in 0..4 {
        app.select_prev();
    }
    assert_eq!(app.selected(), Some(0));
    assert_eq!(app.scroll_offset(), 0);

    app.select_next(); // -> m1: block spans rows 2..=4 -> offset 2
    assert_eq!(app.selected(), Some(1));
    assert_eq!(app.scroll_offset(), 2);
    app.select_next(); // -> m2: block spans rows 4..=6 -> offset 4
    assert_eq!(app.selected(), Some(2));
    assert_eq!(app.scroll_offset(), 4);
    // jump repeatedly past the end clamps at the newest; the trailing
    // framing row stays visible below it (offset == max).
    for _ in 0..10 {
        app.select_next();
    }
    assert_eq!(app.selected(), Some(4));
    assert_eq!(app.scroll_offset(), app.max_offset());
}

#[test]
fn follow_is_paused_while_a_message_is_selected() {
    // Arrange
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.select_buffer(0);
    for i in 0..4 {
        app.push_message(chan_msg("srv", "#a", &format!("m{i}")));
    }
    app.tab();
    let selected = app.selected().unwrap();
    let offset = app.scroll_offset();

    // Act: a new message arrives while a message is selected.
    app.push_message(chan_msg("srv", "#a", "m4"));

    // Assert: neither the viewport nor the selection was disturbed.
    assert_eq!(app.scroll_offset(), offset);
    assert_eq!(app.selected(), Some(selected));
}

#[test]
fn selection_ignored_when_messages_not_focused() {
    // Arrange: selection changes are no-ops unless the message pane is focused.
    let mut app = App::new(40, 3);
    app.open_channel("srv", "#a");
    app.push_message(chan_msg("srv", "#a", "m0"));
    assert_eq!(app.focus(), Focus::Sidebar);

    // Act
    app.select_prev();
    app.select_next();

    // Assert
    assert_eq!(app.selected(), None);
}

// ----- mouse: scrolling -----

#[test]
fn scroll_linesmoves_by_exact_amount_and_clamps() {
    // Arrange: 5 one-line messages -> 11 rows; viewport 3 -> max 8.
    let mut app = App::new(40, 3);
    for i in 0..5 {
        app.push_message(msg("u", &format!("m{i}")));
    }
    app.set_scroll_offset(5);

    // Act / Assert
    app.scroll_lines(-3);
    assert_eq!(app.scroll_offset(), 2);
    app.scroll_lines(-10);
    assert_eq!(app.scroll_offset(), 0);
    app.scroll_lines(100);
    assert_eq!(app.scroll_offset(), 8);
}

// ----- mouse: row -> message lookup -----

#[test]
fn message_at_row_maps_rows_and_skips_separators() {
    // Arrange: 2 one-line messages -> rows [frame, m0, sep, m1, frame].
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("a", "one"));
    app.push_message(msg("b", "two"));

    // Act / Assert
    assert_eq!(app.message_at_row(1), Some(0));
    assert_eq!(app.message_at_row(2), None); // separator row
    assert_eq!(app.message_at_row(3), Some(1));
    assert_eq!(app.message_at_row(0), None); // framing row
    assert_eq!(app.message_at_row(9), None); // past the end
}

#[test]
fn message_at_row_hits_continuation_rows_of_multiline_messages() {
    // Arrange: nick "alice" -> indent 7 at width 20; the body wraps to
    // 2 rows, so message 0 owns content rows 1 and 2.
    let mut app = App::new(20, 10);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("alice", "one two three four five"));

    // Act / Assert
    assert_eq!(app.message_at_row(1), Some(0));
    assert_eq!(app.message_at_row(2), Some(0));
    assert_eq!(app.message_at_row(3), None); // trailing frame
}

#[test]
fn message_at_row_applies_the_scroll_offset() {
    // Arrange: as in message_at_row_maps_rows_and_skips_separators,
    // scrolled so content row 4 is at viewport row 1 (offset 3).
    let mut app = App::new(40, 2);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("a", "one"));
    app.push_message(msg("b", "two"));
    app.set_scroll_offset(3);

    // Act / Assert: viewport row 1 shows content row 4 = trailing frame.
    assert_eq!(app.message_at_row(1), None);
    assert_eq!(app.message_at_row(0), Some(1)); // content row 3 = m1
}

// ----- mouse: hover state -----

#[test]
fn set_hover_message_validates_bounds() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("u", "m0"));

    // Act / Assert: valid indices stick, invalid ones clear.
    app.set_hover_message(Some(0));
    assert_eq!(app.hovered(), Some(0));
    app.set_hover_message(Some(9));
    assert_eq!(app.hovered(), None);
    app.set_hover_message(None);
    assert_eq!(app.hovered(), None);
}

#[test]
fn switching_channel_clears_the_hover() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#c");
    app.open_channel("srv", "#b");
    app.select_buffer(0);
    app.push_message(msg("u", "m0"));
    app.set_hover_message(Some(0));

    // Act
    app.select_buffer(1);

    // Assert: the pointer position is stale for the new channel.
    assert_eq!(app.hovered(), None);
}

#[test]
fn eviction_adjusts_the_hover_like_the_selection() {
    // Arrange: message cap keeps two messages. Hover the newest.
    let mut app = App::with_caps(10, 5, 2, 8);
    app.push_message(msg("u", "aa bb cc 0"));
    app.push_message(msg("u", "aa bb cc 1"));
    app.set_hover_message(Some(1));

    // Act: a third message evicts the oldest ("aa bb cc 0").
    app.push_message(msg("u", "aa bb cc 2"));

    // Assert: the hover shifted down with the list, like `selected`.
    assert_eq!(app.hovered(), Some(0));
    assert_eq!(app.messages().front().unwrap().text, "aa bb cc 1");
}

// ----- mouse: click semantics -----

#[test]
fn click_message_selects_and_focuses_the_pane() {
    // Arrange: viewing a channel with the composer focused.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("a", "one"));
    app.push_message(msg("b", "two"));
    app.tab(); // Sidebar -> Messages
    app.tab(); // Messages -> Composer
    assert_eq!(app.focus(), Focus::Composer);

    // Act
    app.click_message(1);

    // Assert
    assert_eq!(app.selected(), Some(1));
    assert_eq!(app.focus(), Focus::Messages);
}

#[test]
fn click_message_out_of_bounds_only_focuses() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#c");
    app.select_buffer(0);
    app.push_message(msg("a", "one"));

    // Act
    app.click_message(9);

    // Assert: focus moves, the (invalid) selection does not change.
    assert_eq!(app.focus(), Focus::Messages);
    assert_eq!(app.selected(), None);
}

#[test]
fn click_sidebar_channel_row_opens_it_and_focuses_the_composer() {
    // Arrange: rows = [srv, #a, #b], viewing #a via the keyboard path.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.open_channel("srv", "#b");
    app.select_buffer(0);

    // Act: click #b's row (2) - sidebar is NOT focused.
    app.click_sidebar_row(2);

    // Assert
    assert_eq!(active_target(&app), Some(("srv", "#b")));
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.sidebar_cursor(), None);
}

#[test]
fn click_sidebar_server_row_opens_the_console() {
    // Arrange: rows = [srv, #a]; the sidebar is NOT focused.
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");

    // Act
    app.click_sidebar_row(0);

    // Assert: works without sidebar focus; the console opens with the
    // composer focused, like a channel row.
    assert_eq!(active_target(&app), Some(("srv", "")));
    assert_eq!(app.focus(), Focus::Composer);
    assert_eq!(app.sidebar_cursor(), None);
}

#[test]
fn click_sidebar_row_out_of_range_is_ignored() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");

    // Act / Assert: no panic, no state change.
    app.click_sidebar_row(5);
    assert_eq!(active_target(&app), None);
}

#[test]
fn focus_setters_move_focus_without_selecting() {
    // Arrange
    let mut app = App::new(40, 10);
    app.open_channel("srv", "#a");
    app.select_buffer(0);

    // Act / Assert: focus moves, no selection is created.
    app.focus_messages();
    assert_eq!(app.focus(), Focus::Messages);
    assert_eq!(app.selected(), None);
    app.focus_composer();
    assert_eq!(app.focus(), Focus::Composer);
}
#[test]
fn resize_measures_each_message_once_and_keeps_ids() {
    let mut app = App::new(80, 20);
    app.open_channel("srv", "#a");
    for _ in 0..5000 {
        app.push_message(RoutedMessage::chat("srv", "#a", "n", &"x".repeat(100)));
    }
    app.select_buffer(0);
    let ids: Vec<_> = app.messages().iter().map(|m| m.id).collect();
    let before = app.view().unwrap().layout.measurements;
    app.resize(10, 20);
    assert_eq!(app.view().unwrap().layout.measurements - before, 5000);
    assert_eq!(app.messages().iter().map(|m| m.id).collect::<Vec<_>>(), ids);
    app.push_message(RoutedMessage::chat("srv", "#a", "n", "next"));
    assert_eq!(app.view().unwrap().layout.measurements - before, 5001);
}
