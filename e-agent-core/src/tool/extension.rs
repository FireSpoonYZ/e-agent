use crate::{event::AgentEvent, hooks::AgentHooks, session::SessionContext};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub enum HostAction {
    AppendEntry {
        kind: String,
        data: serde_json::Value,
    },
    SendUserMessage {
        text: String,
        deliver_as: String,
    },
    SendMessage {
        message: serde_json::Value,
        deliver_as: String,
        trigger_turn: bool,
    },
}

#[async_trait::async_trait(?Send)]
pub trait ExtensionHost: AgentHooks {
    async fn observe(&self, _event: &AgentEvent, _ctx: &SessionContext) -> anyhow::Result<()> {
        Ok(())
    }

    fn commands(&self) -> Vec<CommandDef>;
    async fn command(&self, name: &str, args: &str, ctx: &SessionContext) -> anyhow::Result<()>;
    fn take_host_actions(&self) -> Vec<HostAction>;
}
