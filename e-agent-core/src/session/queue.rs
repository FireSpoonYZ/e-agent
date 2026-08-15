use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueuedMessage {
    Steer(String),
    FollowUp(String),
}

pub trait MessageSink {
    fn enqueue(&mut self, message: QueuedMessage) -> anyhow::Result<()>;
    fn is_idle(&self) -> bool;
}

#[derive(Default)]
pub struct MessageQueue {
    messages: VecDeque<QueuedMessage>,
    idle: bool,
}

impl MessageQueue {
    pub fn set_idle(&mut self, idle: bool) {
        self.idle = idle;
    }
    pub fn pop_steer(&mut self) -> Option<String> {
        let index = self
            .messages
            .iter()
            .position(|message| matches!(message, QueuedMessage::Steer(_)))?;
        match self.messages.remove(index)? {
            QueuedMessage::Steer(text) => Some(text),
            _ => unreachable!(),
        }
    }
    pub fn pop_follow_up(&mut self) -> Option<String> {
        let index = self
            .messages
            .iter()
            .position(|message| matches!(message, QueuedMessage::FollowUp(_)))?;
        match self.messages.remove(index)? {
            QueuedMessage::FollowUp(text) => Some(text),
            _ => unreachable!(),
        }
    }
}

impl MessageSink for MessageQueue {
    fn enqueue(&mut self, message: QueuedMessage) -> anyhow::Result<()> {
        self.messages.push_back(message);
        Ok(())
    }
    fn is_idle(&self) -> bool {
        self.idle && self.messages.is_empty()
    }
}
