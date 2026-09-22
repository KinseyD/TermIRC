use super::*;

impl Session {
    pub(super) fn channel_index(&self, server: &str, channel: &str) -> Option<usize> {
        let server = ServerId::new(server);
        self.buffers.iter().position(|buffer| {
            buffer.server == server
                && buffer
                    .kind
                    .channel()
                    .is_some_and(|name| name.eq_ignore_ascii_case(channel))
        })
    }

    pub fn channel_status(&self, server: &str, channel: &str) -> ChannelStatus {
        self.channel_index(server, channel)
            .map_or_else(ChannelStatus::default, |index| {
                self.buffers[index].channel_status
            })
    }

    pub(super) fn join_channel(
        &mut self,
        server: &str,
        channel: &str,
        handle: &ConnectionHandle,
    ) -> Result<SubmissionEffect, &'static str> {
        let index = self.channel_index(server, channel);
        let name = index
            .and_then(|index| self.buffers[index].kind.channel())
            .unwrap_or(channel);
        let command = ConnectionCommand::Join(name.to_owned());
        validate_control(&command).map_err(|_| "invalid_control")?;
        if let Some(index) = index {
            match self.buffers[index].channel_status.state {
                ChannelState::Joined | ChannelState::Joining => {
                    self.buffers[index].hidden = false;
                    return Ok(SubmissionEffect::Activate(self.buffers[index].id));
                }
                ChannelState::Parting => return Err("channel_busy"),
                ChannelState::NotJoined | ChannelState::Uncertain => {}
            }
        }
        handle
            .control
            .try_send(command)
            .map_err(|_| "disconnected_or_busy")?;
        let id = self.open_channel(server, channel);
        let buffer = self
            .buffers
            .iter_mut()
            .find(|buffer| buffer.id == id)
            .unwrap();
        buffer.channel_status = ChannelStatus {
            state: ChannelState::Joining,
            desired: true,
        };
        buffer.pending_channel_changes += 1;
        buffer.hidden = false;
        Ok(SubmissionEffect::Activate(id))
    }

    pub(super) fn part_channel(
        &mut self,
        server: &str,
        channel: &str,
        reason: Option<String>,
        handle: &ConnectionHandle,
    ) -> Result<SubmissionEffect, &'static str> {
        let index = self
            .channel_index(server, channel)
            .ok_or("unknown_channel")?;
        let buffer = &mut self.buffers[index];
        let command = ConnectionCommand::Part {
            channel: buffer.kind.channel().unwrap().to_owned(),
            reason,
        };
        validate_control(&command).map_err(|_| "invalid_control")?;
        match buffer.channel_status.state {
            ChannelState::Joining => return Err("channel_busy"),
            ChannelState::Parting => return Ok(SubmissionEffect::None),
            ChannelState::NotJoined if !buffer.channel_status.desired => {
                return Ok(SubmissionEffect::None);
            }
            ChannelState::NotJoined | ChannelState::Joined | ChannelState::Uncertain => {}
        }
        handle
            .control
            .try_send(command)
            .map_err(|_| "disconnected_or_busy")?;
        buffer.channel_status.desired = false;
        if buffer.channel_status.state != ChannelState::NotJoined {
            buffer.channel_status.state = ChannelState::Parting;
        }
        buffer.pending_channel_changes += 1;
        Ok(SubmissionEffect::None)
    }
}
