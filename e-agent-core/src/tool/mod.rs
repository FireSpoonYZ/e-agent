pub mod ptc;

use serde::{Deserialize, Serialize};

use crate::message::{MessageContent, ToolDef};

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    type Error: std::fmt::Debug;
    fn tool_defs(&self) -> Vec<ToolDef>;
    async fn call(&self, name: &str, input: String) -> Result<ToolOutput, Self::Error>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<MessageContent>,
    pub details: Option<serde_json::Value>,
}
