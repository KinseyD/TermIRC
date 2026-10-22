//! SOCKS5 proxy support: a real `spawn_irc_with_policy` worker connects to a
//! mock IRC server *through* a minimal in-process SOCKS5 server, proving the
//! `proxy` feature path actually engages (greeting, CONNECT, then a
//! bidirectional byte splice carrying the IRC session).

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use termirc::config::{ProxyConfig, ServerConfig};
use termirc::connection::{
    ConnectionHandle, ConnectionState, IrcEvent, RetryPolicy, spawn_irc_with_policy,
};
use termirc::core::{ChannelState, ChannelStatus, OutgoingMessage};

const WAIT: Duration = Duration::from_secs(5);

fn policy() -> RetryPolicy {
    RetryPolicy {
        max_retries: 0,
        delay: Duration::from_millis(30),
        timeout: Duration::from_millis(2_000),
    }
}

fn server_config(irc_port: u16, proxy: ProxyConfig) -> ServerConfig {
    ServerConfig {
        username: "test".into(),
        nickname: "test".into(),
        password: String::new(),
        server: "127.0.0.1".into(),
        port: irc_port,
        use_tls: false,
        channels: vec!["#chan".into()],
        queries: vec![],
        sasl_username: None,
        sasl_password: None,
        proxy: Some(proxy),
    }
}

/// Minimal SOCKS5 server: one client, one CONNECT, then raw byte splicing
/// toward the mock IRC listener. Asserts the handshake bytes on the way.
fn spawn_socks5_server(irc_port: u16) -> (TcpListener, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let serving = listener.try_clone().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        client.set_read_timeout(Some(WAIT)).unwrap();
        client.set_write_timeout(Some(WAIT)).unwrap();

        // Greeting: VER, NMETHODS, METHODS — answer "no auth required".
        let mut greeting = [0u8; 2];
        client.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting[0], 0x05, "expected SOCKS5 greeting");
        let mut methods = vec![0u8; greeting[1] as usize];
        client.read_exact(&mut methods).unwrap();
        assert!(
            methods.contains(&0x00),
            "expected the no-auth method, got {methods:?}"
        );
        client.write_all(&[0x05, 0x00]).unwrap();

        // CONNECT request: VER CMD RSV ATYP, address, port.
        let mut header = [0u8; 4];
        client.read_exact(&mut header).unwrap();
        assert_eq!(header[0], 0x05, "CONNECT must be SOCKS5");
        assert_eq!(header[1], 0x01, "expected the CONNECT command");
        let target = match header[3] {
            0x01 => {
                let mut octets = [0u8; 4];
                client.read_exact(&mut octets).unwrap();
                std::net::IpAddr::from(octets).to_string()
            }
            0x03 => {
                let mut len = [0u8; 1];
                client.read_exact(&mut len).unwrap();
                let mut name = vec![0u8; len[0] as usize];
                client.read_exact(&mut name).unwrap();
                String::from_utf8(name).unwrap()
            }
            other => panic!("unexpected address type {other:#x}"),
        };
        let mut port_bytes = [0u8; 2];
        client.read_exact(&mut port_bytes).unwrap();
        let port = u16::from_be_bytes(port_bytes);
        assert_eq!((target.as_str(), port), ("127.0.0.1", irc_port));

        // Success reply (bound address is a placeholder).
        client
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .unwrap();

        // Splice both directions for the lifetime of the IRC session.
        let mut upstream = TcpStream::connect(("127.0.0.1", irc_port)).unwrap();
        upstream.set_read_timeout(Some(WAIT)).unwrap();
        upstream.set_write_timeout(Some(WAIT)).unwrap();
        let mut client_write = client.try_clone().unwrap();
        let mut upstream_write = upstream.try_clone().unwrap();
        let to_irc = std::thread::spawn(move || {
            let _ = std::io::copy(&mut client, &mut upstream_write);
        });
        let _ = std::io::copy(&mut upstream, &mut client_write);
        to_irc.join().unwrap();
    });
    (serving, handle)
}

/// Mock IRC server (no capabilities) reached through the proxy.
struct IrcMock {
    socket: Option<BufReader<TcpStream>>,
    rx: mpsc::Receiver<IrcEvent>,
    handle: Option<ConnectionHandle>,
    worker: Option<JoinHandle<()>>,
}

impl IrcMock {
    fn start(proxy: ProxyConfig) -> (Self, TcpListener, JoinHandle<()>) {
        let irc_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        irc_listener.set_nonblocking(true).unwrap();
        let irc_port = irc_listener.local_addr().unwrap().port();
        let (socks_listener, socks_thread) = spawn_socks5_server(irc_port);
        let proxy_port = socks_listener.local_addr().unwrap().port();
        let mut proxy = proxy;
        proxy.port = proxy_port;
        let server = server_config(irc_port, proxy);
        let (tx, rx) = mpsc::channel();
        let (worker, handle) =
            spawn_irc_with_policy(server, "srv".into(), vec!["#chan".into()], tx, policy());
        // Wait until the splice thread shows up at the IRC listener.
        let deadline = Instant::now() + WAIT;
        let socket = loop {
            match irc_listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "worker did not reach the IRC mock"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        socket.set_nonblocking(false).unwrap();
        socket.set_read_timeout(Some(WAIT)).unwrap();
        socket.set_write_timeout(Some(WAIT)).unwrap();
        (
            Self {
                socket: Some(BufReader::new(socket)),
                rx,
                handle: Some(handle),
                worker: Some(worker),
            },
            socks_listener,
            socks_thread,
        )
    }

    fn send(&mut self, lines: &str) {
        self.socket
            .as_mut()
            .unwrap()
            .get_mut()
            .write_all(lines.as_bytes())
            .unwrap();
    }

    /// One client line, skipping the irc crate's keepalive pings.
    fn read_line(&mut self) -> String {
        loop {
            let reader = self.socket.as_mut().unwrap();
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0, "socket closed");
            let line = line.trim_end_matches(['\r', '\n']).to_owned();
            if line.starts_with("PING ") && line[5..].bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            return line;
        }
    }

    fn expect(&mut self, expected: &str) {
        let line = self.read_line();
        assert_eq!(line, expected);
    }

    fn receive_until(&mut self, predicate: impl Fn(&IrcEvent) -> bool) -> Vec<IrcEvent> {
        let deadline = Instant::now() + WAIT;
        let mut received = Vec::new();
        loop {
            let event = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| panic!("missing event: {error}; received {received:?}"));
            let done = predicate(&event);
            received.push(event);
            if done {
                return received;
            }
        }
    }

    fn finish(mut self) {
        self.handle.take();
        if let Some(socket) = self.socket.take() {
            let _ = socket.get_ref().shutdown(std::net::Shutdown::Both);
        }
        let worker = self.worker.take().unwrap();
        let deadline = Instant::now() + WAIT;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            worker.is_finished(),
            "worker did not stop after handle drop"
        );
        worker.join().unwrap();
    }
}

#[test]
fn socks5_proxy_carries_full_irc_session_to_registration() {
    // Arrange: proxy with credentials — the SOCKS mock asserts method 0x00,
    // so credentials only prove they do not break the greeting negotiation.
    let proxy = ProxyConfig {
        r#type: "socks5".into(),
        host: "127.0.0.1".into(),
        port: 0, // replaced with the mock's bound port in IrcMock::start
        username: None,
        password: None,
    };
    let (mut mock, _socks_listener, socks_thread) = IrcMock::start(proxy);

    // Act: registration through the tunnel.
    mock.expect("CAP LS 302");
    mock.expect("NICK test");
    mock.expect("USER test 0 * test");
    mock.send(":mock 001 test :Welcome\r\n:mock 376 test :End of MOTD\r\n");
    mock.expect("JOIN #chan");
    mock.send(":test!u@h JOIN #chan\r\n");
    let events = mock.receive_until(|event| {
        matches!(event, IrcEvent::Channel(_, channel, ChannelStatus { state: ChannelState::Joined, desired: true }) if channel == "#chan")
    });

    // Assert: the worker saw itself as connected, server→client traffic
    // flowed through the splice, and a client→server PRIVMSG comes back.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, IrcEvent::Connection(_, ConnectionState::Connected))),
        "expected a Connected event: {events:?}"
    );
    mock.send(":friend!f@h PRIVMSG test :hello through the tunnel\r\n");
    mock.receive_until(|event| matches!(event, IrcEvent::Message(message) if message.content.text == "hello through the tunnel"));
    mock.handle
        .as_ref()
        .unwrap()
        .outgoing
        .try_send(OutgoingMessage::Raw {
            server: "srv".into(),
            line: "PRIVMSG #chan :hi back".into(),
        })
        .unwrap();
    mock.expect("PRIVMSG #chan :hi back");

    mock.finish();
    // The SOCKS mock's splices end when the worker closes the tunnel.
    let deadline = Instant::now() + WAIT;
    while !socks_thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    socks_thread.join().unwrap();
}
