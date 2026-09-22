use super::*;

fn connected_session(
    capacity: usize,
) -> (
    Session,
    HashMap<String, ConnectionHandle>,
    tokio::sync::mpsc::Receiver<OutgoingMessage>,
    tokio::sync::mpsc::Receiver<ConnectionCommand>,
) {
    let mut session = Session::default();
    let console = session.open_server("SRV");
    session.select_buffer_id(console);
    session.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Connected,
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

fn submit(
    session: &mut Session,
    connections: &HashMap<String, ConnectionHandle>,
    input: &str,
) -> SubmissionEffect {
    session.restore_input_at(input.into(), 1);
    submit_composer(
        session,
        &Config {
            servers: Default::default(),
        },
        connections,
        &mut String::new(),
    )
}

#[test]
fn join_opens_a_channel_and_clears_only_the_source_draft_without_echo() {
    let (mut session, connections, mut messages, mut commands) = connected_session(8);
    let console = session.active_buffer().unwrap().id;
    let effect = submit(&mut session, &connections, "/join #New");
    let SubmissionEffect::Activate(channel) = effect else {
        panic!("join must activate the new channel");
    };
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(
        session.buffer(channel).unwrap().kind,
        BufferKind::Channel("#New".into())
    );
    assert_eq!(session.active_buffer().unwrap().id, console);
    assert!(session.input().is_empty());
    assert!(session.messages_for(console).is_empty());
    assert!(session.messages_for(channel).is_empty());
    assert!(messages.try_recv().is_err());
    assert!(commands.try_recv().is_ok());
    assert_eq!(
        session.connection_state("srv", Some("#new")),
        ConnectionState::Connecting
    );
}

#[test]
fn raw_join_uses_channel_controls_without_a_raw_echo() {
    let (mut session, connections, mut messages, mut commands) = connected_session(8);
    assert!(matches!(
        submit(&mut session, &connections, "JOIN #new"),
        SubmissionEffect::Activate(_)
    ));
    assert!(messages.try_recv().is_err());
    assert!(commands.try_recv().is_ok());
    assert!(session.messages().is_empty());
    assert!(session.input().is_empty());
}

fn channel_event(session: &mut Session, channel: &str, state: ChannelState, desired: bool) {
    session.handle_event(IrcEvent::Channel(
        "srv".into(),
        channel.into(),
        ChannelStatus { state, desired },
    ));
}

fn applied(session: &mut Session, channel: &str) {
    session.handle_event(IrcEvent::ChannelControlApplied(
        "SRV".into(),
        channel.into(),
    ));
}

#[test]
fn joined_and_joining_channels_reopen_without_resending_or_losing_history_and_draft() {
    for state in [ChannelState::Joined, ChannelState::Joining] {
        let (mut session, connections, mut messages, mut commands) = connected_session(8);
        let console = session.active_buffer().unwrap().id;
        let channel = session.open_channel("srv", "#Room");
        session.select_buffer_id(channel);
        session.restore_input_at("saved draft".into(), 4);
        session.push_message(RoutedMessage::chat("srv", "#Room", "alice", "history"));
        channel_event(&mut session, "#Room", state, true);
        session.select_buffer_id(console);
        assert_eq!(
            submit(&mut session, &connections, "/join #ROOM"),
            SubmissionEffect::Activate(channel)
        );
        assert!(session.input().is_empty());
        assert!(commands.try_recv().is_err());
        assert!(messages.try_recv().is_err());
        assert_eq!(session.buffer_count(), 2);
        assert_eq!(session.messages_for(channel)[0].text, "history");
        session.select_buffer_id(channel);
        assert_eq!(session.input(), "saved draft");
        assert_eq!(session.input_cursor(), 4);
    }
}

#[test]
fn part_resolves_current_channel_and_explicit_channel_from_any_buffer() {
    for (source, input) in [
        (BufferKind::Channel("#Room".into()), "/part"),
        (BufferKind::Channel("#Room".into()), "/part bye  all"),
        (BufferKind::Server, "/part #ROOM bye  all"),
        (BufferKind::Query("alice".into()), "/part #ROOM bye  all"),
    ] {
        let (mut session, connections, mut messages, mut commands) = connected_session(8);
        let channel = session.open_channel("srv", "#Room");
        channel_event(&mut session, "#Room", ChannelState::Joined, true);
        let source = session.open_buffer("srv", source);
        session.select_buffer_id(source);
        assert_eq!(
            submit(&mut session, &connections, input),
            SubmissionEffect::None
        );
        assert_eq!(
            commands.try_recv().unwrap(),
            ConnectionCommand::Part {
                channel: "#Room".into(),
                reason: input.contains("bye").then(|| "bye  all".into()),
            }
        );
        assert_eq!(
            session.channel_status("SRV", "#room"),
            ChannelStatus {
                state: ChannelState::Parting,
                desired: false
            }
        );
        assert_eq!(session.active_buffer().unwrap().id, source);
        assert!(session.input().is_empty());
        assert!(messages.try_recv().is_err());
        assert!(!session.buffer(channel).unwrap().hidden);
        assert!(session.messages_for(channel).is_empty());
        session.select_buffer_id(channel);
        submit(&mut session, &connections, "must not send");
        assert_eq!(session.input(), "must not send");
        assert!(messages.try_recv().is_err());
    }
}

#[test]
fn pending_channel_commands_ignore_stale_snapshots_until_matching_barrier() {
    let (mut session, connections, mut messages, mut commands) = connected_session(8);
    let channel = session.open_channel("srv", "#Room");
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    session.select_buffer_id(channel);
    submit(&mut session, &connections, "/part");
    commands.try_recv().unwrap();
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    applied(&mut session, "#other");
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    assert_eq!(
        session.channel_status("srv", "#Room").state,
        ChannelState::Parting
    );
    submit(&mut session, &connections, "blocked");
    assert!(messages.try_recv().is_err());
    applied(&mut session, "#ROOM");
    channel_event(&mut session, "#Room", ChannelState::NotJoined, false);
    assert_eq!(
        session.channel_status("srv", "#room"),
        ChannelStatus::default()
    );
}

#[test]
fn multiple_pending_controls_require_every_channel_barrier() {
    let (mut session, connections, _messages, mut commands) = connected_session(8);
    let channel = session.open_channel("srv", "#Room");
    channel_event(&mut session, "#Room", ChannelState::NotJoined, true);
    session.select_buffer_id(channel);
    submit(&mut session, &connections, "/part");
    assert_eq!(
        session.channel_status("srv", "#Room"),
        ChannelStatus::default()
    );
    assert_eq!(
        submit(&mut session, &connections, "/join #ROOM"),
        SubmissionEffect::Activate(channel)
    );
    assert!(matches!(
        commands.try_recv().unwrap(),
        ConnectionCommand::Part { .. }
    ));
    assert_eq!(
        commands.try_recv().unwrap(),
        ConnectionCommand::Join("#Room".into())
    );
    applied(&mut session, "#room");
    channel_event(&mut session, "#Room", ChannelState::NotJoined, false);
    assert_eq!(
        session.channel_status("srv", "#Room"),
        ChannelStatus {
            state: ChannelState::Joining,
            desired: true
        }
    );
    applied(&mut session, "#room");
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    assert_eq!(
        session.channel_status("srv", "#Room").state,
        ChannelState::Joined
    );
}

#[test]
fn opposite_in_flight_commands_preserve_draft_and_repeated_part_does_not_queue() {
    for (state, desired, input, accepted) in [
        (ChannelState::Joining, true, "/part", false),
        (ChannelState::Parting, false, "/join #Room", false),
        (ChannelState::Parting, false, "/part", true),
        (ChannelState::NotJoined, false, "/part", true),
    ] {
        let (mut session, connections, mut messages, mut commands) = connected_session(8);
        let channel = session.open_channel("srv", "#Room");
        channel_event(&mut session, "#Room", state, desired);
        session.select_buffer_id(channel);
        submit(&mut session, &connections, input);
        assert!(commands.try_recv().is_err());
        assert!(messages.try_recv().is_err());
        assert_eq!(
            session.channel_status("srv", "#Room"),
            ChannelStatus { state, desired }
        );
        assert_eq!(session.input(), if accepted { "" } else { input });
        if !accepted {
            assert_eq!(session.input_cursor(), 1);
        }
    }
}

#[test]
fn invalid_and_offline_commands_preserve_source_draft_and_do_not_open_buffers() {
    for state in [
        ConnectionState::Connected,
        ConnectionState::Connecting,
        ConnectionState::Stopped,
    ] {
        let (mut session, connections, mut messages, mut commands) = connected_session(8);
        session.apply_connection_event(&IrcEvent::Connection("srv".into(), state));
        let mut inputs = vec![
            "/join #",
            "/join #one key",
            "/join #one,#two",
            "/part",
            "/part bye",
            "/part #unknown",
            "JOIN #one key",
            "JOIN 0",
            "PART #unknown",
            "JOIN #one,#two",
            "PART #one,#two",
        ];
        if state != ConnectionState::Connected {
            inputs.push("/join #one");
            inputs.push("JOIN #one");
        }
        for input in inputs {
            assert_eq!(
                submit(&mut session, &connections, input),
                SubmissionEffect::None,
                "{state:?}: {input}"
            );
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 1);
            assert_eq!(session.buffer_count(), 1);
            assert!(session.messages().is_empty());
            assert!(commands.try_recv().is_err());
            assert!(messages.try_recv().is_err());
        }
    }
}

#[test]
fn queue_full_or_closed_and_wire_limit_fail_before_channel_mutation() {
    for closed in [false, true] {
        for input in ["/join #new", "/part #Room", "JOIN #new", "PART #Room"] {
            let (mut session, connections, mut messages, mut commands) = connected_session(1);
            let channel = session.open_channel("srv", "#Room");
            channel_event(&mut session, "#Room", ChannelState::Joined, true);
            if closed {
                commands.close();
            } else {
                connections["srv"]
                    .control
                    .try_send(ConnectionCommand::Back)
                    .unwrap();
            }
            submit(&mut session, &connections, input);
            assert_eq!(session.input(), input);
            assert_eq!(session.input_cursor(), 1);
            assert_eq!(session.buffer_count(), 2);
            assert_eq!(
                session.channel_status("srv", "#Room").state,
                ChannelState::Joined
            );
            assert!(messages.try_recv().is_err());
            assert!(session.messages_for(channel).is_empty());
        }
    }
    let (mut session, connections, mut messages, mut commands) = connected_session(8);
    session.open_channel("srv", "#Room");
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    for input in [
        format!("/join #{}", "a".repeat(510)),
        format!("/part #Room {}", "x".repeat(510)),
        format!("JOIN #{}", "a".repeat(510)),
        format!("PART #Room :{}", "x".repeat(510)),
    ] {
        submit(&mut session, &connections, &input);
        assert_eq!(session.input(), input);
        assert_eq!(session.input_cursor(), 1);
        assert_eq!(session.buffer_count(), 2);
        assert_eq!(
            session.channel_status("srv", "#Room").state,
            ChannelState::Joined
        );
        assert!(commands.try_recv().is_err());
        assert!(messages.try_recv().is_err());
    }
}

#[test]
fn raw_part_uses_same_control_and_blocks_channel_sending() {
    let (mut session, connections, mut messages, mut commands) = connected_session(8);
    let channel = session.open_channel("srv", "#Room");
    channel_event(&mut session, "#Room", ChannelState::Joined, true);
    submit(&mut session, &connections, "PART #ROOM :bye  all");
    assert_eq!(
        commands.try_recv().unwrap(),
        ConnectionCommand::Part {
            channel: "#Room".into(),
            reason: Some("bye  all".into())
        }
    );
    assert!(messages.try_recv().is_err());
    assert!(session.messages().is_empty());
    assert_eq!(
        session.channel_status("srv", "#Room").state,
        ChannelState::Parting
    );
    session.select_buffer_id(channel);
    submit(&mut session, &connections, "blocked");
    assert!(messages.try_recv().is_err());
}

#[test]
fn disconnect_preserves_intent_and_left_channels_never_enter_joining_on_reconnect() {
    let (mut session, _connections, _messages, _commands) = connected_session(8);
    session.open_channel("srv", "#stay");
    session.open_channel("srv", "#left");
    channel_event(&mut session, "#stay", ChannelState::Joined, true);
    channel_event(&mut session, "#left", ChannelState::NotJoined, false);
    session.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Stopped,
    ));
    assert_eq!(
        session.channel_status("srv", "#stay"),
        ChannelStatus {
            state: ChannelState::NotJoined,
            desired: true
        }
    );
    assert_eq!(
        session.channel_status("srv", "#left"),
        ChannelStatus::default()
    );
    session.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Connecting,
    ));
    assert_eq!(
        session.channel_status("srv", "#stay").state,
        ChannelState::Joining
    );
    assert_eq!(
        session.channel_status("srv", "#left"),
        ChannelStatus::default()
    );
    assert_eq!(
        session.connection_state("srv", Some("#left")),
        ConnectionState::Stopped
    );
}

#[test]
fn unknown_own_join_creates_buffer_before_following_message_without_stealing_focus() {
    let (mut session, _connections, _messages, _commands) = connected_session(8);
    let console = session.active_buffer().unwrap().id;
    session.restore_input("unfinished".into());
    assert_eq!(
        session.channel_status("srv", "#Unknown"),
        ChannelStatus::default()
    );
    channel_event(&mut session, "#Unknown", ChannelState::Joined, true);
    session.handle_event(IrcEvent::Message(RoutedMessage::chat(
        "srv", "#unknown", "me", "joined",
    )));
    assert_eq!(session.buffer_count(), 2);
    let channel = session.open_channel("srv", "#UNKNOWN");
    assert_eq!(session.messages_for(channel)[0].text, "joined");
    assert_eq!(
        session.channel_status("srv", "#unknown").state,
        ChannelState::Joined
    );
    assert_eq!(session.active_buffer().unwrap().id, console);
    assert_eq!(session.input(), "unfinished");
}

#[test]
fn server_barrier_also_blocks_unknown_channel_creation_and_status_snapshots() {
    let (mut session, _connections, _messages, _commands) = connected_session(8);
    session.begin_connection_change("srv", ConnectionState::Stopped);
    channel_event(&mut session, "#new", ChannelState::Joined, true);
    assert_eq!(session.buffer_count(), 1);
    session.handle_event(IrcEvent::ControlApplied("srv".into()));
    channel_event(&mut session, "#new", ChannelState::Joined, true);
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(
        session.connection_state("srv", Some("#new")),
        ConnectionState::Stopped
    );
}

#[test]
fn configuration_starts_desired_channels_joining_without_selecting_a_buffer() {
    let config = Config::parse(
        r##"
[servers.srv]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=["#Room"]
"##,
    )
    .unwrap();
    let mut session = Session::default();
    session.register_config(&config);
    assert_eq!(
        session.channel_status("srv", "#room"),
        ChannelStatus {
            state: ChannelState::Joining,
            desired: true
        }
    );
    assert!(session.active_buffer().is_none());
}
