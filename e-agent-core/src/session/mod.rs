pub mod handle;
pub mod queue;
pub mod store;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use e_agent_extension::SessionId;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    event::{AgentEvent, EventBus, EventReceiver, MessageDelta},
    hooks::{BeforeAgentStart, InputOutcome, ToolCall, ToolCallOutcome},
    message::{
        AssistantMessage, Message, MessageContent, StopReason, ToolInput, ToolResultMessage,
        UserMessage,
    },
    provider::{Provider, ProviderEvent},
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
    pub signal: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Fatal,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: String,
    pub path: PathBuf,
    pub cwd: PathBuf,
}

pub trait SessionView {
    fn metadata(&self) -> SessionMetadata;
    fn messages(&self) -> Vec<Message>;
    fn status(&self) -> SessionStatus;
}

pub struct Session<P: Provider, E: ToolExecutor + ExtensionHost> {
    cwd: PathBuf,
    model: String,
    system_prompt: String,
    store: Box<dyn SessionStore>,
    queue: MessageQueue,
    run: usize,
    provider: P,
    tool_executor: E,
    events: EventBus,
    status: SessionStatus,
    started: bool,
    shutdown_emitted: bool,
    cancellation: std::sync::Arc<std::sync::Mutex<CancellationToken>>,
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
        let store = JsonlSessionStore::open(path)?;
        Ok(Self::with_store(
            provider,
            tool_executor,
            cwd,
            model,
            system_prompt,
            store,
        ))
    }

    pub fn with_store(
        provider: P,
        tool_executor: E,
        cwd: impl Into<PathBuf>,
        model: impl Into<String>,
        system_prompt: impl ToString,
        store: impl SessionStore + 'static,
    ) -> Self {
        let queue = MessageQueue::default();
        queue.set_idle(true);
        Self {
            provider,
            tool_executor,
            cwd: cwd.into(),
            model: model.into(),
            system_prompt: system_prompt.to_string(),
            store: Box::new(store),
            queue,
            run: 0,
            events: EventBus::default(),
            status: SessionStatus::Idle,
            started: false,
            shutdown_emitted: false,
            cancellation: std::sync::Arc::new(std::sync::Mutex::new(CancellationToken::new())),
        }
    }

    pub fn subscribe(&self) -> EventReceiver {
        self.events.subscribe()
    }

    pub fn id(&self) -> SessionId {
        self.store.id()
    }

    pub fn path(&self) -> &Path {
        self.store.path()
    }

    pub(crate) fn cancellation_handle(
        &self,
    ) -> std::sync::Arc<std::sync::Mutex<CancellationToken>> {
        std::sync::Arc::clone(&self.cancellation)
    }

    pub(crate) fn queue_handle(&self) -> MessageQueue {
        self.queue.clone()
    }

    pub(crate) fn event_bus(&self) -> EventBus {
        self.events.clone()
    }

    pub(crate) async fn run_queued(&mut self) -> Result<()> {
        while let Some(message) = self
            .queue
            .pop_steer()
            .or_else(|| self.queue.pop_follow_up())
        {
            self.reset_cancellation();
            self.run_agent(message).await?;
        }
        Ok(())
    }

    fn cancellation(&self) -> CancellationToken {
        self.cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn reset_cancellation(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self
            .cancellation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = token.clone();
        token
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
            idle: self.status == SessionStatus::Idle && self.queue.is_idle(),
            signal: self.cancellation(),
        }
    }

    async fn emit(&mut self, event: AgentEvent) {
        if self.shutdown_emitted {
            return;
        }
        self.events.publish(event.clone());
        if let Err(error) = self.tool_executor.observe(&event, &self.context()).await {
            self.events.publish(AgentEvent::HookError {
                hook: "observer".into(),
                error: format!("{error:#}"),
            });
        }
        if let Err(error) = self.apply_actions() {
            self.events.publish(AgentEvent::HookError {
                hook: "host_action".into(),
                error: format!("{error:#}"),
            });
        }
    }

    fn apply_actions(&mut self) -> Result<()> {
        for action in self.tool_executor.take_host_actions() {
            match action {
                HostAction::AppendEntry { kind, data } => self.store.append_custom(kind, data)?,
                HostAction::SendUserMessage { text, deliver_as } => {
                    let queued = if deliver_as == "steer" {
                        QueuedMessage::Steer(UserMessage::text(text))
                    } else {
                        QueuedMessage::FollowUp(UserMessage::text(text))
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
                            QueuedMessage::Steer(UserMessage::text(text))
                        } else {
                            QueuedMessage::FollowUp(UserMessage::text(text))
                        };
                        self.queue.enqueue(queued)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn ensure_started(&mut self) {
        if !self.started {
            self.started = true;
            self.emit(AgentEvent::SessionStart {
                session_id: self.id().to_string(),
            })
            .await;
        }
    }

    async fn fatal(&mut self, error: anyhow::Error) -> anyhow::Error {
        let message = format!("{error:#}");
        self.status = SessionStatus::Fatal;
        self.queue.close();
        self.events.publish(AgentEvent::PersistenceError {
            error: message.clone(),
        });
        self.events.publish(AgentEvent::SessionFatal {
            error: message.clone(),
        });
        let _ = self.tool_executor.drop_session(self.id()).await;
        self.events.publish(AgentEvent::SessionShutdown);
        self.shutdown_emitted = true;
        anyhow!(message)
    }

    async fn persist(&mut self, message_id: String, mut message: Message) -> Result<Message> {
        let mut working = message.clone();
        match self
            .tool_executor
            .on_message_finalizing(&mut working, &self.context())
            .await
        {
            Ok(()) => match validate_final_message(&message, &working) {
                Ok(()) => message = working,
                Err(error) => {
                    self.emit(AgentEvent::HookError {
                        hook: "message_finalizing".into(),
                        error: error.to_string(),
                    })
                    .await
                }
            },
            Err(error) => {
                self.emit(AgentEvent::HookError {
                    hook: "message_finalizing".into(),
                    error: format!("{error:#}"),
                })
                .await
            }
        }
        if let Err(error) = self.store.append_message(message.clone()) {
            return Err(self.fatal(error).await);
        }
        self.emit(AgentEvent::MessageEnd {
            message_id,
            message: message.clone(),
        })
        .await;
        Ok(message)
    }

    pub async fn resume_pending(&mut self) -> Result<()> {
        let resumed_goal = self
            .store
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                store::SessionEntry::Custom { custom_type, data }
                    if custom_type == "goal-state" =>
                {
                    Some((
                        data["goal"]["status"] == "active",
                        data["goal"]["id"].as_str().map(str::to_owned),
                        data["goal"]["text"].as_str().map(str::to_owned),
                    ))
                }
                _ => None,
            })
            .and_then(|(active, id, text)| active.then(|| Some((id?, text?)))?);
        self.ensure_started().await;
        if let Some((goal_id, objective)) = resumed_goal {
            self.queue.enqueue(QueuedMessage::FollowUp(UserMessage::text(format!("Resume the active goal after process restart. Objective: {objective}\nUse the current goal_id {goal_id} when completing it."))))?;
        }
        while let Some(follow_up) = self.queue.pop_follow_up() {
            self.reset_cancellation();
            self.run_agent(follow_up).await?;
        }
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.shutdown_emitted {
            return Ok(());
        }
        self.ensure_started().await;
        self.store.save()?;
        self.tool_executor
            .drop_session(self.id())
            .await
            .map_err(|error| anyhow!("drop session state failed: {error:?}"))?;
        self.status = SessionStatus::Closed;
        self.queue.close();
        self.events.publish(AgentEvent::SessionShutdown);
        self.shutdown_emitted = true;
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

    pub async fn run_one_trun(&mut self, mut user_input: UserMessage) -> Result<()> {
        if matches!(self.status, SessionStatus::Fatal | SessionStatus::Closed) {
            return Err(anyhow!("session is not accepting messages"));
        }
        self.ensure_started().await;
        self.reset_cancellation();
        let text = message_text(&user_input);
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
            let mut working = user_input.clone();
            match self
                .tool_executor
                .on_input(&mut working, &self.context())
                .await
            {
                Ok(InputOutcome::Continue) => user_input = working,
                Ok(InputOutcome::Handled) => return Ok(()),
                Err(error) => {
                    self.emit(AgentEvent::HookError {
                        hook: "input".into(),
                        error: format!("{error:#}"),
                    })
                    .await
                }
            }
            self.run_agent(user_input).await?;
        }
        while let Some(follow_up) = self.queue.pop_follow_up() {
            self.reset_cancellation();
            self.run_agent(follow_up).await?;
        }
        Ok(())
    }

    async fn run_agent(&mut self, user_input: UserMessage) -> Result<()> {
        self.run += 1;
        let cancellation = self.cancellation();
        let run_id = self.run;
        self.status = SessionStatus::Running;
        self.queue.set_idle(false);

        let mut before = BeforeAgentStart {
            prompt: message_text(&user_input),
            system_prompt: self.build_system_prompt(),
            messages: Vec::new(),
        };
        let mut working = before.clone();
        match self
            .tool_executor
            .before_agent_start(&mut working, &self.context())
            .await
        {
            Ok(()) => before = working,
            Err(error) => {
                self.emit(AgentEvent::HookError {
                    hook: "before_agent_start".into(),
                    error: format!("{error:#}"),
                })
                .await
            }
        }

        self.emit(AgentEvent::AgentStart { run_id }).await;
        let result = self
            .agent_loop(
                run_id,
                user_input,
                before.system_prompt,
                before.messages,
                cancellation,
            )
            .await;
        if result.is_err() && self.status == SessionStatus::Fatal {
            return result;
        }
        result?;
        self.emit(AgentEvent::AgentEnd { run_id }).await;
        self.queue.set_idle(true);
        self.status = SessionStatus::Idle;
        self.emit(AgentEvent::AgentSettled { run_id }).await;
        Ok(())
    }

    async fn agent_loop(
        &mut self,
        run_id: usize,
        user_input: UserMessage,
        system_prompt: String,
        injected: Vec<Message>,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let mut turn_index = 0;
        loop {
            self.emit(AgentEvent::TurnStart { run_id, turn_index })
                .await;
            if turn_index == 0 {
                let user_id = format!("{run_id}:{turn_index}:user");
                let user = Message::User(user_input.clone());
                self.emit(AgentEvent::MessageStart {
                    message_id: user_id.clone(),
                    message: user.clone(),
                })
                .await;
                self.persist(user_id, user).await?;
                for message in &injected {
                    let id = format!(
                        "{run_id}:{turn_index}:injected:{}",
                        self.store.messages().len()
                    );
                    self.emit(AgentEvent::MessageStart {
                        message_id: id.clone(),
                        message: message.clone(),
                    })
                    .await;
                    self.persist(id, message.clone()).await?;
                }
            }

            let tool_defs = self.tool_executor.tool_defs();
            let mut messages = self.store.messages().to_vec();
            let mut working = messages.clone();
            match self
                .tool_executor
                .on_context(&mut working, &self.context())
                .await
            {
                Ok(()) => messages = working,
                Err(error) => {
                    self.emit(AgentEvent::HookError {
                        hook: "context".into(),
                        error: format!("{error:#}"),
                    })
                    .await
                }
            }
            let assistant_id = format!("{run_id}:{turn_index}:assistant");
            let initial = Message::Assistant(AssistantMessage {
                content: Vec::new(),
                stop_reason: StopReason::Stop,
                usage: None,
                error_message: None,
            });
            self.emit(AgentEvent::MessageStart {
                message_id: assistant_id.clone(),
                message: initial,
            })
            .await;

            let context = crate::message::Context {
                system_prompt: Some(&system_prompt),
                messages: &messages,
                tools: &tool_defs,
            };
            let stream = tokio::select! {
                _ = cancellation.cancelled() => None,
                stream = self.provider.stream(&self.model, context) => Some(stream),
            };
            let mut blocks = BTreeMap::<usize, MessageContent>::new();
            let mut usage = None;
            let mut stop_reason = StopReason::Error;
            let mut error_message = None;
            let mut terminal = false;
            match stream {
                None => {
                    stop_reason = StopReason::Aborted;
                    terminal = true;
                }
                Some(Ok(mut stream)) => loop {
                    let item = tokio::select! {
                        _ = cancellation.cancelled() => {
                            stop_reason = StopReason::Aborted;
                            terminal = true;
                            break;
                        }
                        item = stream.next() => item,
                    };
                    let Some(item) = item else { break };
                    match item {
                        Ok(ProviderEvent::ContentDelta { block_index, delta }) => {
                            merge_delta(&mut blocks, block_index, delta.clone());
                            let delta = match delta {
                                MessageContent::Text { text } => MessageDelta::Text(text),
                                MessageContent::Thinking { thinking, .. } => {
                                    MessageDelta::Thinking(thinking)
                                }
                                MessageContent::ToolUse { input, .. } => {
                                    MessageDelta::ToolCallInput(input)
                                }
                            };
                            self.emit(AgentEvent::MessageUpdate {
                                message_id: assistant_id.clone(),
                                block_index,
                                delta,
                                usage,
                            })
                            .await;
                        }
                        Ok(ProviderEvent::Usage(value)) => usage = Some(value),
                        Ok(ProviderEvent::Done(reason)) => {
                            stop_reason = reason;
                            terminal = true;
                            break;
                        }
                        Ok(ProviderEvent::Error(error)) => {
                            stop_reason = StopReason::Error;
                            error_message = Some(error);
                            terminal = true;
                            break;
                        }
                        Ok(ProviderEvent::Aborted) => {
                            stop_reason = StopReason::Aborted;
                            terminal = true;
                            break;
                        }
                        Err(error) => {
                            stop_reason = StopReason::Error;
                            error_message = Some(format!("{error:?}"));
                            terminal = true;
                            break;
                        }
                    }
                },
                Some(Err(error)) => {
                    error_message = Some(format!("{error:?}"));
                    terminal = true;
                }
            }
            if !terminal {
                error_message = Some("provider stream ended without a terminal event".into());
            }
            let content = blocks.into_values().collect::<Vec<_>>();
            if content
                .iter()
                .any(|part| matches!(part, MessageContent::ToolUse { .. }))
                && !matches!(stop_reason, StopReason::Error | StopReason::Aborted)
            {
                stop_reason = StopReason::ToolUse;
            }
            let assistant = Message::Assistant(AssistantMessage {
                content,
                stop_reason,
                usage,
                error_message,
            });
            let assistant = self.persist(assistant_id, assistant).await?;
            let Message::Assistant(answer) = assistant else {
                unreachable!()
            };

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
                self.emit(AgentEvent::ToolExecutionStart {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                })
                .await;
                let mut call = ToolCall {
                    id: id.clone(),
                    name,
                    input,
                };
                let outcome = match self
                    .tool_executor
                    .on_tool_call(&mut call, &self.context())
                    .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let message = format!("tool hook failed: {error:#}");
                        self.emit(AgentEvent::HookError {
                            hook: "tool_call".into(),
                            error: message.clone(),
                        })
                        .await;
                        ToolCallOutcome::Block(message)
                    }
                };
                let validation = validate_tool_call(&call, &tool_defs);
                if let Err(error) = &validation {
                    self.emit(AgentEvent::HookError {
                        hook: "tool_call".into(),
                        error: error.to_string(),
                    })
                    .await;
                }
                let result = match (outcome, validation) {
                    (ToolCallOutcome::Block(reason), _) => Err(anyhow!(reason)),
                    (_, Err(error)) => Err(error),
                    _ => tokio::select! {
                        _ = cancellation.cancelled() => Err(anyhow!("aborted")),
                        result = self.tool_executor.call(self.id(), &call.name, call.input) => result.map_err(|error| anyhow!("{error:?}")),
                    },
                };
                let mut tool_result = match result {
                    Ok(output) => ToolResultMessage {
                        tool_use_id: id.clone(),
                        content: output.content,
                        is_error: false,
                        details: output.details,
                        custom,
                    },
                    Err(error) => ToolResultMessage {
                        details: Some(serde_json::json!({"error": format!("{error:#}")})),
                        custom,
                        ..ToolResultMessage::error(id.clone(), format!("{error:#}"))
                    },
                };
                let mut working = tool_result.clone();
                match self
                    .tool_executor
                    .on_tool_result(&mut working, &self.context())
                    .await
                {
                    Ok(()) => tool_result = working,
                    Err(error) => {
                        self.emit(AgentEvent::HookError {
                            hook: "tool_result".into(),
                            error: format!("{error:#}"),
                        })
                        .await
                    }
                }
                self.emit(AgentEvent::ToolExecutionEnd {
                    id: id.clone(),
                    name: call.name,
                    result: tool_result.details.clone().unwrap_or_default(),
                    is_error: tool_result.is_error,
                })
                .await;
                let message = Message::ToolResult(tool_result);
                let result_id = format!("{run_id}:{turn_index}:tool:{id}");
                self.emit(AgentEvent::MessageStart {
                    message_id: result_id.clone(),
                    message: message.clone(),
                })
                .await;
                tool_results.push(self.persist(result_id, message).await?);
                if let Some(steer) = self.queue.pop_steer() {
                    let id = format!("{run_id}:{turn_index}:steer");
                    let message = Message::User(steer);
                    self.emit(AgentEvent::MessageStart {
                        message_id: id.clone(),
                        message: message.clone(),
                    })
                    .await;
                    self.persist(id, message).await?;
                }
            }
            self.emit(AgentEvent::TurnEnd { run_id, turn_index }).await;
            if tool_results.is_empty() {
                break;
            }
            turn_index += 1;
        }
        Ok(())
    }
}

impl<P: Provider, E: ToolExecutor + ExtensionHost> SessionView for Session<P, E> {
    fn metadata(&self) -> SessionMetadata {
        SessionMetadata {
            session_id: self.id().to_string(),
            path: self.path().to_owned(),
            cwd: self.cwd.clone(),
        }
    }

    fn messages(&self) -> Vec<Message> {
        self.store.messages().to_vec()
    }

    fn status(&self) -> SessionStatus {
        self.status
    }
}

fn merge_delta(blocks: &mut BTreeMap<usize, MessageContent>, index: usize, delta: MessageContent) {
    match (blocks.get_mut(&index), delta) {
        (Some(MessageContent::Text { text }), MessageContent::Text { text: delta }) => {
            text.push_str(&delta)
        }
        (
            Some(MessageContent::Thinking { thinking, .. }),
            MessageContent::Thinking {
                thinking: delta, ..
            },
        ) => thinking.push_str(&delta),
        (_, delta) => {
            blocks.insert(index, delta);
        }
    }
}

fn validate_tool_call(call: &ToolCall, tools: &[crate::message::ToolDef]) -> Result<()> {
    let tool = tools
        .iter()
        .find(|tool| tool.name == call.name)
        .ok_or_else(|| anyhow!("unknown tool {}", call.name))?;
    if let ToolInput::Json(schema) = &tool.input {
        let input = serde_json::from_str::<serde_json::Value>(&call.input)
            .map_err(|error| anyhow!("invalid JSON tool input: {error}"))?;
        jsonschema::validator_for(schema)
            .map_err(|error| anyhow!("invalid tool schema: {error}"))?
            .validate(&input)
            .map_err(|error| anyhow!("tool input does not match schema: {error}"))?;
    }
    Ok(())
}

fn validate_final_message(original: &Message, message: &Message) -> Result<()> {
    if role(message) != role(original) {
        return Err(anyhow!("message role cannot change"));
    }
    match (original, message) {
        (_, Message::User(message)) if message.content.is_empty() => {
            Err(anyhow!("user content cannot be empty"))
        }
        (Message::Assistant(original), Message::Assistant(message))
            if matches!(
                original.stop_reason,
                StopReason::Error | StopReason::Aborted
            ) && message.stop_reason != original.stop_reason =>
        {
            Err(anyhow!("assistant terminal stop reason cannot change"))
        }
        (_, Message::Assistant(message))
            if message.stop_reason == StopReason::Error
                && message.error_message.as_deref().is_none_or(str::is_empty) =>
        {
            Err(anyhow!("error assistant requires error_message"))
        }
        (_, Message::ToolResult(message))
            if message.tool_use_id.is_empty() || message.content.is_empty() =>
        {
            Err(anyhow!("tool result requires id and content"))
        }
        _ => Ok(()),
    }
}

fn role(message: &Message) -> &'static str {
    match message {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "tool_result",
    }
}

#[cfg(test)]
fn message_text_content(content: &[MessageContent]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn message_text(message: &UserMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_command(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let command = input.strip_prefix('/')?;
    let split = command.find(char::is_whitespace).unwrap_or(command.len());
    Some((&command[..split], command[split..].trim_start()))
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use super::*;
    use crate::{
        AgentHooks, ProviderStream, SessionHandle, ToolOutput,
        session::store::SessionEntry,
        tool::extension::{CommandDef, ExtensionHost, HostAction},
    };

    struct FakeProvider;

    #[async_trait::async_trait]
    impl Provider for FakeProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _model: &str,
            _context: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            Ok(Box::pin(futures_util::stream::iter([
                Ok(ProviderEvent::ContentDelta {
                    block_index: 0,
                    delta: MessageContent::text("Hel"),
                }),
                Ok(ProviderEvent::ContentDelta {
                    block_index: 0,
                    delta: MessageContent::text("lo"),
                }),
                Ok(ProviderEvent::Done(StopReason::Stop)),
            ])))
        }
    }

    #[derive(Default)]
    struct SeenContext {
        system_prompt: Option<String>,
        messages: Vec<Message>,
    }

    struct RecordingProvider(Arc<Mutex<SeenContext>>);

    #[async_trait::async_trait]
    impl Provider for RecordingProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _model: &str,
            context: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            *self.0.lock().unwrap() = SeenContext {
                system_prompt: context.system_prompt.map(str::to_owned),
                messages: context.messages.to_vec(),
            };
            Ok(Box::pin(futures_util::stream::iter([Ok(
                ProviderEvent::Done(StopReason::Stop),
            )])))
        }
    }

    struct TransformInput;

    #[async_trait::async_trait(?Send)]
    impl AgentHooks for TransformInput {
        async fn on_input(
            &self,
            message: &mut UserMessage,
            _: &SessionContext,
        ) -> Result<InputOutcome> {
            *message = UserMessage::text(format!("{} A B", message_text(message)));
            Ok(InputOutcome::Continue)
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for TransformInput {
        type Error = Infallible;
        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            Vec::new()
        }
        async fn call(&self, _: SessionId, _: &str, _: String) -> Result<ToolOutput, Self::Error> {
            unreachable!()
        }
        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for TransformInput {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn transformed_input_is_persisted_and_sent_to_the_provider() {
        let directory = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(SeenContext::default()));
        let mut session = Session::open(
            RecordingProvider(Arc::clone(&seen)),
            TransformInput,
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        let persisted = SessionView::messages(&session);
        assert!(
            matches!(&persisted[0], Message::User(message) if message_text(message) == "hello A B")
        );
        assert!(
            matches!(&seen.lock().unwrap().messages[0], Message::User(message) if message_text(message) == "hello A B")
        );
    }

    enum HookMode {
        BeforeAndContext,
        ContextError,
        ValidFinal,
        InvalidFinal,
        EraseTerminal,
    }

    struct HookTools(HookMode);

    #[async_trait::async_trait(?Send)]
    impl AgentHooks for HookTools {
        async fn before_agent_start(
            &self,
            input: &mut BeforeAgentStart,
            _: &SessionContext,
        ) -> Result<()> {
            if matches!(self.0, HookMode::BeforeAndContext) {
                input.system_prompt = "hook system".into();
                input
                    .messages
                    .push(Message::User(UserMessage::text("injected")));
            }
            Ok(())
        }

        async fn on_context(&self, messages: &mut Vec<Message>, _: &SessionContext) -> Result<()> {
            match self.0 {
                HookMode::BeforeAndContext => {
                    messages.push(Message::User(UserMessage::text("context")));
                    Ok(())
                }
                HookMode::ContextError => {
                    messages.push(Message::User(UserMessage::text("discarded")));
                    Err(anyhow!("context hook failed"))
                }
                HookMode::InvalidFinal | HookMode::ValidFinal | HookMode::EraseTerminal => Ok(()),
            }
        }

        async fn on_message_finalizing(
            &self,
            message: &mut Message,
            _: &SessionContext,
        ) -> Result<()> {
            match self.0 {
                HookMode::InvalidFinal if matches!(message, Message::Assistant(_)) => {
                    *message = Message::User(UserMessage::text("invalid role"));
                }
                HookMode::ValidFinal => {
                    if let Message::Assistant(assistant) = message {
                        assistant.content = vec![MessageContent::text("final replacement")];
                    }
                }
                HookMode::EraseTerminal => {
                    if let Message::Assistant(assistant) = message
                        && matches!(
                            assistant.stop_reason,
                            StopReason::Error | StopReason::Aborted
                        )
                    {
                        assistant.stop_reason = StopReason::Stop;
                        assistant.error_message = None;
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for HookTools {
        type Error = Infallible;
        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            Vec::new()
        }
        async fn call(&self, _: SessionId, _: &str, _: String) -> Result<ToolOutput, Self::Error> {
            unreachable!()
        }
        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for HookTools {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    struct ErrorProvider;

    #[async_trait::async_trait]
    impl Provider for ErrorProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _: &str,
            _: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            Ok(Box::pin(futures_util::stream::iter([
                Ok(ProviderEvent::ContentDelta {
                    block_index: 0,
                    delta: MessageContent::text("partial"),
                }),
                Ok(ProviderEvent::Error("provider failed".into())),
            ])))
        }
    }

    struct AbortedProvider;

    #[async_trait::async_trait]
    impl Provider for AbortedProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _: &str,
            _: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            Ok(Box::pin(futures_util::stream::iter([Ok(
                ProviderEvent::Aborted,
            )])))
        }
    }

    struct ToolProvider {
        calls: AtomicUsize,
        contexts: Arc<Mutex<Vec<Vec<Message>>>>,
    }

    #[async_trait::async_trait]
    impl Provider for ToolProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _: &str,
            context: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            self.contexts
                .lock()
                .unwrap()
                .push(context.messages.to_vec());
            let events = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![
                    Ok(ProviderEvent::ContentDelta {
                        block_index: 0,
                        delta: MessageContent::ToolUse {
                            id: "call_1".into(),
                            name: "echo".into(),
                            input: r#"{"value":"original"}"#.into(),
                            custom: false,
                            item_id: None,
                        },
                    }),
                    Ok(ProviderEvent::Done(StopReason::ToolUse)),
                ]
            } else {
                vec![
                    Ok(ProviderEvent::ContentDelta {
                        block_index: 0,
                        delta: MessageContent::text("done"),
                    }),
                    Ok(ProviderEvent::Done(StopReason::Stop)),
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(events)))
        }
    }

    #[derive(Clone, Copy)]
    enum ToolHookMode {
        Valid,
        Invalid,
        Error,
    }

    struct ToolHooks {
        calls: Arc<Mutex<Vec<String>>>,
        mode: ToolHookMode,
    }

    #[async_trait::async_trait(?Send)]
    impl AgentHooks for ToolHooks {
        async fn on_tool_call(
            &self,
            call: &mut ToolCall,
            _: &SessionContext,
        ) -> Result<ToolCallOutcome> {
            match self.mode {
                ToolHookMode::Valid => call.input = r#"{"value":"mutated"}"#.into(),
                ToolHookMode::Invalid => call.input = r#"{"unknown":true}"#.into(),
                ToolHookMode::Error => return Err(anyhow!("denied")),
            }
            Ok(ToolCallOutcome::Continue)
        }

        async fn on_tool_result(
            &self,
            result: &mut ToolResultMessage,
            _: &SessionContext,
        ) -> Result<()> {
            result.content = vec![MessageContent::text("changed")];
            result.details = Some(serde_json::json!({"changed": true}));
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for ToolHooks {
        type Error = Infallible;

        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            vec![crate::message::ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                input: ToolInput::Json(serde_json::json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"],
                    "additionalProperties": false
                })),
            }]
        }

        async fn call(
            &self,
            _: SessionId,
            _: &str,
            input: String,
        ) -> Result<ToolOutput, Self::Error> {
            self.calls.lock().unwrap().push(input);
            Ok(ToolOutput {
                content: vec![MessageContent::text("raw")],
                details: Some(serde_json::json!({"raw": true})),
            })
        }

        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for ToolHooks {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    struct NoTools;
    impl AgentHooks for NoTools {}

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for NoTools {
        type Error = Infallible;
        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            Vec::new()
        }
        async fn call(&self, _: SessionId, _: &str, _: String) -> Result<ToolOutput, Self::Error> {
            unreachable!()
        }
        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for NoTools {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn before_agent_and_context_hooks_feed_the_provider() {
        let directory = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(SeenContext::default()));
        let mut session = Session::open(
            RecordingProvider(Arc::clone(&seen)),
            HookTools(HookMode::BeforeAndContext),
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(seen.system_prompt.as_deref(), Some("hook system"));
        assert_eq!(seen.messages.len(), 3);
        assert!(
            matches!(&seen.messages[1], Message::User(message) if message_text(message) == "injected")
        );
        assert!(
            matches!(&seen.messages[2], Message::User(message) if message_text(message) == "context")
        );
        assert_eq!(SessionView::messages(&session).len(), 3);
    }

    #[tokio::test]
    async fn failed_context_hook_discards_its_working_copy() {
        let directory = tempfile::tempdir().unwrap();
        let seen = Arc::new(Mutex::new(SeenContext::default()));
        let mut session = Session::open(
            RecordingProvider(Arc::clone(&seen)),
            HookTools(HookMode::ContextError),
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        assert_eq!(seen.lock().unwrap().messages.len(), 1);
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(
                |event| matches!(event, AgentEvent::HookError { hook, .. } if hook == "context")
            )
        );
    }

    #[tokio::test]
    async fn invalid_final_message_role_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            FakeProvider,
            HookTools(HookMode::InvalidFinal),
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        let persisted = SessionView::messages(&session);
        assert!(
            matches!(&persisted[1], Message::Assistant(message) if message_text_content(&message.content) == "Hello")
        );
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(
            |event| matches!(event, AgentEvent::HookError { hook, .. } if hook == "message_finalizing")
        ));
    }

    #[tokio::test]
    async fn valid_final_message_replacement_is_persisted_and_published() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            FakeProvider,
            HookTools(HookMode::ValidFinal),
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        let persisted = SessionView::messages(&session);
        assert!(
            matches!(&persisted[1], Message::Assistant(message) if message_text_content(&message.content) == "final replacement")
        );
        assert!(
            std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
                event,
                AgentEvent::MessageEnd {
                    message: Message::Assistant(message),
                    ..
                } if message_text_content(&message.content) == "final replacement"
            ))
        );
    }

    #[tokio::test]
    async fn final_hook_cannot_erase_provider_error_or_abort() {
        let directory = tempfile::tempdir().unwrap();
        let mut error_session = Session::open(
            ErrorProvider,
            HookTools(HookMode::EraseTerminal),
            ".",
            "fake",
            "system",
            Some(directory.path().join("error.jsonl")),
        )
        .unwrap();
        error_session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();
        assert!(matches!(
            &SessionView::messages(&error_session)[1],
            Message::Assistant(message)
                if message.stop_reason == StopReason::Error
                    && message.error_message.as_deref() == Some("provider failed")
        ));

        let mut aborted_session = Session::open(
            AbortedProvider,
            HookTools(HookMode::EraseTerminal),
            ".",
            "fake",
            "system",
            Some(directory.path().join("aborted.jsonl")),
        )
        .unwrap();
        aborted_session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();
        assert!(matches!(
            &SessionView::messages(&aborted_session)[1],
            Message::Assistant(message) if message.stop_reason == StopReason::Aborted
        ));
    }

    #[tokio::test]
    async fn provider_error_persists_partial_output_and_closes_normally() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            ErrorProvider,
            NoTools,
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        let persisted = SessionView::messages(&session);
        let Message::Assistant(assistant) = &persisted[1] else {
            panic!("expected assistant message");
        };
        assert_eq!(assistant.stop_reason, StopReason::Error);
        assert_eq!(assistant.error_message.as_deref(), Some("provider failed"));
        assert_eq!(message_text_content(&assistant.content), "partial");
        let names = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event_name(&event))
            .collect::<Vec<_>>();
        assert_eq!(
            &names[names.len() - 4..],
            ["message_end", "turn_end", "agent_end", "agent_settled"]
        );
    }

    #[tokio::test]
    async fn tool_loop_mutates_call_and_result_across_two_turns() {
        let directory = tempfile::tempdir().unwrap();
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::open(
            ToolProvider {
                calls: AtomicUsize::new(0),
                contexts: Arc::clone(&contexts),
            },
            ToolHooks {
                calls: Arc::clone(&calls),
                mode: ToolHookMode::Valid,
            },
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("hello"))
            .await
            .unwrap();

        assert_eq!(calls.lock().unwrap().as_slice(), [r#"{"value":"mutated"}"#]);
        let persisted = SessionView::messages(&session);
        assert_eq!(persisted.len(), 4);
        assert!(
            matches!(&persisted[2], Message::ToolResult(result) if message_text_content(&result.content) == "changed")
        );
        let contexts = contexts.lock().unwrap();
        assert_eq!(contexts.len(), 2);
        assert!(
            matches!(&contexts[1][2], Message::ToolResult(result) if message_text_content(&result.content) == "changed")
        );

        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert_eq!(
            events.iter().map(event_name).collect::<Vec<_>>(),
            [
                "session_start",
                "agent_start",
                "turn_start",
                "message_start",
                "message_end",
                "message_start",
                "message_update",
                "message_end",
                "tool_execution_start",
                "tool_execution_end",
                "message_start",
                "message_end",
                "turn_end",
                "turn_start",
                "message_start",
                "message_update",
                "message_end",
                "turn_end",
                "agent_end",
                "agent_settled",
            ]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::AgentStart { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::TurnStart { .. }))
                .count(),
            2
        );
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolExecutionEnd { result, .. } if result == &serde_json::json!({"changed": true})
        )));
    }

    #[tokio::test]
    async fn invalid_or_failing_tool_hooks_never_reach_the_executor() {
        for mode in [ToolHookMode::Invalid, ToolHookMode::Error] {
            let directory = tempfile::tempdir().unwrap();
            let calls = Arc::new(Mutex::new(Vec::new()));
            let mut session = Session::open(
                ToolProvider {
                    calls: AtomicUsize::new(0),
                    contexts: Arc::new(Mutex::new(Vec::new())),
                },
                ToolHooks {
                    calls: Arc::clone(&calls),
                    mode,
                },
                ".",
                "fake",
                "system",
                Some(directory.path().join("session.jsonl")),
            )
            .unwrap();
            let mut events = session.subscribe();

            session
                .run_one_trun(UserMessage::text("hello"))
                .await
                .unwrap();

            assert!(calls.lock().unwrap().is_empty());
            assert!(
                SessionView::messages(&session).iter().any(
                    |message| matches!(message, Message::ToolResult(result) if result.is_error)
                )
            );
            assert!(std::iter::from_fn(|| events.try_recv().ok()).any(
                |event| matches!(event, AgentEvent::HookError { hook, .. } if hook == "tool_call")
            ));
        }
    }

    struct HandledInput;

    #[async_trait::async_trait(?Send)]
    impl AgentHooks for HandledInput {
        async fn on_input(&self, _: &mut UserMessage, _: &SessionContext) -> Result<InputOutcome> {
            Ok(InputOutcome::Handled)
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for HandledInput {
        type Error = Infallible;
        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            Vec::new()
        }
        async fn call(&self, _: SessionId, _: &str, _: String) -> Result<ToolOutput, Self::Error> {
            unreachable!()
        }
        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for HandledInput {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn handled_input_does_not_start_an_agent_or_persist_messages() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            FakeProvider,
            HandledInput,
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut events = session.subscribe();

        session
            .run_one_trun(UserMessage::text("handled"))
            .await
            .unwrap();

        let names = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| event_name(&event))
            .collect::<Vec<_>>();
        assert_eq!(names, ["session_start"]);
        assert!(SessionView::messages(&session).is_empty());
    }

    #[tokio::test]
    async fn streams_and_publishes_store_first_terminal_messages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let mut session = Session::open(
            FakeProvider,
            NoTools,
            PathBuf::from("."),
            "fake",
            "system",
            Some(path),
        )
        .unwrap();
        let mut events = session.subscribe();

        session.run_one_trun(UserMessage::text("hi")).await.unwrap();
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        let names = events.iter().map(event_name).collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "session_start",
                "agent_start",
                "turn_start",
                "message_start",
                "message_end",
                "message_start",
                "message_update",
                "message_update",
                "message_end",
                "turn_end",
                "agent_end",
                "agent_settled",
            ]
        );
        assert_eq!(SessionView::messages(&session).len(), 2);
        let AgentEvent::MessageEnd {
            message: Message::Assistant(message),
            ..
        } = &events[8]
        else {
            panic!("expected authoritative assistant message");
        };
        assert_eq!(message.content.len(), 1);
        assert!(matches!(&message.content[0], MessageContent::Text { text } if text == "Hello"));
    }

    struct SlowProvider;

    #[async_trait::async_trait]
    impl Provider for SlowProvider {
        type Error = Infallible;

        async fn stream(
            &self,
            _model: &str,
            _context: crate::message::Context<'_>,
        ) -> Result<ProviderStream<Self::Error>, Self::Error> {
            Ok(Box::pin(
                futures_util::stream::iter([Ok(ProviderEvent::ContentDelta {
                    block_index: 0,
                    delta: MessageContent::text("partial"),
                })])
                .chain(futures_util::stream::pending()),
            ))
        }
    }

    #[tokio::test]
    async fn attachment_snapshots_history_before_buffering_new_events() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let id = SessionId::next();
                let path = PathBuf::from("restored.jsonl");
                let history = vec![
                    Message::User(UserMessage::text("old user")),
                    Message::Assistant(AssistantMessage {
                        content: vec![MessageContent::text("old assistant")],
                        stop_reason: StopReason::Stop,
                        usage: None,
                        error_message: None,
                    }),
                ];
                let store = FailWriteStore {
                    id,
                    path: path.clone(),
                    messages: history.clone(),
                    entries: history
                        .iter()
                        .cloned()
                        .map(|message| SessionEntry::Message { message })
                        .collect(),
                    fail_at: usize::MAX,
                };
                let session =
                    Session::with_store(FakeProvider, NoTools, ".", "fake", "system", store);

                let attachment = session.attach();
                assert_eq!(attachment.metadata.session_id, id.to_string());
                assert_eq!(attachment.metadata.path, path);
                assert_eq!(attachment.metadata.cwd, PathBuf::from("."));
                assert_eq!(attachment.messages.len(), 2);
                assert!(matches!(
                    &attachment.messages[0],
                    Message::User(message) if message_text(message) == "old user"
                ));
                assert_eq!(attachment.status, SessionStatus::Idle);

                attachment
                    .handle
                    .prompt(UserMessage::text("new user"))
                    .await
                    .unwrap();

                let mut events = attachment.events;
                let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
                assert!(matches!(
                    events.first(),
                    Some(AgentEvent::SessionStart { .. })
                ));
                let ended = events
                    .iter()
                    .filter_map(|event| match event {
                        AgentEvent::MessageEnd { message, .. } => Some(message),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                assert_eq!(ended.len(), 2);
                assert!(matches!(
                    ended[0],
                    Message::User(message) if message_text(message) == "new user"
                ));
                assert!(matches!(
                    ended[1],
                    Message::Assistant(message) if message_text_content(&message.content) == "Hello"
                ));

                attachment.handle.close().await.unwrap();
            })
            .await;
    }

    #[tokio::test]
    async fn attachment_abort_wakes_stream_and_persists_partial() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let session = Session::open(
                    SlowProvider,
                    NoTools,
                    ".",
                    "fake",
                    "system",
                    Some(directory.path().join("session.jsonl")),
                )
                .unwrap();
                let attachment = session.attach();
                assert!(attachment.messages.is_empty());
                assert_eq!(attachment.status, SessionStatus::Idle);
                let mut events = attachment.events;
                let prompt_handle = attachment.handle.clone();
                let prompt = tokio::task::spawn_local(async move {
                    prompt_handle.prompt(UserMessage::text("hi")).await
                });

                loop {
                    if matches!(events.recv().await.unwrap(), AgentEvent::MessageUpdate { .. }) {
                        break;
                    }
                }
                attachment.handle.abort().await.unwrap();
                prompt.await.unwrap().unwrap();

                let mut aborted = None;
                while let Ok(event) = events.try_recv() {
                    if let AgentEvent::MessageEnd {
                        message: Message::Assistant(message),
                        ..
                    } = event
                    {
                        aborted = Some(message);
                    }
                }
                let aborted = aborted.expect("assistant terminal event");
                assert_eq!(aborted.stop_reason, StopReason::Aborted);
                assert!(matches!(&aborted.content[0], MessageContent::Text { text } if text == "partial"));
                attachment.handle.close().await.unwrap();
            })
            .await;
    }

    struct FailWriteStore {
        id: SessionId,
        path: PathBuf,
        messages: Vec<Message>,
        entries: Vec<SessionEntry>,
        fail_at: usize,
    }

    impl SessionStore for FailWriteStore {
        fn id(&self) -> SessionId {
            self.id
        }
        fn path(&self) -> &Path {
            &self.path
        }
        fn messages(&self) -> &[Message] {
            &self.messages
        }
        fn entries(&self) -> &[SessionEntry] {
            &self.entries
        }
        fn append_message(&mut self, message: Message) -> Result<()> {
            if self.messages.len() == self.fail_at {
                return Err(anyhow!("injected store failure"));
            }
            self.messages.push(message.clone());
            self.entries.push(SessionEntry::Message { message });
            Ok(())
        }
        fn append_custom(&mut self, kind: String, data: serde_json::Value) -> Result<()> {
            self.entries.push(SessionEntry::Custom {
                custom_type: kind,
                data,
            });
            Ok(())
        }
        fn save(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn store_failure_terminates_without_an_authoritative_message_end() {
        let store = FailWriteStore {
            id: SessionId::next(),
            path: PathBuf::from("injected.jsonl"),
            messages: Vec::new(),
            entries: Vec::new(),
            fail_at: 1,
        };
        let mut session = Session::with_store(FakeProvider, NoTools, ".", "fake", "system", store);
        let mut events = session.subscribe();

        assert!(session.run_one_trun(UserMessage::text("hi")).await.is_err());
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        let names = events.iter().map(event_name).collect::<Vec<_>>();
        assert_eq!(
            &names[names.len() - 3..],
            ["persistence_error", "session_fatal", "session_shutdown"]
        );
        assert_eq!(
            names.iter().filter(|name| **name == "message_end").count(),
            1
        );
        assert_eq!(SessionView::status(&session), SessionStatus::Fatal);
        assert!(
            session
                .run_one_trun(UserMessage::text("again"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn first_store_failure_closes_all_actor_input_paths() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let store = FailWriteStore {
                    id: SessionId::next(),
                    path: PathBuf::from("injected.jsonl"),
                    messages: Vec::new(),
                    entries: Vec::new(),
                    fail_at: 0,
                };
                let session =
                    Session::with_store(FakeProvider, NoTools, ".", "fake", "system", store);
                let attachment = session.attach();
                let mut events = attachment.events;

                assert!(
                    attachment
                        .handle
                        .prompt(UserMessage::text("fail"))
                        .await
                        .is_err()
                );
                assert!(
                    attachment
                        .handle
                        .prompt(UserMessage::text("again"))
                        .await
                        .is_err()
                );
                assert!(
                    attachment
                        .handle
                        .steer(UserMessage::text("steer"))
                        .await
                        .is_err()
                );
                assert!(
                    attachment
                        .handle
                        .follow_up(UserMessage::text("later"))
                        .await
                        .is_err()
                );

                let names = std::iter::from_fn(|| events.try_recv().ok())
                    .map(|event| event_name(&event))
                    .collect::<Vec<_>>();
                assert_eq!(
                    names,
                    [
                        "session_start",
                        "agent_start",
                        "turn_start",
                        "message_start",
                        "persistence_error",
                        "session_fatal",
                        "session_shutdown",
                    ]
                );
            })
            .await;
    }

    #[tokio::test]
    async fn tool_result_store_failure_stops_before_the_next_turn() {
        let store = FailWriteStore {
            id: SessionId::next(),
            path: PathBuf::from("injected.jsonl"),
            messages: Vec::new(),
            entries: Vec::new(),
            fail_at: 2,
        };
        let contexts = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut session = Session::with_store(
            ToolProvider {
                calls: AtomicUsize::new(0),
                contexts: Arc::clone(&contexts),
            },
            ToolHooks {
                calls,
                mode: ToolHookMode::Valid,
            },
            ".",
            "fake",
            "system",
            store,
        );
        let mut events = session.subscribe();

        assert!(session.run_one_trun(UserMessage::text("hi")).await.is_err());

        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        let names = events.iter().map(event_name).collect::<Vec<_>>();
        assert_eq!(
            &names[names.len() - 4..],
            [
                "message_start",
                "persistence_error",
                "session_fatal",
                "session_shutdown",
            ]
        );
        assert_eq!(
            names.iter().filter(|name| **name == "message_end").count(),
            2
        );
        assert_eq!(
            names.iter().filter(|name| **name == "turn_start").count(),
            1
        );
        assert_eq!(contexts.lock().unwrap().len(), 1);
        assert_eq!(SessionView::messages(&session).len(), 2);
    }

    struct DropCountingTools(Arc<AtomicUsize>);

    impl AgentHooks for DropCountingTools {}

    #[async_trait::async_trait(?Send)]
    impl ToolExecutor for DropCountingTools {
        type Error = Infallible;
        fn tool_defs(&self) -> Vec<crate::message::ToolDef> {
            Vec::new()
        }
        async fn call(&self, _: SessionId, _: &str, _: String) -> Result<ToolOutput, Self::Error> {
            unreachable!()
        }
        async fn drop_session(&self, _: SessionId) -> Result<(), Self::Error> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait::async_trait(?Send)]
    impl ExtensionHost for DropCountingTools {
        fn commands(&self) -> Vec<CommandDef> {
            Vec::new()
        }
        async fn command(&self, _: &str, _: &str, _: &SessionContext) -> Result<()> {
            Ok(())
        }
        fn take_host_actions(&self) -> Vec<HostAction> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn normal_and_fatal_shutdown_are_idempotent() {
        let normal_drops = Arc::new(AtomicUsize::new(0));
        let directory = tempfile::tempdir().unwrap();
        let mut normal = Session::open(
            FakeProvider,
            DropCountingTools(Arc::clone(&normal_drops)),
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut normal_events = normal.subscribe();
        normal.close().await.unwrap();
        normal.close().await.unwrap();
        assert_eq!(normal_drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::iter::from_fn(|| normal_events.try_recv().ok())
                .filter(|event| matches!(event, AgentEvent::SessionShutdown))
                .count(),
            1
        );

        let fatal_drops = Arc::new(AtomicUsize::new(0));
        let store = FailWriteStore {
            id: SessionId::next(),
            path: PathBuf::from("injected.jsonl"),
            messages: Vec::new(),
            entries: Vec::new(),
            fail_at: 0,
        };
        let mut fatal = Session::with_store(
            FakeProvider,
            DropCountingTools(Arc::clone(&fatal_drops)),
            ".",
            "fake",
            "system",
            store,
        );
        let mut fatal_events = fatal.subscribe();
        assert!(fatal.run_one_trun(UserMessage::text("fail")).await.is_err());
        fatal.close().await.unwrap();
        assert_eq!(fatal_drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::iter::from_fn(|| fatal_events.try_recv().ok())
                .filter(|event| matches!(event, AgentEvent::SessionShutdown))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn broadcast_receivers_share_order_and_surface_lag() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::open(
            FakeProvider,
            NoTools,
            ".",
            "fake",
            "system",
            Some(directory.path().join("session.jsonl")),
        )
        .unwrap();
        let mut tui = session.subscribe();
        let mut jsonl = session.subscribe();

        session.run_one_trun(UserMessage::text("hi")).await.unwrap();

        let tui_events = std::iter::from_fn(|| tui.try_recv().ok())
            .map(|event| event_name(&event))
            .collect::<Vec<_>>();
        let jsonl_events = std::iter::from_fn(|| jsonl.try_recv().ok())
            .map(|event| event_name(&event))
            .collect::<Vec<_>>();
        assert_eq!(tui_events, jsonl_events);

        let bus = EventBus::default();
        let mut lagged = bus.subscribe();
        for run_id in 0..=crate::event::EVENT_BUS_CAPACITY {
            bus.publish(AgentEvent::AgentStart { run_id });
        }
        assert!(matches!(
            lagged.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
        ));

        let no_receivers = EventBus::default();
        no_receivers.publish(AgentEvent::SessionShutdown);
    }

    fn event_name(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::SessionStart { .. } => "session_start",
            AgentEvent::QueueUpdate { .. } => "queue_update",
            AgentEvent::AgentStart { .. } => "agent_start",
            AgentEvent::TurnStart { .. } => "turn_start",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageUpdate { .. } => "message_update",
            AgentEvent::MessageEnd { .. } => "message_end",
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
            AgentEvent::TurnEnd { .. } => "turn_end",
            AgentEvent::AgentEnd { .. } => "agent_end",
            AgentEvent::AgentSettled { .. } => "agent_settled",
            AgentEvent::HookError { .. } => "hook_error",
            AgentEvent::PersistenceError { .. } => "persistence_error",
            AgentEvent::SessionFatal { .. } => "session_fatal",
            AgentEvent::SessionShutdown => "session_shutdown",
        }
    }
}
