use std::{sync::mpsc, time::Duration};

use irc::proto::Command;
use tokio::time::Instant;

use crate::{
    connection::IrcEvent,
    core::{
        BufferKind, ChannelState, ChannelStatus, ConnectionCommand, MessageContent, RoutedMessage,
        ServerId, valid_channel_name,
    },
};

struct Channel {
    name: String,
    status: ChannelStatus,
    deadline: Option<Instant>,
}

pub(super) struct Channels {
    entries: Vec<Channel>,
    label: String,
    sender: mpsc::Sender<IrcEvent>,
    timeout: Duration,
}

impl Channels {
    pub(super) fn new(
        names: &[String],
        label: &str,
        sender: mpsc::Sender<IrcEvent>,
        timeout: Duration,
    ) -> Self {
        let mut channels = Self {
            entries: Vec::new(),
            label: label.into(),
            sender,
            timeout,
        };
        for name in names {
            if valid_channel_name(name) && channels.index(name).is_none() {
                channels.register(name);
            }
        }
        channels
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    fn register(&mut self, name: &str) -> usize {
        let index = self.entries.len();
        self.entries.push(Channel {
            name: name.into(),
            status: ChannelStatus {
                state: ChannelState::NotJoined,
                desired: true,
            },
            deadline: None,
        });
        index
    }

    fn publish(&self, index: usize) {
        let channel = &self.entries[index];
        let _ = self.sender.send(IrcEvent::Channel(
            self.label.clone(),
            channel.name.clone(),
            channel.status,
        ));
    }

    fn transition(&mut self, index: usize, state: ChannelState) {
        self.entries[index].status.state = state;
        self.entries[index].deadline =
            matches!(state, ChannelState::Joining | ChannelState::Parting)
                .then(|| Instant::now() + self.timeout);
        self.publish(index);
    }

    pub(super) fn names(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|channel| channel.name.clone())
            .collect()
    }

    pub(super) fn can_send(&self, name: &str) -> bool {
        self.index(name).map_or(!valid_channel_name(name), |index| {
            self.entries[index].status.state == ChannelState::Joined
        })
    }

    pub(super) fn start(&mut self) -> Vec<Command> {
        let mut commands = Vec::new();
        for index in 0..self.entries.len() {
            if self.entries[index].status.desired {
                self.transition(index, ChannelState::Joining);
                commands.push(Command::JOIN(self.entries[index].name.clone(), None, None));
            }
        }
        commands
    }

    pub(super) fn disconnected(&mut self) {
        for index in 0..self.entries.len() {
            self.transition(index, ChannelState::NotJoined);
        }
    }

    pub(super) fn request(
        &mut self,
        command: &ConnectionCommand,
        connected: bool,
    ) -> Option<Command> {
        let (name, joining, reason) = match command {
            ConnectionCommand::Join(name) => (name, true, None),
            ConnectionCommand::Part { channel, reason } => (channel, false, reason.clone()),
            _ => return None,
        };
        let index = match self.index(name) {
            Some(index) => index,
            None if joining => self.register(name),
            None => return None,
        };
        let state = self.entries[index].status.state;
        if connected
            && matches!(
                (joining, state),
                (
                    true,
                    ChannelState::Joined | ChannelState::Joining | ChannelState::Parting
                ) | (false, ChannelState::Joining | ChannelState::Parting)
            )
        {
            self.publish(index);
            return None;
        }
        self.entries[index].status.desired = joining;
        if !connected || (!joining && state == ChannelState::NotJoined) {
            self.transition(index, ChannelState::NotJoined);
            return None;
        }
        self.transition(
            index,
            if joining {
                ChannelState::Joining
            } else {
                ChannelState::Parting
            },
        );
        let name = self.entries[index].name.clone();
        Some(if joining {
            Command::JOIN(name, None, None)
        } else {
            Command::PART(name, reason)
        })
    }

    pub(super) fn joined(&mut self, name: &str) -> Option<Command> {
        if !valid_channel_name(name) {
            return None;
        }
        let index = self.index(name).unwrap_or_else(|| self.register(name));
        if self.entries[index].status.desired {
            self.transition(index, ChannelState::Joined);
            None
        } else if self.entries[index].status.state == ChannelState::Parting {
            None
        } else {
            self.transition(index, ChannelState::Parting);
            Some(Command::PART(self.entries[index].name.clone(), None))
        }
    }

    pub(super) fn left(&mut self, name: &str, kicked: bool) {
        if let Some(index) = self.index(name) {
            if !kicked {
                self.entries[index].status.desired = false;
            }
            self.transition(index, ChannelState::NotJoined);
        }
    }

    pub(super) fn error(&mut self, code: u16, name: &str) {
        let Some(index) = self.index(name) else {
            return;
        };
        if code == 442
            || (self.entries[index].status.state == ChannelState::Joining
                && matches!(
                    code,
                    403 | 405 | 407 | 437 | 471 | 473 | 474 | 475 | 476 | 477 | 489
                ))
        {
            self.transition(index, ChannelState::NotJoined);
        }
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.entries
            .iter()
            .filter_map(|channel| channel.deadline)
            .min()
    }

    pub(super) fn expire(&mut self, now: Instant) {
        for index in 0..self.entries.len() {
            if self.entries[index]
                .deadline
                .is_some_and(|deadline| deadline <= now)
            {
                let operation = if self.entries[index].status.state == ChannelState::Joining {
                    "JOIN"
                } else {
                    "PART"
                };
                self.transition(index, ChannelState::Uncertain);
                self.notice(
                    &self.entries[index].name,
                    format!("{operation} timed out; channel membership is unconfirmed"),
                );
            }
        }
    }

    pub(super) fn notice(&self, name: &str, text: String) {
        if let Some(index) = self.index(name) {
            let _ = self.sender.send(IrcEvent::Message(RoutedMessage {
                server: ServerId::new(&self.label),
                target: BufferKind::Channel(self.entries[index].name.clone()),
                content: MessageContent::console(text),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (Channels, mpsc::Receiver<IrcEvent>) {
        let (sender, receiver) = mpsc::channel();
        (
            Channels::new(
                &["#First".into(), "#FIRST".into()],
                "srv",
                sender,
                Duration::from_millis(100),
            ),
            receiver,
        )
    }

    #[test]
    fn runtime_intent_survives_sockets_but_parted_channels_do_not_rejoin() {
        let (mut channels, _) = registry();
        assert_eq!(channels.names(), vec!["#First"]);
        assert_eq!(channels.start().len(), 1);
        channels.joined("#first");
        assert!(
            channels
                .request(&ConnectionCommand::Join("#dynamic".into()), true)
                .is_some()
        );
        channels.joined("#dynamic");
        assert!(
            channels
                .request(
                    &ConnectionCommand::Part {
                        channel: "#FIRST".into(),
                        reason: None
                    },
                    true
                )
                .is_some()
        );
        channels.left("#First", false);
        channels.disconnected();
        assert_eq!(
            channels.start(),
            vec![Command::JOIN("#dynamic".into(), None, None)]
        );
    }

    #[test]
    fn stale_join_confirmation_does_not_restore_cancelled_intent() {
        let (mut channels, _) = registry();
        channels.request(
            &ConnectionCommand::Part {
                channel: "#first".into(),
                reason: None,
            },
            false,
        );
        assert_eq!(
            channels.joined("#FIRST"),
            Some(Command::PART("#First".into(), None))
        );
        assert_eq!(
            channels.entries[0].status,
            ChannelStatus {
                state: ChannelState::Parting,
                desired: false
            }
        );
        assert!(channels.joined("#first").is_none());
    }

    #[test]
    fn channel_deadlines_and_errors_are_scoped_to_the_operation() {
        let (mut channels, _) = registry();
        channels.start();
        let first_deadline = channels.deadline().unwrap();
        channels.request(&ConnectionCommand::Join("#later".into()), true);
        channels.entries[1].deadline = Some(first_deadline + Duration::from_secs(1));
        channels.expire(first_deadline);
        assert_eq!(channels.entries[0].status.state, ChannelState::Uncertain);
        assert_eq!(channels.entries[1].status.state, ChannelState::Joining);
        channels.joined("#later");
        channels.error(404, "#later");
        assert_eq!(channels.entries[1].status.state, ChannelState::Joined);
        channels.error(475, "#later");
        assert_eq!(channels.entries[1].status.state, ChannelState::Joined);
    }

    #[test]
    fn channel_send_permission_waits_for_confirmation_and_stops_on_part() {
        let (mut channels, _) = registry();
        channels.start();
        assert!(!channels.can_send("#first"));
        channels.joined("#first");
        assert!(channels.can_send("#FIRST"));
        channels.request(
            &ConnectionCommand::Part {
                channel: "#first".into(),
                reason: None,
            },
            true,
        );
        assert!(!channels.can_send("#First"));
        assert!(channels.can_send("alice"));
        assert!(!channels.can_send("#unknown"));
    }
}
