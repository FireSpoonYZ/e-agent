pub mod queue;
pub mod store;

use std::path::{Path, PathBuf};

use anyhow::Result;
use e_agent_extension::SessionId;

use crate::{
    lifecycle::{LifecycleEffect, LifecycleEvent},
    message::{Message, MessageContent, ToolResultMessage, UserMessage},
    provider::Provider,
    session::{
        queue::{MessageQueue, MessageSink, QueuedMessage},
        store::{JsonlSessionStore, SessionStore},
    },
    tool::{
        ToolExecutor,
        extension::{ExtensionHost, HostAction},
    },
};

#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub entries: Vec<serde_json::Value>,
    pub idle: bool,
}

pub struct Session<P: Provider, E: ToolExecutor + ExtensionHost> {
    cwd: PathBuf,
    on_message: Option<Box<dyn Fn(&Message)>>,
    model: String,
    system_prompt: String,
    store: JsonlSessionStore,
    queue: MessageQueue,
    run: usize,
    provider: P,
    tool_executor: E,
    started: bool,
    closed: bool,
}

impl<P: Provider, E: ToolExecutor + ExtensionHost> Session<P, E> {
    pub fn new(
        provider: P,
        tool_executor: E,
        cwd: impl Into<PathBuf>,
        model: impl Into<String>,
        system_prompt: impl ToString,
    ) -> Self {
        Self::open(provider, tool_executor, cwd, model, system_prompt, None)
            .expect("create session store")
    }

    pub fn open(
        provider: P,
        tool_executor: E,
        cwd: impl Into<PathBuf>,
        model: impl Into<String>,
        system_prompt: impl ToString,
        path: Option<PathBuf>,
    ) -> Result<Self> {
        let mut queue = MessageQueue::default();
        queue.set_idle(true);
        Ok(Self {
            provider,
            tool_executor,
            cwd: cwd.into(),
            on_message: None,
            model: model.into(),
            system_prompt: system_prompt.to_string(),
            store: JsonlSessionStore::open(path)?,
            queue,
            run: 0,
            started: false,
            closed: false,
        })
    }

    pub fn set_message_handler(&mut self, handler: impl Fn(&Message) + 'static) {
        self.on_message = Some(Box::new(handler));
    }

    fn emit_message(&self, message: &Message) {
        if let Some(handler) = &self.on_message {
            handler(message);
        }
    }

    pub fn id(&self) -> SessionId {
        self.store.id()
    }
    pub fn path(&self) -> &Path {
        self.store.path()
    }

    fn context(&self) -> SessionContext {
        SessionContext {
            session_id: self.id(),
            cwd: self.cwd.clone(),
            entries: self
                .store
                .entries()
                .iter()
                .map(|entry| serde_json::to_value(entry).unwrap())
                .collect(),
            idle: self.queue.is_idle(),
        }
    }

    async fn dispatch(&mut self, event: LifecycleEvent) -> Result<LifecycleEffect> {
        let effect = self.tool_executor.dispatch(event, &self.context()).await?;
        self.apply_actions()?;
        Ok(effect)
    }

    fn apply_actions(&mut self) -> Result<()> {
        for action in self.tool_executor.take_host_actions() {
            match action {
                HostAction::AppendEntry { kind, data } => self.store.append_custom(kind, data)?,
                HostAction::SendUserMessage { text, deliver_as } => {
                    let queued = if deliver_as == "steer" {
                        QueuedMessage::Steer(text)
                    } else {
                        QueuedMessage::FollowUp(text)
                    };
                    self.queue.enqueue(queued)?;
                }
                HostAction::SendMessage {
                    message,
                    deliver_as,
                    trigger_turn,
                } => {
                    let kind = message["customType"]
                        .as_str()
                        .unwrap_or("extension-message")
                        .to_string();
                    self.store.append_custom(kind, message.clone())?;
                    if trigger_turn {
                        let text = message["content"].as_str().unwrap_or_default().to_string();
                        let queued = if deliver_as == "steer" {
                            QueuedMessage::Steer(text)
                        } else {
                            QueuedMessage::FollowUp(text)
                        };
                        self.queue.enqueue(queued)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn ensure_started(&mut self) -> Result<()> {
        if !self.started {
            self.started = true;
            self.dispatch(LifecycleEvent::SessionStart).await?;
        }
        Ok(())
    }

    pub async fn resume_pending(&mut self) -> Result<()> {
        let resumed_goal = self
            .store
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                store::SessionEntry::Custom { custom_type, data }
                    if custom_type == "goal-state" && data["goal"]["status"] == "active" =>
                {
                    Some((
                        data["goal"]["id"].as_str()?.to_string(),
                        data["goal"]["text"].as_str()?.to_string(),
                    ))
                }
                _ => None,
            });
        self.ensure_started().await?;
        self.queue.set_idle(true);
        self.dispatch(LifecycleEvent::AgentSettled).await?;
        if let Some((goal_id, objective)) = resumed_goal {
            self.queue.enqueue(QueuedMessage::FollowUp(format!(
                "Resume the active goal after process restart. Objective: {objective}\nUse the current goal_id {goal_id} when completing it."
            )))?;
        }
        while let Some(follow_up) = self.queue.pop_follow_up() {
            self.run_agent(UserMessage::text(follow_up)).await?;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.ensure_started().await?;
        self.dispatch(LifecycleEvent::SessionShutdown).await?;
        self.store.save()?;
        self.tool_executor
            .drop_session(self.id())
            .await
            .map_err(|err| anyhow::anyhow!("drop session state failed: {err:?}"))?;
        self.closed = true;
        Ok(())
    }

    pub fn build_system_prompt(&self) -> String {
        let mut prompt = format!(
            "{}\n当前时间为:{}\n当前目录为:{}",
            self.system_prompt,
            chrono::Local::now(),
            self.cwd.display()
        );
        for extension_prompt in self.tool_executor.system_prompts() {
            prompt.push('\n');
            prompt.push_str(&extension_prompt);
        }
        prompt
    }

    pub async fn run_one_trun(&mut self, user_input: UserMessage) -> Result<()> {
        self.ensure_started().await?;
        let mut text = user_input
            .content
            .iter()
            .filter_map(|part| match part {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if let Some((name, args)) = parse_command(&text)
            && self
                .tool_executor
                .commands()
                .iter()
                .any(|command| command.name == name)
        {
            self.tool_executor
                .command(name, args, &self.context())
                .await?;
            self.apply_actions()?;
        } else {
            match self
                .dispatch(LifecycleEvent::Input {
                    text: text.clone(),
                    source: "interactive".into(),
                })
                .await?
            {
                LifecycleEffect::TransformInput { text: transformed } => text = transformed,
                LifecycleEffect::Handled => return Ok(()),
                _ => {}
            }
            self.run_agent(UserMessage::text(text)).await?;
        }
        while let Some(follow_up) = self.queue.pop_follow_up() {
            self.run_agent(UserMessage::text(follow_up)).await?;
        }
        Ok(())
    }

    async fn run_agent(&mut self, user_input: UserMessage) -> Result<()> {
        self.run += 1;
        self.queue.set_idle(false);
        let user = Message::User(user_input);
        self.store.append_message(user.clone())?;
        self.dispatch(LifecycleEvent::MessageStart {
            message: user.clone(),
        })
        .await?;
        self.dispatch(LifecycleEvent::MessageEnd {
            message: user.clone(),
        })
        .await?;

        let prompt = user
            .content()
            .iter()
            .filter_map(|part| match part {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut system_prompt = self.build_system_prompt();
        if let LifecycleEffect::BeforeAgentStart {
            system_prompt: changed,
            messages,
        } = self
            .dispatch(LifecycleEvent::BeforeAgentStart {
                prompt,
                system_prompt: system_prompt.clone(),
            })
            .await?
        {
            if !changed.is_empty() {
                system_prompt = changed;
            }
            for message in messages {
                self.store.append_message(message)?;
            }
        }
        self.dispatch(LifecycleEvent::AgentStart).await?;
        let start = self.store.messages().len();
        let result = self.agent_loop(system_prompt).await;
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        self.dispatch(LifecycleEvent::AgentEnd {
            messages: self.store.messages()[start..].to_vec(),
            error,
        })
        .await?;
        self.queue.set_idle(true);
        self.dispatch(LifecycleEvent::AgentSettled).await?;
        result
    }

    async fn agent_loop(&mut self, system_prompt: String) -> Result<()> {
        let mut turn_index = 0;
        loop {
            self.dispatch(LifecycleEvent::TurnStart { turn_index })
                .await?;
            let tool_defs = self.tool_executor.tool_defs();
            let context = crate::message::Context {
                system_prompt: Some(&system_prompt),
                messages: self.store.messages(),
                tools: &tool_defs,
            };
            let answer = self
                .provider
                .send(&self.model, context)
                .await
                .map_err(|error| anyhow::anyhow!("llm invoke failed: {error:?}"))?;
            let assistant = Message::Assistant(answer.clone());
            self.emit_message(&assistant);
            self.dispatch(LifecycleEvent::MessageStart {
                message: assistant.clone(),
            })
            .await?;
            self.store.append_message(assistant.clone())?;
            self.dispatch(LifecycleEvent::MessageEnd {
                message: assistant.clone(),
            })
            .await?;

            let mut tool_results = Vec::new();
            for content in answer.content {
                let MessageContent::ToolUse {
                    id,
                    name,
                    input,
                    custom,
                    ..
                } = content
                else {
                    continue;
                };
                let blocked = match self
                    .dispatch(LifecycleEvent::ToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    })
                    .await?
                {
                    LifecycleEffect::BlockTool { reason } => Some(reason),
                    _ => None,
                };
                let result = if let Some(reason) = blocked {
                    Err(anyhow::anyhow!(reason))
                } else {
                    self.tool_executor
                        .call(self.id(), &name, input)
                        .await
                        .map_err(|error| anyhow::anyhow!("{error:?}"))
                };
                let (tool_result, value, is_error) = match result {
                    Ok(output) => (
                        ToolResultMessage {
                            tool_use_id: id.clone(),
                            content: output.content,
                            is_error: false,
                            custom,
                        },
                        output.details.unwrap_or_default(),
                        false,
                    ),
                    Err(error) => {
                        let mut result = ToolResultMessage::error(id.clone(), format!("{error:#}"));
                        result.custom = custom;
                        (
                            result,
                            serde_json::json!({"error":format!("{error:#}")}),
                            true,
                        )
                    }
                };
                self.dispatch(LifecycleEvent::ToolExecutionEnd {
                    id,
                    name,
                    result: value,
                    is_error,
                })
                .await?;
                let message = Message::ToolResult(tool_result);
                self.emit_message(&message);
                self.store.append_message(message.clone())?;
                tool_results.push(message);
                if let Some(steer) = self.queue.pop_steer() {
                    self.store
                        .append_message(Message::User(UserMessage::text(steer)))?;
                }
            }
            self.dispatch(LifecycleEvent::TurnEnd {
                turn_index,
                message: Some(assistant),
                tool_results: tool_results.clone(),
            })
            .await?;
            if tool_results.is_empty() {
                break;
            }
            turn_index += 1;
        }
        Ok(())
    }
}

fn parse_command(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let command = input.strip_prefix('/')?;
    let split = command.find(char::is_whitespace).unwrap_or(command.len());
    Some((&command[..split], command[split..].trim_start()))
}
