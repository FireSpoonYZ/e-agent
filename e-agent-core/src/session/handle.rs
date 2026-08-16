use anyhow::{Result, anyhow};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    event::{AgentEvent, EventBus, EventReceiver},
    message::{Message, UserMessage},
    provider::Provider,
    session::{
        Session, SessionMetadata, SessionStatus, SessionView,
        queue::{MessageQueue, QueuedMessage},
    },
    tool::{ToolExecutor, extension::ExtensionHost},
};

#[async_trait::async_trait(?Send)]
pub trait SessionHandle {
    async fn prompt(&self, message: UserMessage) -> Result<()>;
    async fn steer(&self, message: UserMessage) -> Result<()>;
    async fn follow_up(&self, message: UserMessage) -> Result<()>;
    async fn abort(&self) -> Result<()>;
    async fn close(&self) -> Result<()>;
}

#[derive(Clone)]
pub struct SessionClient {
    commands: mpsc::UnboundedSender<SessionCommand>,
    cancellation: std::sync::Arc<std::sync::Mutex<CancellationToken>>,
    queue: MessageQueue,
    events: EventBus,
}

pub struct SessionAttachment {
    pub handle: SessionClient,
    pub events: EventReceiver,
    pub metadata: SessionMetadata,
    pub messages: Vec<Message>,
    pub status: SessionStatus,
}

enum SessionCommand {
    Prompt(UserMessage, oneshot::Sender<Result<()>>),
    Wake,
    Close(oneshot::Sender<Result<()>>),
}

impl SessionClient {
    async fn send(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<()>>) -> SessionCommand,
    ) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.commands
            .send(command(reply))
            .map_err(|_| anyhow!("session is closed"))?;
        receive
            .await
            .map_err(|_| anyhow!("session actor stopped"))?
    }

    fn enqueue(&self, message: QueuedMessage) -> Result<()> {
        self.queue.push(message)?;
        self.events.publish(AgentEvent::QueueUpdate {
            pending: self.queue.len(),
        });
        self.commands
            .send(SessionCommand::Wake)
            .map_err(|_| anyhow!("session is closed"))
    }
}

#[async_trait::async_trait(?Send)]
impl SessionHandle for SessionClient {
    async fn prompt(&self, message: UserMessage) -> Result<()> {
        self.send(|reply| SessionCommand::Prompt(message, reply))
            .await
    }

    async fn steer(&self, message: UserMessage) -> Result<()> {
        self.enqueue(QueuedMessage::Steer(message))
    }

    async fn follow_up(&self, message: UserMessage) -> Result<()> {
        self.enqueue(QueuedMessage::FollowUp(message))
    }

    async fn abort(&self) -> Result<()> {
        self.cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancel();
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.send(SessionCommand::Close).await
    }
}

impl<P, E> Session<P, E>
where
    P: Provider + 'static,
    E: ToolExecutor + ExtensionHost + 'static,
{
    pub fn attach(self) -> SessionAttachment {
        // Subscribe before taking the snapshot so events emitted while the adapter
        // initializes remain buffered in this receiver.
        let events = self.subscribe();
        let metadata = self.metadata();
        let messages = self.messages();
        let status = self.status();
        let cancellation = self.cancellation_handle();
        let queue = self.queue_handle();
        let event_bus = self.event_bus();
        let (commands, mut receiver) = mpsc::unbounded_channel();
        tokio::task::spawn_local(async move {
            let mut session = self;
            while let Some(command) = receiver.recv().await {
                let close = matches!(command, SessionCommand::Close(_));
                match command {
                    SessionCommand::Prompt(message, reply) => {
                        let _ = reply.send(session.run_one_trun(message).await);
                    }
                    SessionCommand::Wake => {
                        let _ = session.run_queued().await;
                    }
                    SessionCommand::Close(reply) => {
                        let _ = reply.send(session.close().await);
                    }
                }
                if close {
                    break;
                }
            }
        });
        SessionAttachment {
            handle: SessionClient {
                commands,
                cancellation,
                queue,
                events: event_bus,
            },
            events,
            metadata,
            messages,
            status,
        }
    }
}
