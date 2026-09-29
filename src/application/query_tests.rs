use super::*;

fn incoming(nick: &str, text: &str) -> RoutedMessage {
    RoutedMessage {
        server: "srv".into(),
        target: BufferKind::Query(nick.into()),
        content: MessageContent::chat(nick, text),
    }
}

fn config() -> Config {
    Config {
        servers: Default::default(),
    }
}

#[test]
fn configured_queries_are_deduplicated_per_server_without_opening_a_view() {
    let config = Config::parse(
        r##"
[servers.first]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=["#one"]
queries=["Alice", "ALICE", "Bob"]
[servers.second]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=[]
queries=["Alice"]
[servers.legacy]
username="me"
nickname="me"
password=""
server="localhost"
port=6667
channels=[]
"##,
    )
    .unwrap();
    assert!(config.servers["legacy"].queries.is_empty());
    let mut session = Session::default();
    session.register_config(&config);
    assert_eq!(session.buffer_count(), 7);
    assert!(session.active_buffer().is_none());
    assert_eq!(
        session
            .buffers
            .iter()
            .map(|buffer| (&*buffer.server_label, buffer.kind.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("first", BufferKind::Server),
            ("first", BufferKind::Channel("#one".into())),
            ("first", BufferKind::Query("Alice".into())),
            ("first", BufferKind::Query("Bob".into())),
            ("second", BufferKind::Server),
            ("second", BufferKind::Query("Alice".into())),
            ("legacy", BufferKind::Server),
        ]
    );
    assert!(
        crate::connection::build_client_config(&config.servers["first"])
            .channels
            .is_empty()
    );
}

#[test]
fn incoming_query_creates_background_conversation_without_touching_draft() {
    let mut session = Session::default();
    let console = session.open_server("SRV");
    session.select_buffer(0);
    session.restore_input("in progress".into());
    session.handle_event(IrcEvent::Message(incoming("Alice", "hello")));
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(session.active_buffer().unwrap().id, console);
    assert_eq!(session.input(), "in progress");
    let query = session.open_buffer("srv", BufferKind::Query("alice".into()));
    assert_eq!(session.messages_for(query)[0].text, "hello");
    session.handle_event(IrcEvent::Message(incoming("ALICE", "again")));
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(session.messages_for(query).len(), 2);
}

#[test]
fn query_input_queues_privmsg_and_echoes_into_query_without_join() {
    let mut session = Session::default();
    let query = session.open_buffer("srv", BufferKind::Query("alice".into()));
    session.select_buffer(0);
    session.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Connected,
    ));
    session.apply_connection_event(&IrcEvent::Nickname("srv".into(), "me".into()));
    session.restore_input("hello".into());
    let (outgoing, mut received) = tokio::sync::mpsc::channel(1);
    let (control, mut commands) = tokio::sync::mpsc::channel(1);
    let handles = HashMap::from([("srv".into(), ConnectionHandle { outgoing, control })]);
    submit_composer(&mut session, &config(), &handles, &mut String::new());
    assert_eq!(
        received.try_recv().unwrap(),
        OutgoingMessage::Privmsg {
            server: "srv".into(),
            target: "alice".into(),
            text: "hello".into(),
        }
    );
    assert!(commands.try_recv().is_err());
    assert_eq!(session.messages_for(query)[0].nick, "me");
    assert_eq!(
        session.messages_for(query)[0].delivery,
        DeliveryState::Unconfirmed
    );
    assert_eq!(
        session.messages_for(query)[0].direction,
        Direction::Outgoing
    );
    assert_eq!(session.input(), "");
}

#[test]
fn local_query_command_does_not_require_connected_handle() {
    let mut session = Session::default();
    session.open_server("srv");
    session.select_buffer(0);
    session.restore_input("/query Alice".into());
    submit_composer(&mut session, &config(), &HashMap::new(), &mut String::new());
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(session.input(), "");
}

#[test]
fn query_command_preserves_destination_draft_and_reuses_hidden_buffer() {
    let mut session = Session::default();
    let console = session.open_server("srv");
    let query = session.open_query("srv", "alice");
    session.select_buffer_id(query);
    session.restore_input_at("unfinished".into(), 3);
    session.push_message(incoming("alice", "saved"));
    assert_eq!(session.close_query(query), Some(console));
    assert!(session.buffer(query).unwrap().hidden);
    session.select_buffer_id(console);
    session.restore_input("/query ALICE".into());
    let effect = submit_composer(&mut session, &config(), &HashMap::new(), &mut String::new());
    assert_eq!(effect, SubmissionEffect::Activate(query));
    assert_eq!(session.active, Some(console));
    assert_eq!(session.input(), "");
    assert!(!session.buffer(query).unwrap().hidden);
    session.select_buffer_id(query);
    assert_eq!(session.input(), "unfinished");
    assert_eq!(session.input_cursor(), 3);
    assert_eq!(session.messages()[0].text, "saved");
}

#[test]
fn close_command_hides_only_queries_without_connection_controls() {
    let mut session = Session::default();
    let console = session.open_server("srv");
    let query = session.open_query("srv", "alice");
    session.select_buffer_id(query);
    session.restore_input("/close".into());
    let effect = submit_composer(&mut session, &config(), &HashMap::new(), &mut String::new());
    assert_eq!(effect, SubmissionEffect::Activate(console));
    assert!(session.buffer(query).unwrap().hidden);
    session.select_buffer_id(console);
    session.restore_input("/close".into());
    assert_eq!(
        submit_composer(&mut session, &config(), &HashMap::new(), &mut String::new()),
        SubmissionEffect::None
    );
    assert_eq!(session.input(), "/close");
}

#[test]
fn incoming_restores_hidden_query_and_marks_unread_without_switching() {
    let mut session = Session::default();
    let console = session.open_server("srv");
    let query = session.open_query("srv", "Alice");
    session.close_query(query);
    session.select_buffer_id(console);
    session.push_message(incoming("ALICE", "wake up"));
    assert_eq!(session.active, Some(console));
    assert_eq!(session.buffer_count(), 2);
    assert!(!session.buffer(query).unwrap().hidden);
    assert!(session.buffer(query).unwrap().unread);
    session.mark_read(query);
    assert!(!session.buffer(query).unwrap().unread);
}

#[test]
fn notices_and_errors_never_create_or_restore_queries() {
    let mut session = Session::default();
    let console = session.open_server("srv");
    let query = session.open_query("srv", "alice");
    let error = RoutedMessage {
        server: "srv".into(),
        target: BufferKind::Server,
        content: MessageContent {
            kind: MessageKind::Error {
                code: 401,
                target: Some("alice".into()),
                reason: "absent".into(),
            },
            ..MessageContent::console("401 alice absent")
        },
    };
    let away = RoutedMessage {
        server: "srv".into(),
        target: BufferKind::Query("alice".into()),
        content: MessageContent::console("alice is away"),
    };
    session.push_message(error.clone());
    session.push_message(away.clone());
    assert_eq!(session.messages_for(query).len(), 2);
    session.close_query(query);
    session.push_message(error);
    session.push_message(away);
    assert_eq!(session.messages_for(query).len(), 2);
    assert_eq!(session.messages_for(console).len(), 2);
    assert!(session.buffer(query).unwrap().hidden);
    assert!(!session.buffer(query).unwrap().unread);
    let unknown = RoutedMessage {
        server: "srv".into(),
        target: BufferKind::Query("nobody".into()),
        content: MessageContent::console("away"),
    };
    session.push_message(unknown);
    assert_eq!(session.buffer_count(), 2);
    assert_eq!(session.messages_for(console).len(), 3);
}

#[test]
fn confirmed_peer_rename_preserves_identity_and_handles_conflicts() {
    let mut session = Session::default();
    let original = session.open_query("srv", "alice");
    session.select_buffer_id(original);
    session.restore_input_at("draft".into(), 2);
    session.push_message(incoming("alice", "before"));
    session.close_query(original);
    session.handle_event(IrcEvent::PeerNickname(
        "srv".into(),
        "ALICE".into(),
        "bob".into(),
    ));
    assert_eq!(
        session.buffer(original).unwrap().kind,
        BufferKind::Query("bob".into())
    );
    assert!(session.buffer(original).unwrap().hidden);
    assert_eq!(session.buffer(original).unwrap().draft.text(), "draft");
    assert_eq!(session.messages_for(original)[0].text, "before");
    let other = session.open_query("srv", "carol");
    session.handle_event(IrcEvent::PeerNickname(
        "srv".into(),
        "bob".into(),
        "carol".into(),
    ));
    assert_eq!(
        session.buffer(original).unwrap().kind,
        BufferKind::Query("bob".into())
    );
    assert!(session.buffer(original).unwrap().send_blocked);
    assert!(
        session
            .messages_for(original)
            .back()
            .unwrap()
            .text
            .contains("/query carol")
    );
    assert!(session.messages_for(other).is_empty());
    assert_eq!(session.open_query("srv", "BOB"), original);
    assert!(!session.buffer(original).unwrap().send_blocked);
}

#[test]
fn self_addressed_messages_have_one_local_echo_even_for_identical_sends() {
    let mut session = Session::default();
    let query = session.open_query("srv", "me");
    for _ in 0..2 {
        let mut outgoing = incoming("me", "same");
        outgoing.content.direction = Direction::Outgoing;
        outgoing.content.delivery = DeliveryState::Unconfirmed;
        session.push_message(outgoing);
    }
    assert!(!session.buffer(query).unwrap().unread);
    session.push_message(incoming("me", "same"));
    session.push_message(incoming("me", "same"));
    assert_eq!(session.messages_for(query).len(), 2);
    session.push_message(incoming("me", "same"));
    assert_eq!(session.messages_for(query).len(), 3);
}

#[test]
fn self_echo_matching_does_not_survive_our_nickname_change() {
    let mut session = Session::default();
    let query = session.open_query("srv", "me");
    session.apply_connection_event(&IrcEvent::Nickname("srv".into(), "me".into()));
    let mut outgoing = incoming("me", "same");
    outgoing.content.direction = Direction::Outgoing;
    outgoing.content.delivery = DeliveryState::Unconfirmed;
    session.push_message(outgoing);
    session.apply_connection_event(&IrcEvent::Nickname("srv".into(), "new_me".into()));
    session.push_message(incoming("me", "same"));
    assert_eq!(session.messages_for(query).len(), 2);
}

#[test]
fn self_echo_tracking_is_bounded_by_history_and_reset_on_disconnect() {
    let mut session = Session::new(1);
    let query = session.open_query("srv", "me");
    for _ in 0..3 {
        let mut outgoing = incoming("me", "same");
        outgoing.content.direction = Direction::Outgoing;
        session.push_message(outgoing);
    }
    assert_eq!(session.pending_self_echoes[&query].len(), 1);
    session.apply_connection_event(&IrcEvent::Connection(
        "srv".into(),
        ConnectionState::Connecting,
    ));
    assert!(session.pending_self_echoes.is_empty());
    session.push_message(incoming("me", "same"));
    assert_eq!(
        session.messages_for(query)[0].direction,
        Direction::Incoming
    );
}

#[test]
fn query_commands_without_context_or_with_invalid_arguments_preserve_input() {
    for input in [
        "/query alice",
        "/close",
        "/query #channel",
        "/query alice,bob",
        "/query",
        "/close extra",
    ] {
        let mut session = Session::default();
        session.restore_input_at(input.into(), 2);
        let mut status = "unchanged".into();
        let effect = submit_composer(&mut session, &config(), &HashMap::new(), &mut status);
        assert_eq!(effect, SubmissionEffect::None);
        assert_eq!(session.buffer_count(), 0);
        assert_eq!(session.input(), input);
        assert_eq!(session.input_cursor(), 2);
        assert_eq!(status, "unchanged");
    }
}

#[test]
fn query_send_failures_keep_text_cursor_and_history_unchanged() {
    for failure in ["disconnected", "full", "oversized", "blocked"] {
        let mut session = Session::default();
        let query = session.open_query("srv", "alice");
        session.select_buffer_id(query);
        let text = if failure == "oversized" {
            "界".repeat(200)
        } else {
            " keep draft ".into()
        };
        session.restore_input_at(text.clone(), 2);
        if failure != "disconnected" {
            session.apply_connection_event(&IrcEvent::Connection(
                "srv".into(),
                ConnectionState::Connected,
            ));
        }
        if failure == "blocked" {
            session
                .buffers
                .iter_mut()
                .find(|buffer| buffer.id == query)
                .unwrap()
                .send_blocked = true;
        }
        let (outgoing, mut received) = tokio::sync::mpsc::channel(1);
        if failure == "full" {
            outgoing
                .try_send(OutgoingMessage::Raw {
                    server: "srv".into(),
                    line: "WHOIS other".into(),
                })
                .unwrap();
        }
        let handles = HashMap::from([(
            "srv".into(),
            ConnectionHandle {
                outgoing,
                control: tokio::sync::mpsc::channel(1).0,
            },
        )]);
        let mut status = String::new();
        submit_composer(&mut session, &config(), &handles, &mut status);
        assert_eq!(session.input(), text, "{failure}");
        assert_eq!(session.input_cursor(), 2);
        assert!(session.messages_for(query).is_empty());
        assert!(!status.is_empty());
        if failure == "full" {
            received.try_recv().unwrap();
        }
        assert!(received.try_recv().is_err());
    }
}
