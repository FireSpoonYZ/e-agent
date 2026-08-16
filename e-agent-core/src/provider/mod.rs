use std::pin::Pin;

use futures_core::Stream;

use crate::message::{Context, MessageContent, StopReason, Usage};

#[derive(Debug, Clone)]
pub enum ProviderEvent {
    ContentDelta {
        block_index: usize,
        delta: MessageContent,
    },
    Usage(Usage),
    Done(StopReason),
    Error(String),
    Aborted,
}

pub type ProviderStream<E> = Pin<Box<dyn Stream<Item = Result<ProviderEvent, E>> + Send + 'static>>;

#[async_trait::async_trait]
pub trait Provider {
    type Error: std::fmt::Debug;
    async fn stream(
        &self,
        model: &str,
        context: Context<'_>,
    ) -> Result<ProviderStream<Self::Error>, Self::Error>;
}
