use crate::{
    message::{Message, ToolResultMessage, UserMessage},
    session::SessionContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    Continue,
    Handled,
}

#[derive(Debug, Clone)]
pub struct BeforeAgentStart {
    pub prompt: String,
    pub system_prompt: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallOutcome {
    Continue,
    Block(String),
}

#[async_trait::async_trait(?Send)]
pub trait AgentHooks: Send + Sync {
    async fn on_input(
        &self,
        _message: &mut UserMessage,
        _ctx: &SessionContext,
    ) -> anyhow::Result<InputOutcome> {
        Ok(InputOutcome::Continue)
    }

    async fn before_agent_start(
        &self,
        _input: &mut BeforeAgentStart,
        _ctx: &SessionContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_context(
        &self,
        _messages: &mut Vec<Message>,
        _ctx: &SessionContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_tool_call(
        &self,
        _call: &mut ToolCall,
        _ctx: &SessionContext,
    ) -> anyhow::Result<ToolCallOutcome> {
        Ok(ToolCallOutcome::Continue)
    }

    async fn on_tool_result(
        &self,
        _result: &mut ToolResultMessage,
        _ctx: &SessionContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn on_message_finalizing(
        &self,
        _message: &mut Message,
        _ctx: &SessionContext,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
