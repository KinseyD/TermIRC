use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use termirc::config::ServerConfig;
use termirc::connection::{
    ConnectionHandle, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy,
};
use termirc::core::{BufferKind, ConnectionCommand, MessageKind, OutgoingMessage, RoutedMessage};

const WAIT: Duration = Duration::from_secs(3);

struct MockSession {
    listener: TcpListener,
    socket: TcpStream,
    reader: BufReader<TcpStream>,
    events: mpsc::Receiver<IrcEvent>,
    handle: Option<ConnectionHandle>,
    worker: Option<JoinHandle<()>>,
    registration: Vec<IrcEvent>,
}

impl MockSession {
    fn start(channels: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = ServerConfig {
            username: "test".into(),
            nickname: "requested".into(),
            password: String::new(),
            server: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            use_tls: false,
            channels: channels.clone(),
            queries: vec![],
        };
        let (sender, events) = mpsc::channel();
        let (worker, handle) = spawn_irc_with_policy(
            server,
            "srv".into(),
            channels,
            sender,
            RetryPolicy {
                max_retries: 0,
                delay: Duration::from_millis(30),
                timeout: WAIT,
            },
        );
        let (socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        let reader = BufReader::new(socket.try_clone().unwrap());
        let mut session = Self {
            listener,
            socket,
            reader,
            events,
            handle: Some(handle),
            worker: Some(worker),
            registration: Vec::new(),
        };
        while !session.read_line().starts_with("USER ") {}
        session.send(":mock 001 assigned :Welcome\r\n");
        let events = session.receive_until(|event| {
            *event == IrcEvent::Connection("srv".into(), ConnectionState::Connected)
        });
        assert!(events.contains(&IrcEvent::Nickname("srv".into(), "assigned".into())));
        session.registration = events;
        session
    }

    fn reconnect(&mut self) -> Vec<IrcEvent> {
        self.control(ConnectionCommand::Reconnect);
        let (socket, _) = self.listener.accept().unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        self.reader = BufReader::new(socket.try_clone().unwrap());
        self.socket = socket;
        while !self.read_line().starts_with("USER ") {}
        self.send(":mock 001 assigned :Welcome back\r\n");
        self.receive_until(|event| {
            *event == IrcEvent::Connection("srv".into(), ConnectionState::Connected)
        })
    }

    fn send(&mut self, lines: &str) {
        self.socket.write_all(lines.as_bytes()).unwrap();
    }

    fn read_line(&mut self) -> String {
        let mut line = String::new();
        assert_ne!(self.reader.read_line(&mut line).unwrap(), 0);
        line
    }

    fn receive_until(&self, predicate: impl Fn(&IrcEvent) -> bool) -> Vec<IrcEvent> {
        let deadline = Instant::now() + WAIT;
        let mut events = Vec::new();
        loop {
            let event = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("missing event: {error}; received {events:?}"));
            let done = predicate(&event);
            events.push(event);
            if done {
                return events;
            }
        }
    }

    fn messages_until_done(&mut self) -> Vec<RoutedMessage> {
        self.send(":mock NOTICE * :done\r\n");
        self.receive_until(
            |event| matches!(event, IrcEvent::Message(message) if message.content.text == "done"),
        )
        .into_iter()
        .filter_map(|event| match event {
            IrcEvent::Message(message) if message.content.text != "done" => Some(message),
            _ => None,
        })
        .filter(|message| message.content.text != "Welcome")
        .collect()
    }

    fn control(&self, command: ConnectionCommand) {
        self.handle
            .as_ref()
            .unwrap()
            .control
            .blocking_send(command)
            .unwrap();
    }
}

impl Drop for MockSession {
    fn drop(&mut self) {
        self.handle.take();
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn incoming_private_messages_use_welcome_nickname_without_joining() {
    let mut session = MockSession::start(vec![]);
    session.send(concat!(
        "@custom=value :Alice!u@h PRIVMSG ASSIGNED :hello\r\n",
        ":Alice!u@h PRIVMSG assigned :\x01ACTION waves\x01\r\n",
        ":assigned!u@h PRIVMSG assigned :note to self\r\n",
        ":Alice!u@h PRIVMSG requested :wrong initial nickname\r\n",
        ":Alice!u@h PRIVMSG stranger :not for us\r\n",
        ":Alice!u@h PRIVMSG #unknown :not configured\r\n",
        ":Alice!u@h PRIVMSG assigned :\x01VERSION\x01\r\n",
    ));
    let messages = session.messages_until_done();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert_eq!(messages[0].server.as_str(), "srv");
    assert_eq!(messages[0].target, BufferKind::Query("Alice".into()));
    assert_eq!(messages[0].content.nick, "Alice");
    assert_eq!(messages[0].content.text, "hello");
    assert_eq!(messages[0].content.kind, MessageKind::Chat);
    assert_eq!(
        messages[0].content.tags,
        vec![("custom".into(), Some("value".into()))]
    );
    assert_eq!(messages[1].target, BufferKind::Query("Alice".into()));
    assert_eq!(messages[1].content.text, "* waves");
    assert_eq!(messages[1].content.kind, MessageKind::Action);
    assert_eq!(messages[2].target, BufferKind::Query("assigned".into()));
    assert_eq!(messages[2].content.text, "note to self");
}

#[test]
fn incoming_private_targets_change_only_after_confirmed_own_nick() {
    let mut session = MockSession::start(vec![]);
    session.control(ConnectionCommand::Nick("renamed".into()));
    assert_eq!(session.read_line(), "NICK renamed\r\n");
    session.send(concat!(
        ":Alice!u@h PRIVMSG renamed :not confirmed\r\n",
        ":Alice!u@h PRIVMSG assigned :before confirmation\r\n",
    ));
    let messages = session.messages_until_done();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].content.text, "before confirmation");

    session.send(concat!(
        ":ASSIGNED!u@h NICK :renamed\r\n",
        ":Alice!u@h PRIVMSG assigned :stale nickname\r\n",
        ":Alice!u@h PRIVMSG RENAMED :after confirmation\r\n",
    ));
    session.receive_until(|event| *event == IrcEvent::Nickname("srv".into(), "renamed".into()));
    let messages = session.messages_until_done();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].target, BufferKind::Query("Alice".into()));
    assert_eq!(messages[0].content.text, "after confirmation");
}

#[test]
fn peer_nickname_changes_are_emitted_without_an_existing_query() {
    let mut session = MockSession::start(vec![]);
    session.send(concat!(
        ":Alice!u@h NICK :Alicia\r\n",
        ":server.example NICK :not-a-peer\r\n",
        ":mock NOTICE * :done\r\n",
    ));
    let events = session.receive_until(
        |event| matches!(event, IrcEvent::Message(message) if message.content.text == "done"),
    );
    let nicknames: Vec<_> = events
        .into_iter()
        .filter(|event| matches!(event, IrcEvent::Nickname(..) | IrcEvent::PeerNickname(..)))
        .collect();
    assert_eq!(
        nicknames,
        vec![IrcEvent::PeerNickname(
            "srv".into(),
            "Alice".into(),
            "Alicia".into(),
        )]
    );
    session.send(":Alicia!u@h PRIVMSG assigned :still addressed to us\r\n");
    let messages = session.messages_until_done();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].target, BufferKind::Query("Alicia".into()));
}

#[test]
fn private_reply_numerics_preserve_query_and_structured_error_destinations() {
    let mut session = MockSession::start(vec![]);
    session.send(concat!(
        ":mock 301 assigned Alice :Out to lunch\r\n",
        ":mock 401 assigned nobody :No such nick/channel\r\n",
        ":Alice!u@h NOTICE assigned :private notice\r\n",
    ));
    let messages = session.messages_until_done();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert_eq!(messages[0].target, BufferKind::Query("Alice".into()));
    assert_eq!(messages[0].content.kind, MessageKind::Console);
    assert_eq!(messages[0].content.text, "Alice Out to lunch");
    assert_eq!(messages[1].target, BufferKind::Server);
    assert_eq!(messages[1].content.text, "401 nobody No such nick/channel");
    assert_eq!(
        messages[1].content.kind,
        MessageKind::Error {
            code: 401,
            target: Some("nobody".into()),
            reason: "No such nick/channel".into(),
        }
    );
    assert_eq!(messages[2].target, BufferKind::Server);
    assert_eq!(messages[2].content.kind, MessageKind::Console);
    assert_eq!(messages[2].content.text, "private notice");
    assert!(
        messages
            .iter()
            .all(|message| message.content.nick.is_empty())
    );
}

#[test]
fn outgoing_private_message_reaches_wire_before_any_join_confirmation() {
    let mut session = MockSession::start(vec!["#pending".into()]);
    assert_eq!(session.read_line(), "JOIN #pending\r\n");
    session
        .handle
        .as_ref()
        .unwrap()
        .outgoing
        .blocking_send(OutgoingMessage::Privmsg {
            server: "srv".into(),
            target: "Alice".into(),
            text: "hello  privately".into(),
        })
        .unwrap();
    assert_eq!(session.read_line(), "PRIVMSG Alice :hello  privately\r\n");
    assert!(session.messages_until_done().is_empty());
}

#[test]
fn query_ui_commands_and_worker_preserve_conversation_across_reconnect() {
    use termirc::application::submit_composer;
    use termirc::config::Config;
    use termirc::tui::App;

    let mut transport = MockSession::start(vec![]);
    let mut app = App::new(40, 5);
    let console = app.open_server("srv");
    app.activate_buffer(console);
    for event in transport.registration.clone() {
        app.session.handle_event(event);
    }
    let config = Config {
        servers: Default::default(),
    };
    let handle = transport.handle.as_ref().unwrap();
    let handles = std::collections::HashMap::from([(
        "srv".into(),
        ConnectionHandle {
            outgoing: handle.outgoing.clone(),
            control: handle.control.clone(),
        },
    )]);
    let mut status = String::new();
    app.restore_input("/query Alice".into());
    let effect = submit_composer(&mut app.session, &config, &handles, &mut status);
    app.apply_submission_effect(effect);
    let query = app.active_buffer().unwrap().id;
    app.restore_input("hello".into());
    submit_composer(&mut app.session, &config, &handles, &mut status);
    assert_eq!(transport.read_line(), "PRIVMSG Alice hello\r\n");
    transport.send(concat!(
        ":Alice!u@h PRIVMSG assigned :reply\r\n",
        ":mock 301 assigned Alice :Away\r\n",
        ":mock 401 assigned Alice :No such nick\r\n",
    ));
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }
    assert_eq!(app.messages_for(query).len(), 4);
    assert_eq!(
        app.messages_for(query)[0].content.direction,
        termirc::core::Direction::Outgoing
    );
    assert_eq!(app.messages_for(query)[1].text, "reply");
    assert_eq!(app.messages_for(query)[2].text, "Alice Away");
    assert!(matches!(
        app.messages_for(query)[3].kind,
        MessageKind::Error { code: 401, .. }
    ));
    app.restore_input("/close".into());
    let effect = submit_composer(&mut app.session, &config, &handles, &mut status);
    app.apply_submission_effect(effect);
    assert_eq!(app.active_buffer().unwrap().id, console);
    assert!(app.buffer(query).unwrap().hidden);
    for event in transport.reconnect() {
        app.session.handle_event(event);
    }
    assert!(app.buffer(query).unwrap().hidden);
    assert_eq!(app.messages_for(query).len(), 4);
    transport.send(":Alice!u@h NICK :Alicia\r\n:mock NOTICE * :renamed\r\n");
    for event in transport.receive_until(
        |event| matches!(event, IrcEvent::Message(message) if message.content.text == "renamed"),
    ) {
        app.session.handle_event(event);
    }
    assert_eq!(
        app.buffer(query).unwrap().kind,
        BufferKind::Query("Alicia".into())
    );
    assert!(app.buffer(query).unwrap().hidden);
    transport.send(":Alicia!u@h PRIVMSG assigned :back\r\n");
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }
    assert_eq!(app.active_buffer().unwrap().id, console);
    assert!(!app.buffer(query).unwrap().hidden);
    assert!(app.buffer(query).unwrap().unread);
    app.restore_input("/query Alicia".into());
    let effect = submit_composer(&mut app.session, &config, &handles, &mut status);
    app.apply_submission_effect(effect);
    assert_eq!(app.active_buffer().unwrap().id, query);
    app.restore_input("again".into());
    submit_composer(&mut app.session, &config, &handles, &mut status);
    assert_eq!(transport.read_line(), "PRIVMSG Alicia again\r\n");
    drop(handles);
}
