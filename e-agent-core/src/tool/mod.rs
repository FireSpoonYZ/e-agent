pub mod ptc;

use e_agent_tool::SessionId;
use serde::{Deserialize, Serialize};

use crate::message::{MessageContent, ToolDef};

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    type Error: std::fmt::Debug;
    fn tool_defs(&self) -> Vec<ToolDef>;
    /// Extra system-prompt text contributed by loaded extensions, in load order.
    fn system_prompts(&self) -> Vec<String> {
        Vec::new()
    }
    async fn call(
        &self,
        session: SessionId,
        name: &str,
        input: String,
    ) -> Result<ToolOutput, Self::Error>;
    /// Release any per-session state held for `session`.
    async fn drop_session(&self, session: SessionId) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<MessageContent>,
    pub details: Option<serde_json::Value>,
}
