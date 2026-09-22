use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use termirc::application::submit_composer;
use termirc::config::{Config, ServerConfig};
use termirc::connection::{
    ConnectionHandle, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy,
};
use termirc::core::{
    BufferKind, ChannelState, ChannelStatus, ConnectionCommand, MessageKind, OutgoingMessage,
};
use termirc::tui::App;

const WAIT: Duration = Duration::from_secs(5);

struct MockSession {
    listener: TcpListener,
    socket: Option<BufReader<TcpStream>>,
    events: mpsc::Receiver<IrcEvent>,
    handle: Option<ConnectionHandle>,
    worker: Option<JoinHandle<()>>,
    app: App,
    config: Config,
    status: String,
    observed: Vec<IrcEvent>,
}

impl MockSession {
    fn start(channels: &[&str]) -> Self {
        Self::with_timeout(channels, WAIT)
    }

    fn with_timeout(channels: &[&str], timeout: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = ServerConfig {
            username: "test".into(),
            nickname: "test".into(),
            password: String::new(),
            server: "127.0.0.1".into(),
            use_tls: false,
            port: listener.local_addr().unwrap().port(),
            channels: channels.iter().map(|channel| (*channel).into()).collect(),
            queries: vec![],
        };
        let config = Config {
            servers: [("srv".into(), server.clone())].into_iter().collect(),
        };
        let mut app = App::new(40, 5);
        app.session.register_config(&config);
        let console = app.open_server("srv");
        app.activate_buffer(console);
        let (sender, events) = mpsc::channel();
        let (worker, handle) = spawn_irc_with_policy(
            server.clone(),
            "srv".into(),
            server.channels,
            sender,
            RetryPolicy {
                max_retries: 0,
                delay: Duration::from_millis(30),
                timeout,
            },
        );
        let mut session = Self {
            listener,
            socket: None,
            events,
            handle: Some(handle),
            worker: Some(worker),
            app,
            config,
            status: String::new(),
            observed: Vec::new(),
        };
        session.accept();
        session.register();
        session
    }

    fn accept(&mut self) {
        let deadline = Instant::now() + WAIT;
        let socket = loop {
            match self.listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "worker did not connect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        self.socket = Some(BufReader::new(socket));
        let deadline = Instant::now() + WAIT;
        while !self.read_line_until(deadline).starts_with("USER ") {}
    }

    fn register(&mut self) {
        self.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
        self.receive_until(|event| {
            *event == IrcEvent::Connection("srv".into(), ConnectionState::Connected)
        });
        self.sync_events();
    }

    fn send(&mut self, lines: &str) {
        self.socket
            .as_mut()
            .unwrap()
            .get_mut()
            .write_all(lines.as_bytes())
            .unwrap();
    }

    fn read_line_until(&mut self, deadline: Instant) -> String {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "wire deadline elapsed");
        let reader = self.socket.as_mut().unwrap();
        reader.get_ref().set_read_timeout(Some(remaining)).unwrap();
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "socket closed");
        line.trim_end_matches(['\r', '\n']).into()
    }

    fn control(&self, command: ConnectionCommand) {
        self.handle
            .as_ref()
            .unwrap()
            .control
            .try_send(command)
            .unwrap();
    }

    fn outgoing(&self, message: OutgoingMessage) {
        self.handle
            .as_ref()
            .unwrap()
            .outgoing
            .try_send(message)
            .unwrap();
    }

    fn wire(&mut self) -> Vec<String> {
        self.outgoing(OutgoingMessage::Raw {
            server: "srv".into(),
            line: "PING :wire-barrier".into(),
        });
        let deadline = Instant::now() + WAIT;
        let mut lines = Vec::new();
        loop {
            let line = self.read_line_until(deadline);
            if line.starts_with("PING ") && line.contains("wire-barrier") {
                return lines;
            }
            lines.push(line);
        }
    }

    fn receive_until(&mut self, predicate: impl Fn(&IrcEvent) -> bool) -> Vec<IrcEvent> {
        let deadline = Instant::now() + WAIT;
        let mut received = Vec::new();
        loop {
            let event = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("missing event: {error}; received {received:?}"));
            let done = predicate(&event);
            self.app.session.handle_event(event.clone());
            self.observed.push(event.clone());
            received.push(event);
            if done {
                return received;
            }
        }
    }

    fn sync_events(&mut self) -> Vec<IrcEvent> {
        self.send(":mock NOTICE * :event-barrier\r\n");
        self.receive_until(
            |event| matches!(event, IrcEvent::Message(message) if message.content.text == "event-barrier"),
        )
    }

    fn wait_channel(&mut self, channel: &str, state: ChannelState, desired: bool) -> Vec<IrcEvent> {
        self.receive_until(|event| channel_event(event, channel, state, desired))
    }

    fn assert_channel(&self, channel: &str, state: ChannelState, desired: bool) {
        assert_eq!(
            self.app.session.channel_status("srv", channel),
            ChannelStatus { state, desired },
            "channel {channel}; observed {:?}",
            self.observed
        );
    }

    fn submit(&mut self, input: &str) {
        self.app.restore_input(input.into());
        let handle = self.handle.as_ref().unwrap();
        let connections = HashMap::from([(
            "srv".into(),
            ConnectionHandle {
                control: handle.control.clone(),
                outgoing: handle.outgoing.clone(),
            },
        )]);
        let effect = submit_composer(
            &mut self.app.session,
            &self.config,
            &connections,
            &mut self.status,
        );
        self.app.apply_submission_effect(effect);
    }

    fn confirm_join(&mut self, channel: &str) {
        self.send(&format!(":test!u@h JOIN {channel}\r\n"));
        self.wait_channel(channel, ChannelState::Joined, true);
    }

    fn reconnect(&mut self) {
        self.control(ConnectionCommand::Reconnect);
        if let Some(socket) = self.socket.take() {
            let _ = socket.get_ref().shutdown(Shutdown::Both);
        }
        self.accept();
        self.register();
    }
}

impl Drop for MockSession {
    fn drop(&mut self) {
        self.handle.take();
        if let Some(socket) = self.socket.take() {
            let _ = socket.get_ref().shutdown(Shutdown::Both);
        }
        if let Some(worker) = self.worker.take() {
            let deadline = Instant::now() + WAIT;
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if !std::thread::panicking() {
                assert!(
                    worker.is_finished(),
                    "worker did not stop after handle drop"
                );
                worker.join().unwrap();
            } else if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

fn channel_event(event: &IrcEvent, channel: &str, state: ChannelState, desired: bool) -> bool {
    matches!(event, IrcEvent::Channel(server, name, status)
        if server == "srv" && name.eq_ignore_ascii_case(channel)
            && *status == ChannelStatus { state, desired })
}

#[test]
fn configured_channels_seed_intent_but_opening_a_buffer_does_not_join() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.assert_channel("#SEED", ChannelState::Joining, true);
    session.app.open_channel("srv", "#view-only");
    session.assert_channel("#view-only", ChannelState::NotJoined, false);
    assert!(session.wire().is_empty());
    assert_eq!(session.config.servers["srv"].channels, ["#seed"]);
}

#[test]
fn slash_join_waits_for_own_confirmation_before_chat_and_part_keeps_buffer() {
    let mut session = MockSession::start(&[]);
    session.submit("/join #Live");
    assert_eq!(session.wire(), ["JOIN #Live"]);
    session.sync_events();
    session.assert_channel("#live", ChannelState::Joining, true);
    assert!(session.observed.contains(&IrcEvent::ChannelControlApplied(
        "srv".into(),
        "#Live".into()
    )));
    let channel = session.app.active_buffer().unwrap().id;
    assert_eq!(
        session.app.active_buffer().unwrap().kind,
        BufferKind::Channel("#Live".into())
    );
    session.submit("not ready");
    assert_eq!(session.app.input(), "not ready");
    assert!(session.app.messages_for(channel).is_empty());
    assert!(session.wire().is_empty());
    session.send(":someone!u@h JOIN #Live\r\n");
    session.sync_events();
    session.assert_channel("#live", ChannelState::Joining, true);
    session.confirm_join("#LIVE");
    session.submit("hello channel");
    assert_eq!(session.wire(), ["PRIVMSG #Live :hello channel"]);
    assert_eq!(session.app.messages_for(channel).len(), 1);
    session.submit("/part #Live leaving for lunch");
    assert_eq!(session.wire(), ["PART #Live :leaving for lunch"]);
    session.sync_events();
    session.assert_channel("#live", ChannelState::Parting, false);
    session.submit("keep this draft");
    assert_eq!(session.app.input(), "keep this draft");
    assert!(session.wire().is_empty());
    session.send(":test!u@h PART #live :leaving for lunch\r\n");
    session.wait_channel("#live", ChannelState::NotJoined, false);
    assert_eq!(session.app.active_buffer().unwrap().id, channel);
    assert!(!session.app.buffer(channel).unwrap().hidden);
    assert_eq!(session.app.input(), "keep this draft");
    assert_eq!(session.app.messages_for(channel).len(), 1);
    assert!(session.config.servers["srv"].channels.is_empty());
}

#[test]
fn own_unsolicited_join_creates_buffer_before_first_message_without_stealing_focus() {
    let mut session = MockSession::start(&[]);
    let console = session.app.active_buffer().unwrap().id;
    session.app.restore_input("console draft".into());
    session.send(concat!(
        ":other!u@h JOIN #unknown\r\n",
        ":other!u@h PRIVMSG #unknown :drop this\r\n",
        ":TEST!u@h JOIN #discovered\r\n",
        ":Alice!u@h PRIVMSG #DISCOVERED :first message\r\n",
        ":Alice!u@h PRIVMSG TEST :private message\r\n",
        ":Alice!u@h PRIVMSG stranger :not for us\r\n",
    ));
    let events = session.sync_events();
    let joined = events
        .iter()
        .position(|event| channel_event(event, "#discovered", ChannelState::Joined, true))
        .unwrap();
    let first_message = events.iter().position(|event| matches!(event, IrcEvent::Message(message) if message.content.text == "first message")).unwrap();
    assert!(joined < first_message, "{events:?}");
    assert!(!events.iter().any(|event| matches!(event, IrcEvent::Message(message) if matches!(message.content.text.as_str(), "drop this" | "not for us"))));
    assert_eq!(session.app.buffer_count(), 3);
    let dynamic = session.app.open_channel("srv", "#discovered");
    assert_eq!(session.app.messages_for(dynamic).len(), 1);
    assert_eq!(session.app.messages_for(dynamic)[0].text, "first message");
    assert!(events.iter().any(|event| matches!(event, IrcEvent::Message(message) if message.target == BufferKind::Query("Alice".into()) && message.content.text == "private message")));
    assert_eq!(session.app.active_buffer().unwrap().id, console);
    assert_eq!(session.app.input(), "console draft");
    assert!(session.wire().is_empty());
}

#[test]
fn join_failures_keep_intent_and_route_once_only_for_known_channels() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.send(":mock 473 test #seed :Invite only\r\n");
    session.wait_channel("#seed", ChannelState::NotJoined, true);
    let codes = [403, 405, 407, 437, 471, 473, 474, 475, 476, 477, 489];
    for code in codes {
        session.control(ConnectionCommand::Join("#runtime".into()));
        assert_eq!(session.wire(), ["JOIN #runtime"]);
        session.sync_events();
        session.send(&format!(
            ":mock {code} test #RUNTIME :Join denied\r\n:mock {code} test #unknown :Unknown channel\r\n"
        ));
        let events = session.sync_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| channel_event(event, "#runtime", ChannelState::NotJoined, true))
                .count(),
            1,
            "code {code}: {events:?}"
        );
        assert!(!events.iter().any(|event| matches!(event, IrcEvent::Channel(_, channel, _) if channel.eq_ignore_ascii_case("#unknown"))), "code {code}: {events:?}");
        let errors: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                IrcEvent::Message(message)
                    if matches!(message.content.kind, MessageKind::Error { .. }) =>
                {
                    Some(message)
                }
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 2, "code {code}: {events:?}");
        assert!(
            errors[0]
                .target
                .matches(&BufferKind::Channel("#runtime".into()))
        );
        assert_eq!(errors[1].target, BufferKind::Server);
        assert!(
            matches!(&errors[0].content.kind, MessageKind::Error { code: actual, target: Some(target), reason }
            if *actual == code && target == "#RUNTIME" && reason == "Join denied")
        );
        session.assert_channel("#runtime", ChannelState::NotJoined, true);
        assert!(
            session.wire().is_empty(),
            "JOIN failure must not auto-retry"
        );
    }
    let channel = session.app.open_channel("srv", "#runtime");
    assert_eq!(session.app.messages_for(channel).len(), codes.len());
    assert_eq!(session.app.buffer_count(), 3);
}

#[test]
fn cannot_send_error_does_not_clear_membership_and_not_on_channel_completes_part() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.confirm_join("#seed");
    session.send(":mock 404 test #seed :Cannot send to channel\r\n");
    session.sync_events();
    session.assert_channel("#seed", ChannelState::Joined, true);
    session.control(ConnectionCommand::Part {
        channel: "#seed".into(),
        reason: None,
    });
    assert_eq!(session.wire(), ["PART #seed"]);
    session.send(":mock 442 test #seed :Not on channel\r\n");
    session.wait_channel("#seed", ChannelState::NotJoined, false);
    session.reconnect();
    assert!(session.wire().is_empty());
}

#[test]
fn join_deadlines_are_independent_and_late_confirmation_resolves_uncertainty() {
    let mut session = MockSession::with_timeout(&[], Duration::from_millis(900));
    session.control(ConnectionCommand::Join("#first".into()));
    assert_eq!(session.wire(), ["JOIN #first"]);
    session.wait_channel("#first", ChannelState::Joining, true);
    std::thread::sleep(Duration::from_millis(450));
    session.control(ConnectionCommand::Join("#second".into()));
    assert_eq!(session.wire(), ["JOIN #second"]);
    session.wait_channel("#second", ChannelState::Joining, true);
    let events = session.wait_channel("#first", ChannelState::Uncertain, true);
    assert!(!events.iter().any(|event| channel_event(
        event,
        "#second",
        ChannelState::Uncertain,
        true
    )));
    session.assert_channel("#second", ChannelState::Joining, true);
    let first = session.app.open_channel("srv", "#first");
    session.app.activate_buffer(first);
    session.submit("no uncertain chat");
    assert_eq!(session.app.input(), "no uncertain chat");
    assert!(session.wire().is_empty());
    session.confirm_join("#first");
    session.assert_channel("#first", ChannelState::Joined, true);
    session.wait_channel("#second", ChannelState::Uncertain, true);
    session.assert_channel("#first", ChannelState::Joined, true);
    assert!(
        session.wire().is_empty(),
        "timeouts must not automatically repeat JOIN"
    );
}

#[test]
fn part_deadlines_are_independent_and_timeout_keeps_exit_intent() {
    let mut session = MockSession::with_timeout(&["#first", "#second"], Duration::from_millis(900));
    assert_eq!(session.wire(), ["JOIN #first", "JOIN #second"]);
    session.confirm_join("#first");
    session.confirm_join("#second");
    session.control(ConnectionCommand::Part {
        channel: "#first".into(),
        reason: None,
    });
    assert_eq!(session.wire(), ["PART #first"]);
    session.wait_channel("#first", ChannelState::Parting, false);
    std::thread::sleep(Duration::from_millis(450));
    session.control(ConnectionCommand::Part {
        channel: "#second".into(),
        reason: None,
    });
    assert_eq!(session.wire(), ["PART #second"]);
    session.wait_channel("#second", ChannelState::Parting, false);
    session.wait_channel("#first", ChannelState::Uncertain, false);
    session.assert_channel("#second", ChannelState::Parting, false);
    session.send(":test!u@h PART #first\r\n");
    session.wait_channel("#first", ChannelState::NotJoined, false);
    session.wait_channel("#second", ChannelState::Uncertain, false);
    assert!(
        session.wire().is_empty(),
        "timeouts must not automatically repeat PART"
    );
    session.reconnect();
    assert!(
        session.wire().is_empty(),
        "part timeout must not restore configured intent"
    );
}

#[test]
fn kick_keeps_intent_without_autoretry_but_unsolicited_own_part_clears_it() {
    let mut session =
        MockSession::with_timeout(&["#kicked", "#parted"], Duration::from_millis(250));
    assert_eq!(session.wire(), ["JOIN #kicked", "JOIN #parted"]);
    session.confirm_join("#kicked");
    session.confirm_join("#parted");
    session.send(concat!(
        ":op!u@h KICK #kicked somebody :not us\r\n",
        ":op!u@h KICK #unknown test :unknown\r\n",
    ));
    session.sync_events();
    session.assert_channel("#kicked", ChannelState::Joined, true);
    session.send(concat!(
        ":op!u@h KICK #kicked TEST :Removed\r\n",
        ":test!u@h PART #parted :Leaving\r\n",
    ));
    session.sync_events();
    session.assert_channel("#kicked", ChannelState::NotJoined, true);
    session.assert_channel("#parted", ChannelState::NotJoined, false);
    assert_eq!(session.app.buffer_count(), 3);
    std::thread::sleep(Duration::from_millis(300));
    assert!(session.wire().is_empty());
    session.reconnect();
    assert_eq!(session.wire(), ["JOIN #kicked"]);
}

#[test]
fn late_join_after_part_is_countered_by_part_without_restoring_join_intent() {
    let mut session = MockSession::with_timeout(&[], Duration::from_millis(250));
    session.control(ConnectionCommand::Join("#late".into()));
    assert_eq!(session.wire(), ["JOIN #late"]);
    session.wait_channel("#late", ChannelState::Uncertain, true);
    session.control(ConnectionCommand::Part {
        channel: "#late".into(),
        reason: Some("changed mind".into()),
    });
    assert_eq!(session.wire(), ["PART #late :changed mind"]);
    session.send(":mock 442 test #late :Not on channel\r\n");
    session.wait_channel("#late", ChannelState::NotJoined, false);
    session.send(":test!u@h JOIN #late\r\n");
    session.sync_events();
    let lines = session.wire();
    assert_eq!(lines.len(), 1, "late JOIN must be countered: {lines:?}");
    assert!(lines[0] == "PART #late" || lines[0].starts_with("PART #late :"));
    session.assert_channel("#late", ChannelState::Parting, false);
    session.send(":test!u@h PART #late :Leaving\r\n");
    session.wait_channel("#late", ChannelState::NotJoined, false);
    session.reconnect();
    assert!(session.wire().is_empty());
}

#[test]
fn reconnect_joins_runtime_intent_once_and_excludes_parted_configuration() {
    let mut session = MockSession::start(&["#keep", "#leave"]);
    assert_eq!(session.wire(), ["JOIN #keep", "JOIN #leave"]);
    session.confirm_join("#keep");
    session.confirm_join("#leave");
    session.control(ConnectionCommand::Join("#runtime".into()));
    assert_eq!(session.wire(), ["JOIN #runtime"]);
    session.confirm_join("#runtime");
    session.control(ConnectionCommand::Part {
        channel: "#leave".into(),
        reason: None,
    });
    assert_eq!(session.wire(), ["PART #leave"]);
    session.send(":test!u@h PART #leave\r\n");
    session.wait_channel("#leave", ChannelState::NotJoined, false);
    for reconnect in [false, true] {
        if reconnect {
            session.reconnect();
            let mut lines = session.wire();
            lines.sort();
            assert_eq!(lines, ["JOIN #keep", "JOIN #runtime"]);
        }
        session.send(concat!(
            ":mock 001 test :Duplicate welcome\r\n",
            ":mock 376 test :Duplicate end of MOTD\r\n",
            ":mock 422 test :No MOTD\r\n",
            ":mock 376 test :End again\r\n",
            ":mock 422 test :Still no MOTD\r\n",
        ));
        session.sync_events();
        assert!(
            session.wire().is_empty(),
            "registration replies must not repeat JOIN"
        );
    }
    assert_eq!(session.config.servers["srv"].channels, ["#keep", "#leave"]);
}

#[test]
fn controls_accepted_across_disconnect_preserve_intent_without_replaying_chat() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.confirm_join("#seed");
    session.control(ConnectionCommand::Disconnect("pause".into()));
    session.control(ConnectionCommand::Join("#queued".into()));
    session.control(ConnectionCommand::Part {
        channel: "#seed".into(),
        reason: None,
    });
    session.outgoing(OutgoingMessage::Privmsg {
        server: "srv".into(),
        target: "Alice".into(),
        text: "old chat must not replay".into(),
    });
    session.receive_until(|event| {
        *event == IrcEvent::Connection("srv".into(), ConnectionState::Stopped)
    });
    session.receive_until(|event| matches!(event, IrcEvent::ChannelControlApplied(server, channel) if server == "srv" && channel == "#seed"));
    assert!(session.observed.contains(&IrcEvent::ChannelControlApplied(
        "srv".into(),
        "#queued".into()
    )));
    session.control(ConnectionCommand::Connect);
    if let Some(socket) = session.socket.take() {
        let _ = socket.get_ref().shutdown(Shutdown::Both);
    }
    session.accept();
    session.register();
    assert_eq!(session.wire(), ["JOIN #queued"]);
    session.assert_channel("#seed", ChannelState::NotJoined, false);
    session.assert_channel("#queued", ChannelState::Joining, true);
}

#[test]
fn console_raw_join_and_part_use_managed_state_without_console_echoes() {
    let mut session = MockSession::start(&[]);
    let console = session.app.active_buffer().unwrap().id;
    let history_before = session.app.messages_for(console).len();
    session.submit("jOiN #raw");
    assert_eq!(session.wire(), ["JOIN #raw"]);
    assert_eq!(session.app.messages_for(console).len(), history_before);
    session.sync_events();
    session.assert_channel("#raw", ChannelState::Joining, true);
    session.confirm_join("#raw");
    session.app.activate_buffer(console);
    session.submit("pArT #raw :done  for now");
    assert_eq!(session.wire(), ["PART #raw :done  for now"]);
    session.sync_events();
    session.assert_channel("#raw", ChannelState::Parting, false);
    session.send(":test!u@h PART #raw :done\r\n");
    session.wait_channel("#raw", ChannelState::NotJoined, false);
    session.reconnect();
    assert!(session.wire().is_empty());
}

#[test]
fn worker_raw_join_and_part_cannot_bypass_runtime_intent() {
    let mut session = MockSession::start(&[]);
    session.outgoing(OutgoingMessage::Raw {
        server: "srv".into(),
        line: "JOIN #direct".into(),
    });
    assert_eq!(session.wire(), ["JOIN #direct"]);
    session.sync_events();
    session.assert_channel("#direct", ChannelState::Joining, true);
    session.confirm_join("#direct");
    session.outgoing(OutgoingMessage::Raw {
        server: "srv".into(),
        line: "PART #direct :done".into(),
    });
    assert_eq!(session.wire(), ["PART #direct done"]);
    session.send(":test!u@h PART #direct :done\r\n");
    session.wait_channel("#direct", ChannelState::NotJoined, false);
    session.reconnect();
    assert!(session.wire().is_empty());
}

#[test]
fn keys_multiple_targets_and_raw_prefixes_are_rejected_without_side_effects() {
    let mut session = MockSession::start(&[]);
    let console = session.app.active_buffer().unwrap().id;
    let history_before = session.app.messages_for(console).len();
    for input in [
        "/join",
        "/join #one key",
        "/join #one,#two",
        "/join 0",
        "/join peer",
        "/part #one,#two reason",
        "JOIN #one key",
        "JOIN #one,#two",
        "JOIN 0",
        "JOIN #one :key",
        "PART #one,#two :reason",
        ":test JOIN #one",
        "@tag=value JOIN #one",
        "JOIN #one\r\nJOIN #two",
    ] {
        session.submit(input);
        assert_eq!(session.app.input(), input, "accepted {input:?}");
        assert_eq!(session.app.active_buffer().unwrap().id, console);
        assert_eq!(
            session.app.buffer_count(),
            1,
            "created buffer for {input:?}"
        );
    }
    assert_eq!(session.app.messages_for(console).len(), history_before);
    assert!(session.wire().is_empty());
    for channel in ["", "0", "peer", "#one,#two", "#one key", "#one\r\nQUIT"] {
        session.control(ConnectionCommand::Join(channel.into()));
        session.control(ConnectionCommand::Part {
            channel: channel.into(),
            reason: None,
        });
    }
    session.control(ConnectionCommand::Part {
        channel: "#one".into(),
        reason: Some("bad\r\nQUIT".into()),
    });
    assert!(session.wire().is_empty());
    let events = session.sync_events();
    assert!(!events.iter().any(|event| matches!(
        event,
        IrcEvent::Channel(..) | IrcEvent::ChannelControlApplied(..)
    )));
    assert_eq!(session.app.buffer_count(), 1);
}

#[test]
fn prioritized_part_prevents_queued_channel_chat_from_leaking_to_the_wire() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.confirm_join("#seed");
    session.control(ConnectionCommand::Part {
        channel: "#seed".into(),
        reason: None,
    });
    session.outgoing(OutgoingMessage::Privmsg {
        server: "srv".into(),
        target: "#seed".into(),
        text: "queued before the UI saw PART confirmation".into(),
    });
    assert_eq!(session.wire(), ["PART #seed"]);
    session.wait_channel("#seed", ChannelState::Parting, false);
}

#[test]
fn ui_join_and_part_are_online_only_while_existing_buffers_and_drafts_survive() {
    let mut session = MockSession::start(&["#seed"]);
    assert_eq!(session.wire(), ["JOIN #seed"]);
    session.confirm_join("#seed");
    session.control(ConnectionCommand::Disconnect("pause".into()));
    session.receive_until(|event| {
        *event == IrcEvent::Connection("srv".into(), ConnectionState::Stopped)
    });
    for input in [
        "/join #offline",
        "/part #seed",
        "JOIN #offline",
        "PART #seed",
    ] {
        session.submit(input);
        assert_eq!(session.app.input(), input);
        assert_eq!(session.app.buffer_count(), 2);
    }
    session.control(ConnectionCommand::Connect);
    session.accept();
    session.register();
    assert_eq!(session.wire(), ["JOIN #seed"]);
}
