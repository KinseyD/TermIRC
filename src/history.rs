//! Bounded in-memory history independent of frontend geometry.

use std::collections::{HashMap, VecDeque};

use crate::core::{BufferId, Message, MessageContent, MessageId};

/// IDs affected by an append, used to maintain viewport and selection anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryChange {
    pub inserted: MessageId,
    pub evicted: Vec<MessageId>,
}

#[derive(Debug)]
pub struct HistoryStore {
    buffers: HashMap<BufferId, VecDeque<Message>>,
    empty: VecDeque<Message>,
    capacity: usize,
    next_id: u64,
}

impl Default for HistoryStore {
    fn default() -> Self {
        Self::new(5_000)
    }
}

impl HistoryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffers: HashMap::new(),
            empty: VecDeque::new(),
            capacity,
            next_id: 1,
        }
    }

    /// Append to one buffer and evict its oldest messages by count alone.
    pub fn append(&mut self, buffer: BufferId, content: MessageContent) -> HistoryChange {
        let inserted = MessageId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("message identity space exhausted");
        let messages = self.buffers.entry(buffer).or_default();
        messages.push_back(Message {
            id: inserted,
            buffer,
            content,
        });
        let mut evicted = Vec::new();
        while messages.len() > self.capacity {
            evicted.push(messages.pop_front().expect("nonempty history").id);
        }
        HistoryChange { inserted, evicted }
    }

    /// Messages ordered oldest first; unknown buffers have an empty history.
    pub fn messages(&self, buffer: BufferId) -> &VecDeque<Message> {
        self.buffers.get(&buffer).unwrap_or(&self.empty)
    }
}
