//! Serial connection lifecycle: cancellation, bounded retries and confirmed state.

use crate::{
    config::ServerConfig,
    irc::{
        ConnectionCommand, ConnectionState, IrcEvent, OutgoingMessage, RetryPolicy,
        build_client_config,
    },
    message::ChatMessage,
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
    loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                acknowledge(&tx, &label, command.as_ref());
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
            message = outgoing.recv() => { if message.is_none() { break; } },
            _ = sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                // Never replay messages queued for a previous socket.
                while outgoing.try_recv().is_ok() {}
                state(&tx, &label, ConnectionState::Connecting);
                match session(&mut server, &label, &channels, &tx, policy.timeout, &mut outgoing, &mut control).await {
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
    channels: &[String],
    tx: &mpsc::Sender<IrcEvent>,
    limit: Duration,
    outgoing: &mut Receiver<OutgoingMessage>,
    control: &mut Receiver<ConnectionCommand>,
) -> End {
    let deadline = Instant::now() + limit;
    let connecting = Client::from_config(build_client_config(server, channels));
    tokio::pin!(connecting);
    let mut client = loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                acknowledge(tx, label, command.as_ref());
                match command {
                None => return End::Shutdown,
                Some(ConnectionCommand::Disconnect(_)) => return End::Stop,
                Some(ConnectionCommand::Reconnect) => return End::Restart,
                _ => {}
                }
            },
            message = outgoing.recv() => { if message.is_none() { return End::Shutdown; } },
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
    let mut pending: Vec<String> = channels.to_vec();
    let mut join_deadline = deadline;
    loop {
        tokio::select! {
            biased;
            command = control.recv() => {
                acknowledge(tx, label, command.as_ref());
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
                };
                if client.send(wire).is_err() { return failed(false, registered_at); }
            },
            _ = sleep_until(deadline), if registered_at.is_none() => return failed(false, None),
            _ = sleep_until(join_deadline), if registered_at.is_some() && !pending.is_empty() => {
                for channel in pending.drain(..) {
                    let _ = tx.send(IrcEvent::Channel(label.into(), channel, ConnectionState::Stopped));
                }
            },
            outgoing = outgoing.recv() => {
                let Some(message) = outgoing else { close(&client, &mut stream, "").await; return End::Shutdown; };
                if registered_at.is_none() { continue; }
                let result = match message {
                    OutgoingMessage::Privmsg { target, text, .. } => client.send_privmsg(target, text),
                    OutgoingMessage::Raw { line, .. } => {
                        let mut parts = line.split_whitespace();
                        let command = parts.next().unwrap_or_default().to_string();
                        client.send(Command::Raw(command, parts.map(str::to_string).collect()))
                    }
                };
                if result.is_err() { return failed(false, registered_at); }
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
                        while outgoing.try_recv().is_ok() {}
                        registered_at = Some(Instant::now());
                        join_deadline = Instant::now() + limit;
                        let _ = tx.send(IrcEvent::Nickname(label.into(), server.nickname.clone()));
                        let _ = tx.send(IrcEvent::Away(label.into(), false));
                        state(tx, label, ConnectionState::Connected);
                    }
                    Command::Response(Response::ERR_PASSWDMISMATCH | Response::ERR_YOUREBANNEDCREEP, _) => return failed(true, registered_at),
                    Command::NICK(nick) if own_message(&message, &server.nickname) => {
                        server.nickname.clone_from(nick);
                        let _ = tx.send(IrcEvent::Nickname(label.into(), nick.clone()));
                        tracing::debug!(target: "termirc::slash", outcome = "confirmed", "nickname updated");
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
                    Command::JOIN(channel, _, _) if own_message(&message, &server.nickname) => channel_state(tx, label, channel, ConnectionState::Connected, &mut pending),
                    Command::PART(channel, _) if own_message(&message, &server.nickname) => channel_state(tx, label, channel, ConnectionState::Stopped, &mut pending),
                    Command::KICK(channel, nick, _) if nick.eq_ignore_ascii_case(&server.nickname) => channel_state(tx, label, channel, ConnectionState::Stopped, &mut pending),
                    Command::Response(response, args) if matches!(*response as u16, 403 | 405 | 407 | 437 | 442 | 471 | 473 | 474 | 475 | 476 | 477 | 489) => {
                        if let Some(channel) = args.get(1) { channel_state(tx, label, channel, ConnectionState::Stopped, &mut pending); }
                    }
                    Command::ERROR(_) => return failed(false, registered_at),
                    _ => {}
                }
                if let Some(chat) = ChatMessage::from_proto(&message, label, channels)
                    .or_else(|| ChatMessage::console_from_proto(&message, label)) {
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

fn channel_state(
    tx: &mpsc::Sender<IrcEvent>,
    label: &str,
    channel: &str,
    state: ConnectionState,
    pending: &mut Vec<String>,
) {
    pending.retain(|name| !name.eq_ignore_ascii_case(channel));
    let _ = tx.send(IrcEvent::Channel(label.into(), channel.into(), state));
}

fn acknowledge(tx: &mpsc::Sender<IrcEvent>, label: &str, command: Option<&ConnectionCommand>) {
    if matches!(
        command,
        Some(ConnectionCommand::Disconnect(_) | ConnectionCommand::Reconnect)
    ) {
        let _ = tx.send(IrcEvent::ControlApplied(label.into()));
    }
}
