use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::message::{Message, MessageContent, Usage};

pub const EVENT_BUS_CAPACITY: usize = 16_384;
pub type EventReceiver = broadcast::Receiver<AgentEvent>;

#[derive(Debug, Clone)]
pub struct EventBus(broadcast::Sender<AgentEvent>);

impl Default for EventBus {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(EVENT_BUS_CAPACITY);
        Self(sender)
    }
}

impl EventBus {
    pub fn subscribe(&self) -> EventReceiver {
        self.0.subscribe()
    }

    pub fn publish(&self, event: AgentEvent) {
        let _ = self.0.send(event);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStart {
        session_id: String,
    },
    QueueUpdate {
        pending: usize,
    },
    AgentStart {
        run_id: usize,
    },
    TurnStart {
        run_id: usize,
        turn_index: usize,
    },
    MessageStart {
        message_id: String,
        message: Message,
    },
    MessageUpdate {
        message_id: String,
        block_index: usize,
        delta: MessageDelta,
        usage: Option<Usage>,
    },
    MessageEnd {
        message_id: String,
        message: Message,
    },
    ToolExecutionStart {
        id: String,
        name: String,
        input: String,
    },
    ToolExecutionUpdate {
        id: String,
        update: Value,
    },
    ToolExecutionEnd {
        id: String,
        name: String,
        result: Value,
        is_error: bool,
    },
    TurnEnd {
        run_id: usize,
        turn_index: usize,
    },
    AgentEnd {
        run_id: usize,
    },
    AgentSettled {
        run_id: usize,
    },
    HookError {
        hook: String,
        error: String,
    },
    PersistenceError {
        error: String,
    },
    SessionFatal {
        error: String,
    },
    SessionShutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MessageDelta {
    Text(String),
    Thinking(String),
    ToolCallInput(String),
    Content(MessageContent),
}
