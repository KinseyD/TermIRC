use super::*;

impl Session {
    pub(super) fn clear_self_echoes(&mut self, server: &ServerId) {
        self.pending_self_echoes.retain(|id, _| {
            self.buffers
                .iter()
                .any(|buffer| buffer.id == *id && &buffer.server != server)
        });
    }

    pub(super) fn consume_self_echo(&mut self, message: &RoutedMessage) -> bool {
        if message.content.direction != Direction::Incoming
            || !matches!(&message.target, BufferKind::Query(nickname) if nickname.eq_ignore_ascii_case(&message.content.nick))
        {
            return false;
        }
        let Some(buffer) = self
            .buffers
            .iter()
            .find(|buffer| buffer.server == message.server && buffer.kind.matches(&message.target))
        else {
            return false;
        };
        let id = buffer.id;
        let Some(pending) = self.pending_self_echoes.get_mut(&id) else {
            return false;
        };
        let matching = self
            .history
            .messages(id)
            .iter()
            .find(|candidate| {
                pending.contains(&candidate.id)
                    && candidate.nick.eq_ignore_ascii_case(&message.content.nick)
                    && candidate.text == message.content.text
                    && candidate.kind == message.content.kind
            })
            .map(|candidate| candidate.id);
        matching.is_some_and(|message_id| pending.remove(&message_id))
    }

    pub(super) fn rename_query(&mut self, server: &str, previous: &str, nickname: &str) {
        let server = ServerId::new(server);
        let Some(index) = self.buffers.iter().position(|buffer| {
            buffer.server == server
                && !buffer.send_blocked
                && buffer.kind.matches(&BufferKind::Query(previous.into()))
        }) else {
            return;
        };
        let id = self.buffers[index].id;
        let collision = self.buffers.iter().any(|buffer| {
            buffer.server == server
                && buffer.id != id
                && buffer.kind.matches(&BufferKind::Query(nickname.into()))
        });
        if collision {
            self.buffers[index].send_blocked = true;
            self.buffers[index].unread = true;
            self.history.append(id, MessageContent::console(format!(
                "{previous} is now {nickname}; existing conversations kept. Use /query {nickname}"
            )));
        } else {
            self.buffers[index].kind = BufferKind::Query(nickname.into());
        }
    }

    pub fn buffer(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.iter().find(|buffer| buffer.id == id)
    }

    pub fn select_buffer_id(&mut self, id: BufferId) {
        if self.buffer(id).is_some_and(|buffer| !buffer.hidden) {
            self.active = Some(id);
        }
    }

    pub fn open_query(&mut self, server: &str, nickname: &str) -> BufferId {
        let id = self.open_buffer(server, BufferKind::Query(nickname.into()));
        let buffer = self
            .buffers
            .iter_mut()
            .find(|buffer| buffer.id == id)
            .unwrap();
        buffer.hidden = false;
        buffer.send_blocked = false;
        id
    }

    pub fn close_query(&mut self, id: BufferId) -> Option<BufferId> {
        let buffer = self.buffers.iter_mut().find(|buffer| buffer.id == id)?;
        if !matches!(buffer.kind, BufferKind::Query(_)) {
            return None;
        }
        buffer.hidden = true;
        let server = buffer.server_label.clone();
        Some(self.open_server(&server))
    }

    pub fn mark_read(&mut self, id: BufferId) {
        if let Some(buffer) = self.buffers.iter_mut().find(|buffer| buffer.id == id) {
            buffer.unread = false;
        }
    }

    pub(super) fn clear_source_draft(&mut self, source: Option<BufferId>) {
        if let Some(buffer) = self
            .buffers
            .iter_mut()
            .find(|buffer| Some(buffer.id) == source)
        {
            buffer.draft.clear();
        } else if source.is_none() {
            self.welcome_draft.clear();
        }
    }

    pub(super) fn query_message_destination(&mut self, message: &RoutedMessage) -> BufferId {
        let existing = self.buffers.iter().position(|buffer| {
            buffer.server == message.server && buffer.kind.matches(&message.target)
        });
        let chat = matches!(
            message.content.kind,
            MessageKind::Chat | MessageKind::Action
        );
        if let Some(index) = existing {
            let buffer = &mut self.buffers[index];
            if chat {
                buffer.hidden = false;
                buffer.unread |= message.content.direction == Direction::Incoming;
            }
            if !buffer.hidden {
                return buffer.id;
            }
        }
        let server = self
            .servers
            .get(&message.server)
            .map_or_else(|| message.server.to_string(), |state| state.label.clone());
        if chat {
            let id = self.open_buffer(&server, message.target.clone());
            let buffer = self
                .buffers
                .iter_mut()
                .find(|buffer| buffer.id == id)
                .unwrap();
            buffer.unread = message.content.direction == Direction::Incoming;
            id
        } else {
            self.open_server(&server)
        }
    }

    pub(super) fn route_query_error(&self, message: &mut RoutedMessage) {
        if message.target == BufferKind::Server
            && let MessageKind::Error {
                code: 401,
                target: Some(nickname),
                ..
            } = &message.content.kind
            && let Some(buffer) = self.buffers.iter().find(|buffer| {
                buffer.server == message.server
                    && !buffer.hidden
                    && buffer.kind.matches(&BufferKind::Query(nickname.clone()))
            })
        {
            message.target = buffer.kind.clone();
        }
    }
}
