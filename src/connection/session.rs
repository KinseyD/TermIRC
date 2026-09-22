//! Serial connection lifecycle: cancellation, bounded retries and confirmed state.

use super::channels::Channels;
use crate::{
    config::ServerConfig,
    connection::{ConnectionState, IrcEvent, RetryPolicy, build_client_config},
    core::{ConnectionCommand, OutgoingMessage},
    protocol::{channel_control_from_raw, decode_message, encode_outgoing, validate_control},
};
use futures_util::StreamExt;
use irc::client::{Client, ClientStream};
use irc::proto::{Command, Message, Response};
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
    if client.identify().is_err() {
        return failed(false, None);
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
                    Command::Response(Response::RPL_WELCOME, args) if registered_at.is_none() => {
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
