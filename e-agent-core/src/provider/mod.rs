mod openai;

pub use openai::OpenAIProvider;

use crate::message::{AssistantMessage, Context};
#[async_trait::async_trait]
pub trait Provider {
    type Error: std::fmt::Debug;
    async fn send(
        &self,
        model: &str,
        context: Context<'_>,
    ) -> Result<AssistantMessage, Self::Error>;
}
