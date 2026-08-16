use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crate::message::UserMessage;

#[derive(Debug, Clone)]
pub enum QueuedMessage {
    Steer(UserMessage),
    FollowUp(UserMessage),
}

pub trait MessageSink {
    fn enqueue(&mut self, message: QueuedMessage) -> anyhow::Result<()>;
    fn is_idle(&self) -> bool;
}

#[derive(Default)]
struct QueueState {
    messages: VecDeque<QueuedMessage>,
    idle: bool,
    closed: bool,
}

#[derive(Clone, Default)]
pub struct MessageQueue(Arc<Mutex<QueueState>>);

impl MessageQueue {
    fn with_state<R>(&self, f: impl FnOnce(&mut QueueState) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(|error| error.into_inner()))
    }

    pub fn set_idle(&self, idle: bool) {
        self.with_state(|state| state.idle = idle);
    }

    pub fn len(&self) -> usize {
        self.with_state(|state| state.messages.len())
    }

    pub fn push(&self, message: QueuedMessage) -> anyhow::Result<()> {
        self.with_state(|state| {
            if state.closed {
                anyhow::bail!("session is not accepting messages");
            }
            state.messages.push_back(message);
            Ok(())
        })
    }

    pub fn close(&self) {
        self.with_state(|state| state.closed = true);
    }

    pub fn pop_steer(&self) -> Option<UserMessage> {
        self.with_state(|state| {
            let index = state
                .messages
                .iter()
                .position(|message| matches!(message, QueuedMessage::Steer(_)))?;
            match state.messages.remove(index)? {
                QueuedMessage::Steer(text) => Some(text),
                _ => unreachable!(),
            }
        })
    }

    pub fn pop_follow_up(&self) -> Option<UserMessage> {
        self.with_state(|state| {
            let index = state
                .messages
                .iter()
                .position(|message| matches!(message, QueuedMessage::FollowUp(_)))?;
            match state.messages.remove(index)? {
                QueuedMessage::FollowUp(text) => Some(text),
                _ => unreachable!(),
            }
        })
    }
}

impl MessageSink for MessageQueue {
    fn enqueue(&mut self, message: QueuedMessage) -> anyhow::Result<()> {
        self.push(message)
    }

    fn is_idle(&self) -> bool {
        self.with_state(|state| state.idle && state.messages.is_empty())
    }
}
