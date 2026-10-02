//! Serial connection lifecycle: cancellation, bounded retries and confirmed state.

use super::channels::Channels;
use super::sasl::encode_sasl_plain;
use crate::{
    config::ServerConfig,
    connection::{ConnectionState, IrcEvent, RetryPolicy, build_client_config},
    core::{ConnectionCommand, OutgoingMessage},
    protocol::{channel_control_from_raw, decode_message, encode_outgoing, validate_control},
};
use futures_util::StreamExt;
use irc::client::{Client, ClientStream};
use irc::proto::{CapSubCommand, Command, Message, NegotiationVersion, Response, caps::Capability};
use std::{sync::mpsc, time::Duration};
use tokio::{
    sync::mpsc::Receiver,
    time::{Instant, sleep_until, timeout},
};

enum End {
    Stop,
    Restart,
    Shutdown,
    Failed {
        permanent: bool,
        healthy: bool,
        registered: bool,
    },
}

pub(crate) async fn run(
    mut server: ServerConfig,
    label: String,
    channels: Vec<String>,
    tx: mpsc::Sender<IrcEvent>,
    policy: RetryPolicy,
    mut outgoing: Receiver<OutgoingMessage>,
    mut control: Receiver<ConnectionCommand>,
) {
    let mut next = Some(Instant::now());
    let mut failures = 0u32;
    let mut channels = Channels::new(&channels, &label, tx.clone(), policy.timeout);
    loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                if command.as_ref().is_some_and(|command| validate_control(command).is_err()) {
                    tracing::debug!(target: "termirc::slash", reason = "invalid_control", "control rejected");
                    continue;
                }
                acknowledge(&tx, &label, command.as_ref());
                if let Some(command) = command.as_ref() { channels.request(command, false); }
                match command {
                None => break,
                Some(ConnectionCommand::Connect) if next.is_none() => { failures = 0; next = Some(Instant::now()); }
                Some(ConnectionCommand::Reconnect) => { failures = 0; next = Some(Instant::now()); }
                Some(ConnectionCommand::Disconnect(_)) => {
                    next = None;
                    state(&tx, &label, ConnectionState::Stopped);
                }
                _ => {}
                }
            },
            message = outgoing.recv() => {
                let Some(message) = message else { break; };
                retain_channel_intent(&message, &mut channels);
            },
            _ = sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                // Never replay messages queued for a previous socket.
                discard_chat(&mut outgoing, &mut channels);
                state(&tx, &label, ConnectionState::Connecting);
                let end = session(&mut server, &label, &mut channels, &tx, policy.timeout, &mut outgoing, &mut control).await;
                channels.disconnected();
                match end {
                    End::Shutdown => break,
                    End::Stop => { next = None; state(&tx, &label, ConnectionState::Stopped); }
                    End::Restart => { failures = 0; next = Some(Instant::now()); }
                    End::Failed { permanent, healthy, registered } => {
                        if healthy { failures = 0; }
                        let _ = tx.send(if registered {
                            IrcEvent::Status(label.clone(), "disconnected (connection closed)".into())
                        } else {
                            IrcEvent::Error(label.clone(), "connection or registration failed".into())
                        });
                        if permanent || failures >= policy.max_retries {
                            next = None;
                            state(&tx, &label, ConnectionState::Stopped);
                        } else {
                            let delay = policy.delay.saturating_mul(1u32 << failures.min(5)).min(Duration::from_secs(30));
                            failures += 1;
                            if registered { state(&tx, &label, ConnectionState::Connecting); }
                            next = Some(Instant::now() + delay);
                        }
                    }
                }
            }
        }
    }
}

fn state(tx: &mpsc::Sender<IrcEvent>, label: &str, state: ConnectionState) {
    let _ = tx.send(IrcEvent::Connection(label.into(), state));
    tracing::debug!(target: "termirc::slash", ?state, "connection state changed");
}

async fn close(client: &Client, stream: &mut ClientStream, reason: &str) {
    let _ = client.send_quit(reason);
    // Polling the stream also flushes the irc crate's outgoing queue.
    let _ = timeout(Duration::from_millis(250), async {
        while stream.next().await.is_some() {}
    })
    .await;
}

async fn session(
    server: &mut ServerConfig,
    label: &str,
    channels: &mut Channels,
    tx: &mpsc::Sender<IrcEvent>,
    limit: Duration,
    outgoing: &mut Receiver<OutgoingMessage>,
    control: &mut Receiver<ConnectionCommand>,
) -> End {
    let deadline = Instant::now() + limit;
    let connecting = Client::from_config(build_client_config(server));
    tokio::pin!(connecting);
    let mut client = loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                if command.as_ref().is_some_and(|command| validate_control(command).is_err()) {
                    tracing::debug!(target: "termirc::slash", reason = "invalid_control", "control rejected");
                    continue;
                }
                acknowledge(tx, label, command.as_ref());
                if let Some(command) = command.as_ref() { channels.request(command, false); }
                match command {
                None => return End::Shutdown,
                Some(ConnectionCommand::Disconnect(_)) => return End::Stop,
                Some(ConnectionCommand::Reconnect) => return End::Restart,
                _ => {}
                }
            },
            message = outgoing.recv() => {
                let Some(message) = message else { return End::Shutdown; };
                retain_channel_intent(&message, channels);
            },
            _ = sleep_until(deadline) => return failed(false, None),
            result = &mut connecting => match result {
                Ok(client) => break client,
                Err(_) => return failed(false, None),
            }
        }
    };
    // IRCv3 negotiation is driven manually (the crate's helpers cover single
    // lines, not the exchange): CAP LS goes first so negotiation overlaps the
    // classic PASS/NICK/USER triplet, and CAP END waits for the exchange to
    // resolve. Servers without CAP just send 001 and the exchange abandons
    // itself with zero extra traffic.
    let mut negotiation = Negotiation::new(server);
    if client.send_cap_ls(NegotiationVersion::V302).is_err() {
        return failed(false, None);
    }
    let registration = [
        (!server.password.is_empty()).then(|| Command::PASS(server.password.clone())),
        Some(Command::NICK(server.nickname.clone())),
        Some(Command::USER(
            server.username.clone(),
            "0".into(),
            server.nickname.clone(),
        )),
    ]
    .into_iter()
    .flatten();
    for wire in registration {
        if client.send(wire).is_err() {
            return failed(false, None);
        }
    }
    let Ok(mut stream) = client.stream() else {
        return failed(false, None);
    };
    let mut registered_at = None;
    let mut away = false;
    loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                if command.as_ref().is_some_and(|command| validate_control(command).is_err()) {
                    tracing::debug!(target: "termirc::slash", reason = "invalid_control", "control rejected");
                    continue;
                }
                acknowledge(tx, label, command.as_ref());
                if let Some(command @ (ConnectionCommand::Join(_) | ConnectionCommand::Part { .. })) = command.as_ref() {
                    if channels.request(command, registered_at.is_some()).is_some_and(|wire| client.send(wire).is_err()) {
                        return failed(false, registered_at);
                    }
                    continue;
                }
                let wire = match command {
                    None => { close(&client, &mut stream, "").await; return End::Shutdown; }
                    Some(ConnectionCommand::Disconnect(reason)) => { close(&client, &mut stream, &reason).await; return End::Stop; }
                    Some(ConnectionCommand::Reconnect) => { close(&client, &mut stream, "Reconnecting").await; return End::Restart; }
                    Some(ConnectionCommand::Connect) => continue,
                    Some(_) if registered_at.is_none() => continue,
                    Some(ConnectionCommand::Nick(nick)) => Command::NICK(nick),
                    Some(ConnectionCommand::Away(reason)) => Command::AWAY(match reason {
                        Some(reason) => Some(reason),
                        None if away => None,
                        None => Some("Away".into()),
                    }),
                    Some(ConnectionCommand::Back) => Command::AWAY(None),
                    Some(ConnectionCommand::Join(_) | ConnectionCommand::Part { .. }) => unreachable!(),
                };
                if client.send(wire).is_err() { return failed(false, registered_at); }
            },
            _ = sleep_until(deadline), if registered_at.is_none() => return failed(false, None),
            _ = sleep_until(channels.deadline().unwrap_or_else(Instant::now)), if registered_at.is_some() && channels.deadline().is_some() => {
                channels.expire(Instant::now());
            },
            outgoing = outgoing.recv() => {
                let Some(message) = outgoing else { close(&client, &mut stream, "").await; return End::Shutdown; };
                if registered_at.is_none() {
                    retain_channel_intent(&message, channels);
                    continue;
                }
                let wire = match encode_outgoing(&message) {
                    Ok(wire) => wire,
                    Err(error) => {
                        tracing::debug!(target: "termirc::slash", %error, "outgoing message rejected");
                        continue;
                    }
                };
                if let Some(command) = raw_channel_control(&message) {
                    if channels.request(&command, true).is_some_and(|wire| client.send(wire).is_err()) {
                        return failed(false, registered_at);
                    }
                    continue;
                }
                if matches!(&message, OutgoingMessage::Privmsg { target, .. } if !channels.can_send(target)) {
                    continue;
                }
                if client.send(wire).is_err() { return failed(false, registered_at); }
            },
            message = stream.next() => {
                let message = match message {
                    Some(Ok(message)) => message,
                    // The crate consumes 432/433 before yielding them. A failed
                    // nickname change after registration must not kill the socket.
                    Some(Err(irc::error::Error::NoUsableNick)) if registered_at.is_some() => {
                        tracing::debug!(target: "termirc::slash", reason = "nickname_rejected", "identity change failed");
                        continue;
                    }
                    Some(Err(irc::error::Error::NoUsableNick)) => return failed(true, None),
                    _ => return failed(false, registered_at),
                };
                match &message.command {
                    Command::CAP(_, sub, third, fourth) if !negotiation.finished() => {
                        match negotiation.on_cap(*sub, third.as_deref(), fourth.as_deref()) {
                            NegotiationAction::Ignore => {}
                            NegotiationAction::Send(lines) => {
                                for wire in lines {
                                    if client.send(wire).is_err() {
                                        return failed(false, registered_at);
                                    }
                                }
                            }
                            NegotiationAction::Fatal(reason) => {
                                let _ = tx.send(IrcEvent::Error(label.into(), reason.into()));
                                return failed(true, None);
                            }
                        }
                    }
                    Command::AUTHENTICATE(data) if negotiation.authenticating() => {
                        if let NegotiationAction::Send(lines) = negotiation.on_challenge(data) {
                            for wire in lines {
                                if client.send(wire).is_err() {
                                    return failed(false, registered_at);
                                }
                            }
                        }
                    }
                    Command::Response(
                        Response::RPL_SASLSUCCESS | Response::RPL_LOGGEDIN,
                        _,
                    ) if negotiation.authenticating() => {
                        if let NegotiationAction::Send(lines) = negotiation.on_sasl_success() {
                            for wire in lines {
                                if client.send(wire).is_err() {
                                    return failed(false, registered_at);
                                }
                            }
                        }
                    }
                    Command::Response(
                        Response::ERR_SASLFAIL
                        | Response::ERR_NICKLOCKED
                        | Response::ERR_SASLTOOLONG,
                        _,
                    ) if negotiation.authenticating() => {
                        let _ = tx.send(IrcEvent::Error(
                            label.into(),
                            "sasl authentication failed".into(),
                        ));
                        return failed(true, None);
                    }
                    Command::Response(Response::ERR_UNKNOWNCOMMAND, _)
                        if !negotiation.finished() =>
                    {
                        negotiation.abandon();
                    }
                    Command::Response(Response::RPL_WELCOME, args) if registered_at.is_none() => {
                        negotiation.abandon();
                        if let Some(nick) = args.first() { server.nickname.clone_from(nick); }
                        discard_chat(outgoing, channels);
                        registered_at = Some(Instant::now());
                        let _ = tx.send(IrcEvent::Nickname(label.into(), server.nickname.clone()));
                        let _ = tx.send(IrcEvent::Away(label.into(), false));
                        state(tx, label, ConnectionState::Connected);
                        for wire in channels.start() {
                            if client.send(wire).is_err() { return failed(false, registered_at); }
                        }
                    }
                    Command::Response(Response::ERR_PASSWDMISMATCH | Response::ERR_YOUREBANNEDCREEP, _) => {
                        if let Some(message) = decode_message(&message, label, &channels.names(), &server.nickname) {
                            let _ = tx.send(IrcEvent::Message(message));
                        }
                        return failed(true, registered_at);
                    },
                    Command::NICK(nick) if own_message(&message, &server.nickname) => {
                        server.nickname.clone_from(nick);
                        let _ = tx.send(IrcEvent::Nickname(label.into(), nick.clone()));
                        tracing::debug!(target: "termirc::slash", outcome = "confirmed", "nickname updated");
                    }
                    Command::NICK(nick) => {
                        if let Some(previous) = message.source_nickname() {
                            let _ = tx.send(IrcEvent::PeerNickname(label.into(), previous.into(), nick.clone()));
                        }
                    }
                    Command::Response(Response::RPL_NOWAWAY, _) => {
                        away = true;
                        let _ = tx.send(IrcEvent::Away(label.into(), true));
                        tracing::debug!(target: "termirc::slash", outcome = "confirmed", away, "away state updated");
                        continue;
                    }
                    Command::Response(Response::RPL_UNAWAY, _) => {
                        away = false;
                        let _ = tx.send(IrcEvent::Away(label.into(), false));
                        tracing::debug!(target: "termirc::slash", outcome = "confirmed", away, "away state updated");
                        continue;
                    }
                    Command::JOIN(channel, _, _) if own_message(&message, &server.nickname) => {
                        if channels.joined(channel).is_some_and(|wire| client.send(wire).is_err()) {
                            return failed(false, registered_at);
                        }
                    },
                    Command::PART(channel, _) if own_message(&message, &server.nickname) => channels.left(channel, false),
                    Command::KICK(channel, nick, reason) if nick.eq_ignore_ascii_case(&server.nickname) => {
                        channels.left(channel, true);
                        channels.notice(channel, format!("Kicked from {channel}: {}", reason.as_deref().unwrap_or("no reason given")));
                    },
                    Command::Response(response, args) => {
                        if let Some(channel) = args.get(1) { channels.error(*response as u16, channel); }
                    },
                    Command::Raw(code, args) => {
                        if let (Ok(code), Some(channel)) = (code.parse::<u16>(), args.get(1)) { channels.error(code, channel); }
                    }
                    Command::ERROR(_) => return failed(false, registered_at),
                    _ => {}
                }
                if let Some(chat) = decode_message(&message, label, &channels.names(), &server.nickname) {
                    let _ = tx.send(IrcEvent::Message(chat));
                }
            }
        }
    }
}

/// What the registration CAP exchange wants the session loop to do next.
enum NegotiationAction {
    /// Not negotiation traffic; continue with normal handling.
    Ignore,
    /// Send these lines and continue.
    Send(Vec<Command>),
    /// Registration cannot proceed authenticated; stop without retrying.
    Fatal(&'static str),
}

/// One step of the registration-time IRCv3 CAP exchange.
#[derive(Debug)]
enum CapPhase {
    /// Accumulating `CAP LS` lines; IRCv3.2 servers may split the list.
    Ls(Vec<String>),
    /// Waiting for `CAP ACK`; the flag records whether `sasl` was requested.
    Ack(bool),
    /// Waiting for the SASL round-trip to complete.
    Authenticating,
    /// Exchange closed (nothing requested, or `CAP END` sent).
    Complete,
    /// Server proceeded without CAP; no further negotiation traffic.
    Abandoned,
}

struct Negotiation {
    phase: CapPhase,
    /// Configured SASL PLAIN credentials; `None` skips SASL entirely.
    sasl: Option<(String, String)>,
}

fn cap_end() -> Command {
    Command::CAP(None, CapSubCommand::END, None, None)
}

fn cap_request(wanted: &[Capability]) -> Command {
    let list = wanted
        .iter()
        .map(|capability| capability.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    Command::CAP(None, CapSubCommand::REQ, None, Some(list))
}

/// IRCv3.2 advertises values as `name=value`; names alone are compared.
fn offers(tokens: &[String], name: &str) -> bool {
    tokens.iter().any(|token| {
        token
            .split('=')
            .next()
            .is_some_and(|capability| capability.eq_ignore_ascii_case(name))
    })
}

impl Negotiation {
    fn new(server: &ServerConfig) -> Self {
        Self {
            phase: CapPhase::Ls(Vec::new()),
            sasl: server
                .sasl_username
                .as_ref()
                .zip(server.sasl_password.as_ref())
                .map(|(username, password)| (username.clone(), password.clone())),
        }
    }

    fn finished(&self) -> bool {
        matches!(self.phase, CapPhase::Complete | CapPhase::Abandoned)
    }

    fn authenticating(&self) -> bool {
        matches!(self.phase, CapPhase::Authenticating)
    }

    /// A server that rejects CAP wholesale or registers us mid-exchange
    /// abandons it: no `CAP END`, keeping post-registration traffic clean.
    fn abandon(&mut self) {
        if !self.finished() {
            self.phase = CapPhase::Abandoned;
        }
    }

    fn on_cap(
        &mut self,
        sub: CapSubCommand,
        third: Option<&str>,
        fourth: Option<&str>,
    ) -> NegotiationAction {
        match sub {
            CapSubCommand::LS => {
                let CapPhase::Ls(advertised) = &mut self.phase else {
                    return NegotiationAction::Ignore;
                };
                // irc-proto parks the list in the fourth slot when a 302
                // version token precedes it, and in the third otherwise;
                // a continuation line marks itself with a lone "*".
                let (tokens, done) = match (third, fourth) {
                    (Some("*"), Some(list)) => (list, false),
                    (_, Some(list)) => (list, true),
                    (Some(list), None) => (list, true),
                    (None, None) => ("", true),
                };
                advertised.extend(tokens.split_whitespace().map(str::to_owned));
                if !done {
                    return NegotiationAction::Ignore;
                }
                // Configured SASL must be negotiable: never silently
                // register unauthenticated when credentials are configured.
                if self.sasl.is_some() && !offers(advertised, "sasl") {
                    return NegotiationAction::Fatal("sasl unsupported by server");
                }
                let mut wanted = Vec::new();
                if offers(advertised, "server-time") {
                    wanted.push(Capability::ServerTime);
                }
                if offers(advertised, "echo-message") {
                    wanted.push(Capability::EchoMessage);
                }
                if self.sasl.is_some() {
                    wanted.push(Capability::Sasl);
                }
                if wanted.is_empty() {
                    self.phase = CapPhase::Complete;
                    return NegotiationAction::Send(vec![cap_end()]);
                }
                let sasl = self.sasl.is_some();
                self.phase = CapPhase::Ack(sasl);
                NegotiationAction::Send(vec![cap_request(&wanted)])
            }
            CapSubCommand::ACK => {
                let CapPhase::Ack(sasl_requested) = self.phase else {
                    return NegotiationAction::Ignore;
                };
                let list = fourth.or(third).unwrap_or("");
                let acked: Vec<String> = list.split_whitespace().map(str::to_owned).collect();
                if sasl_requested && !offers(&acked, "sasl") {
                    return NegotiationAction::Fatal("sasl unsupported by server");
                }
                if sasl_requested {
                    self.phase = CapPhase::Authenticating;
                    NegotiationAction::Send(vec![Command::AUTHENTICATE("PLAIN".into())])
                } else {
                    self.phase = CapPhase::Complete;
                    NegotiationAction::Send(vec![cap_end()])
                }
            }
            CapSubCommand::NAK => {
                let CapPhase::Ack(sasl_requested) = self.phase else {
                    return NegotiationAction::Ignore;
                };
                if sasl_requested {
                    return NegotiationAction::Fatal("sasl unsupported by server");
                }
                self.phase = CapPhase::Complete;
                NegotiationAction::Send(vec![cap_end()])
            }
            CapSubCommand::LIST
            | CapSubCommand::NEW
            | CapSubCommand::DEL
            | CapSubCommand::REQ
            | CapSubCommand::END => NegotiationAction::Ignore,
        }
    }

    /// `AUTHENTICATE +` challenges us to send the encoded PLAIN credentials.
    fn on_challenge(&mut self, data: &str) -> NegotiationAction {
        if !self.authenticating() || data != "+" {
            return NegotiationAction::Ignore;
        }
        let Some((username, password)) = &self.sasl else {
            return NegotiationAction::Ignore;
        };
        NegotiationAction::Send(vec![Command::AUTHENTICATE(encode_sasl_plain(
            username, password,
        ))])
    }

    /// 900/903 close a successful SASL exchange; `CAP END` finishes it.
    fn on_sasl_success(&mut self) -> NegotiationAction {
        if !self.authenticating() {
            return NegotiationAction::Ignore;
        }
        self.phase = CapPhase::Complete;
        NegotiationAction::Send(vec![cap_end()])
    }
}

fn failed(permanent: bool, registered_at: Option<Instant>) -> End {
    End::Failed {
        permanent,
        registered: registered_at.is_some(),
        healthy: registered_at.is_some_and(|start| start.elapsed() >= Duration::from_secs(30)),
    }
}

fn own_message(message: &Message, nickname: &str) -> bool {
    message
        .source_nickname()
        .is_some_and(|nick| nick.eq_ignore_ascii_case(nickname))
}

fn retain_channel_intent(message: &OutgoingMessage, channels: &mut Channels) {
    if let Some(command) = raw_channel_control(message) {
        channels.request(&command, false);
    }
}

fn raw_channel_control(message: &OutgoingMessage) -> Option<ConnectionCommand> {
    match message {
        OutgoingMessage::Raw { line, .. } => channel_control_from_raw(line).ok().flatten(),
        _ => None,
    }
}

fn discard_chat(outgoing: &mut Receiver<OutgoingMessage>, channels: &mut Channels) {
    while let Ok(message) = outgoing.try_recv() {
        retain_channel_intent(&message, channels);
    }
}

fn acknowledge(tx: &mpsc::Sender<IrcEvent>, label: &str, command: Option<&ConnectionCommand>) {
    if let Some(ConnectionCommand::Join(channel) | ConnectionCommand::Part { channel, .. }) =
        command
    {
        let _ = tx.send(IrcEvent::ChannelControlApplied(
            label.into(),
            channel.clone(),
        ));
    }
    if matches!(
        command,
        Some(ConnectionCommand::Disconnect(_) | ConnectionCommand::Reconnect)
    ) {
        let _ = tx.send(IrcEvent::ControlApplied(label.into()));
    }
}
