use serde_json::Value;

use crate::{message::Message, session::SessionContext};

#[derive(Debug, Clone)]
pub enum LifecycleEvent {
    SessionStart,
    Input {
        text: String,
        source: String,
    },
    BeforeAgentStart {
        prompt: String,
        system_prompt: String,
    },
    AgentStart,
    TurnStart {
        turn_index: usize,
    },
    MessageStart {
        message: Message,
    },
    MessageEnd {
        message: Message,
    },
    ToolCall {
        id: String,
        name: String,
        input: String,
    },
    ToolExecutionEnd {
        id: String,
        name: String,
        result: Value,
        is_error: bool,
    },
    TurnEnd {
        turn_index: usize,
        message: Option<Message>,
        tool_results: Vec<Message>,
    },
    AgentEnd {
        messages: Vec<Message>,
        error: Option<String>,
    },
    AgentSettled,
    SessionShutdown,
}

#[derive(Debug, Clone, Default)]
pub enum LifecycleEffect {
    #[default]
    None,
    TransformInput {
        text: String,
    },
    Handled,
    BeforeAgentStart {
        system_prompt: String,
        messages: Vec<Message>,
    },
    BlockTool {
        reason: String,
    },
}

#[async_trait::async_trait(?Send)]
pub trait LifecycleHook: Send + Sync {
    async fn dispatch(
        &self,
        event: LifecycleEvent,
        ctx: &SessionContext,
    ) -> anyhow::Result<LifecycleEffect>;
}
