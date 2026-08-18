use anyhow::Result;
use e_agent_core::{SessionClient, SessionHandle, UserMessage};

use crate::{
    broker::UiBrokerServer,
    reducer::{Effect, SessionCommand},
};

pub async fn execute_effects(
    handle: &SessionClient,
    broker: Option<&UiBrokerServer>,
    effects: Vec<Effect>,
) -> Result<bool> {
    let mut exit = false;
    for effect in effects {
        match effect {
            Effect::Session(SessionCommand::Prompt(text)) => {
                let handle = handle.clone();
                tokio::task::spawn_local(async move {
                    let _ = handle.prompt(UserMessage::text(text)).await;
                });
            }
            Effect::Session(SessionCommand::Steer(text)) => {
                handle.steer(UserMessage::text(text)).await?
            }
            Effect::Session(SessionCommand::FollowUp(text)) => {
                handle.follow_up(UserMessage::text(text)).await?
            }
            Effect::Session(SessionCommand::Abort) => handle.abort().await?,
            Effect::Session(SessionCommand::Close) => handle.close().await?,
            Effect::UiReply(request, reply) => {
                if let Some(broker) = broker {
                    broker.reply(request, reply);
                }
            }
            Effect::SetTitle(title) => {
                crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(title))?;
            }
            Effect::Exit => exit = true,
        }
    }
    Ok(exit)
}
