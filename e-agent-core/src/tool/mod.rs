pub mod extension;

use e_agent_extension::SessionId;
use serde::{Deserialize, Serialize};

use crate::message::{MessageContent, ToolDef};

#[async_trait::async_trait(?Send)]
pub trait ToolExecutor: Send + Sync {
    type Error: std::fmt::Debug;
    fn tool_defs(&self) -> Vec<ToolDef>;
    fn system_prompts(&self) -> Vec<String> {
        Vec::new()
    }
    async fn call(
        &self,
        session: SessionId,
        name: &str,
        input: String,
    ) -> Result<ToolOutput, Self::Error>;
    async fn drop_session(&self, session: SessionId) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<MessageContent>,
    pub details: Option<serde_json::Value>,
}
