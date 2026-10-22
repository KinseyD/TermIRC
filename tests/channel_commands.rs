use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use termirc::application::submit_composer;
use termirc::config::{Config, ServerConfig};
use termirc::connection::{ConnectionHandle, IrcEvent, RetryPolicy, spawn_irc_with_policy};
use termirc::core::{BufferId, MessageKind, RoutedMessage};
use termirc::tui::App;

const WAIT: Duration = Duration::from_secs(3);
struct MockSession {
    socket: TcpStream,
    reader: BufReader<TcpStream>,
    events: mpsc::Receiver<IrcEvent>,
    handle: Option<ConnectionHandle>,
    worker: Option<JoinHandle<()>>,
}

impl MockSession {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = ServerConfig {
            username: "test".into(),
            nickname: "requested".into(),
            password: String::new(),
            server: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            use_tls: false,
            channels: vec!["#osu".into()],
            queries: vec![],
            sasl_username: None,
            sasl_password: None,
            proxy: None,
        };
        let (sender, events) = mpsc::channel();
        let (worker, handle) = spawn_irc_with_policy(
            server,
            "srv".into(),
            vec!["#osu".into()],
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
            socket,
            reader,
            events,
            handle: Some(handle),
            worker: Some(worker),
        };
        while !session.read_line().starts_with("USER ") {}
        session.send(":mock 001 assigned :Welcome\r\n");
        assert_eq!(session.read_line(), "JOIN #osu\r\n");
        session.send(":assigned!u@h JOIN :#osu\r\n");
        session
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
        .collect()
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

struct Wired {
    connections: std::collections::HashMap<String, ConnectionHandle>,
    config: Config,
    console: BufferId,
    channel: BufferId,
}

/// A connected app whose #osu channel buffer exists, is joined, and active.
fn connected_app(transport: &mut MockSession) -> (App, Wired) {
    let mut app = App::new(40, 5);
    let console = app.session.open_server("srv");
    // Feed the connection transition before opening the channel buffer so
    // the "Connected to srv" note lands in the console only.
    for event in transport.receive_until(|event| matches!(event, IrcEvent::Channel(..))) {
        if !matches!(event, IrcEvent::Channel(..)) {
            app.session.handle_event(event);
        }
    }
    let channel = app.session.open_channel("srv", "#osu");
    app.activate_buffer(channel);
    app.session.handle_event(IrcEvent::Channel(
        "srv".into(),
        "#osu".into(),
        termirc::core::ChannelStatus {
            state: termirc::core::ChannelState::Joined,
            desired: true,
        },
    ));
    let handle = transport.handle.as_ref().unwrap();
    let connections = std::collections::HashMap::from([(
        "srv".into(),
        ConnectionHandle {
            outgoing: handle.outgoing.clone(),
            control: handle.control.clone(),
        },
    )]);
    (
        app,
        Wired {
            connections,
            config: Config {
                servers: Default::default(),
                history: Default::default(),
                theme: Default::default(),
            },
            console,
            channel,
        },
    )
}

fn submit(app: &mut App, wired: &Wired, input: &str) {
    app.restore_input(input.into());
    let effect = submit_composer(
        &mut app.session,
        &wired.config,
        &wired.connections,
        &mut String::new(),
    );
    app.apply_submission_effect(effect);
}

#[test]
fn topic_and_names_round_trip_into_the_channel_buffer_and_member_cache() {
    let mut transport = MockSession::start();
    let (mut app, wired) = connected_app(&mut transport);

    submit(&mut app, &wired, "/topic");
    assert_eq!(transport.read_line(), "TOPIC #osu\r\n");
    submit(&mut app, &wired, "/names");
    assert_eq!(transport.read_line(), "NAMES #osu\r\n");

    transport.send(concat!(
        ":mock 332 assigned #osu :Welcome to #osu\r\n",
        ":mock 353 assigned = #osu :assigned @op +voice\r\n",
        ":mock 366 assigned #osu :End of /NAMES list\r\n",
    ));
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }

    let history = app.messages_for(wired.channel);
    assert_eq!(history.len(), 3, "topic, names, end-of-names");
    assert_eq!(history[0].text, "#osu Welcome to #osu");
    assert_eq!(history[0].kind, MessageKind::Info { code: 332 });
    assert_eq!(history[1].text, "assigned @op +voice");
    assert_eq!(history[1].kind, MessageKind::Info { code: 353 });
    assert_eq!(
        app.session.channel_members(wired.channel),
        [
            "assigned".to_string(),
            "op".to_string(),
            "voice".to_string()
        ]
    );
}

#[test]
fn whois_replies_and_errors_land_in_the_issuing_buffer() {
    let mut transport = MockSession::start();
    let (mut app, wired) = connected_app(&mut transport);

    submit(&mut app, &wired, "/whois ghost");
    assert_eq!(transport.read_line(), "WHOIS ghost\r\n");
    transport.send(concat!(
        ":mock 311 assigned ghost ~g host * :a spooky user\r\n",
        ":mock 318 assigned ghost :End of /WHOIS list\r\n",
    ));
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }
    let history = app.messages_for(wired.channel);
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].text, "ghost ~g host * a spooky user");
    assert_eq!(history[0].kind, MessageKind::Info { code: 311 });

    // A second /whois answered by 401 follows the same issuing buffer.
    submit(&mut app, &wired, "/whois nobody");
    assert_eq!(transport.read_line(), "WHOIS nobody\r\n");
    transport.send(":mock 401 assigned nobody :No such nick/channel\r\n");
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }
    let history = app.messages_for(wired.channel);
    assert_eq!(history.len(), 3);
    assert!(matches!(
        history[2].kind,
        MessageKind::Error { code: 401, .. }
    ));
}

#[test]
fn bare_channel_commands_from_the_console_keep_the_draft_and_never_reach_the_wire() {
    let mut transport = MockSession::start();
    let (mut app, wired) = connected_app(&mut transport);
    // The server console is not a channel: bare commands cannot resolve.
    app.activate_buffer(wired.console);

    for input in ["/topic", "/names", "/mode"] {
        submit(&mut app, &wired, input);
        // Rejected commands keep the draft for editing.
        assert_eq!(app.session.input(), input);
    }
    // Nothing was queued: the next outgoing line is the explicit /mode.
    submit(&mut app, &wired, "/mode #osu");
    assert_eq!(transport.read_line().trim_end(), "MODE #osu");
}

#[test]
fn confirmed_part_clears_the_member_cache() {
    let mut transport = MockSession::start();
    let (mut app, wired) = connected_app(&mut transport);

    transport.send(":mock 353 assigned = #osu :assigned @op\r\n");
    for message in transport.messages_until_done() {
        app.session.handle_event(IrcEvent::Message(message));
    }
    assert_eq!(
        app.session.channel_members(wired.channel),
        ["assigned".to_string(), "op".to_string()]
    );

    app.session
        .handle_event(IrcEvent::ChannelControlApplied("srv".into(), "#osu".into()));
    transport.send(":assigned!u@h PART #osu :bye\r\n");
    transport.receive_until(|event| {
        matches!(
            &event,
            IrcEvent::Channel(server, channel, status) if server == "srv"
                && channel == "#osu"
                && !status.desired
        )
    });
    app.session.handle_event(IrcEvent::Channel(
        "srv".into(),
        "#osu".into(),
        termirc::core::ChannelStatus {
            state: termirc::core::ChannelState::NotJoined,
            desired: false,
        },
    ));
    assert!(app.session.channel_members(wired.channel).is_empty());
    assert!(matches!(
        app.session
            .buffer(wired.channel)
            .unwrap()
            .channel_status
            .state,
        termirc::core::ChannelState::NotJoined
    ));
}
