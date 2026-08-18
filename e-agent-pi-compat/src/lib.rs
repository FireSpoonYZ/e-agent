mod ansi;
mod renderers;
mod ui;

use std::{
    collections::{BTreeSet, HashMap},
    ffi::c_void,
    mem::ManuallyDrop,
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use base64::Engine;
use e_agent_extension::{AbiBuffer, EXTENSION_ABI_VERSION, ExtensionV1, SessionId, ToolExtension};
use e_agent_node_runtime::{
    ExtensionToolDef, HostcallKind, HostcallOutcome, HostcallRequest, NativeCall, NativeFunction,
    NativeModule, PiJsRuntime, PiJsRuntimeConfig, ProgramOutput, WallClock,
    execute_program_with_hostcalls,
};
use libloading::Library;
use serde::Serialize;
pub use ui::{PI_UI_TARGET, PiUiConfig, pi_operation_support};

use e_agent_core::{
    event::AgentEvent,
    hooks::{AgentHooks, BeforeAgentStart, InputOutcome, ToolCall, ToolCallOutcome},
    message::{Message, MessageContent, ToolDef, ToolInput, ToolResultMessage, UserMessage},
    session::SessionContext,
    tool::{
        ToolExecutor, ToolOutput,
        extension::{CommandDef, ExtensionHost, HostAction},
    },
};

#[derive(Debug, Clone, Serialize)]
pub struct PTCOutput {
    stdout: String,
    stderr: String,
}

static NODE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct LoadedExtension {
    metadata: ToolExtension,
    abi: ExtensionV1,
    // Extension runtimes are process-scoped, matching the previous execution
    // lifecycle, so their code must remain loaded.
    _library: ManuallyDrop<Library>,
}

#[derive(Debug, Clone, Serialize)]
struct PtcToolCatalog {
    module: String,
    source: &'static str,
    functions: Vec<PtcFunctionCatalog>,
}

#[derive(Debug, Clone, Serialize)]
struct PtcFunctionCatalog {
    name: String,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
}

struct JsExtension {
    module: String,
    extension_id: String,
    tools: Vec<ExtensionToolDef>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct JsCommandDef {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct JsShortcutDef {
    extension_id: String,
    shortcut: String,
}

struct HostcallState {
    actions: std::sync::Mutex<Vec<HostAction>>,
}

impl HostcallState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            actions: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn take(&self) -> Vec<HostAction> {
        std::mem::take(
            &mut *self
                .actions
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

enum JsRuntimeCommand {
    Load {
        extension_id: String,
        path: std::path::PathBuf,
        reply: tokio::sync::oneshot::Sender<Result<Vec<ExtensionToolDef>>>,
    },
    Execute {
        extension_id: String,
        tool: String,
        call_id: String,
        input: serde_json::Value,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    GetCommands {
        reply: tokio::sync::oneshot::Sender<Result<Vec<JsCommandDef>>>,
    },
    GetShortcuts {
        reply: tokio::sync::oneshot::Sender<Result<Vec<JsShortcutDef>>>,
    },
    ExecuteCommand {
        name: String,
        args: String,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    DispatchEvent {
        name: String,
        payload: serde_json::Value,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    ExecuteShortcut {
        shortcut: String,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    RenderTool {
        extension_id: String,
        tool: String,
        slot: String,
        payload: serde_json::Value,
        context: serde_json::Value,
        width: u16,
        reply: tokio::sync::oneshot::Sender<Result<Option<Vec<String>>>>,
    },
    Shutdown,
}

#[derive(Clone)]
struct JsRuntimeHandle {
    sender: tokio::sync::mpsc::UnboundedSender<JsRuntimeCommand>,
}

impl JsRuntimeHandle {
    fn start(ui: PiUiConfig) -> Result<Self> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("e-agent-pi-extension".into())
            .spawn(move || {
                let local = tokio::task::LocalSet::new();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(host) => host.block_on(local.run_until(async move {
                        let mut config = PiJsRuntimeConfig::default();
                        config.limits.hostcall_timeout_ms = Some(5_000);
                        config.cwd = std::env::current_dir()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let runtime =
                            match PiJsRuntime::with_clock_and_config(WallClock, config).await {
                                Ok(runtime) => {
                                    let _ = ready_tx.send(Ok(()));
                                    runtime
                                }
                                Err(error) => {
                                    let _ = ready_tx.send(Err(error.to_string()));
                                    return;
                                }
                            };
                        let mut ui_input = ui.subscribe_input();
                        let input_hostcalls = HostcallState::new();
                        loop {
                            let command = if let Some(input) = ui_input.as_mut() {
                                tokio::select! {
                                    command = receiver.recv() => command,
                                    event = input.recv() => {
                                        if let Ok(event) = event
                                            && let Some(data) = crate::ui::pi_input_data(&event)
                                        {
                                            let input_ui = ui.clone();
                                            let hostcalls = Arc::clone(&input_hostcalls);
                                            let _ = runtime
                                                .dispatch_terminal_input_with_hostcalls(&data, move |request| {
                                                    let input_ui = input_ui.clone();
                                                    let hostcalls = Arc::clone(&hostcalls);
                                                    async move {
                                                        execute_hostcall_with_state(request, hostcalls, &input_ui).await
                                                    }
                                                })
                                                .await;
                                        }
                                        continue;
                                    }
                                }
                            } else {
                                receiver.recv().await
                            };
                            let Some(command) = command else { break };
                            match command {
                                JsRuntimeCommand::Load {
                                    extension_id,
                                    path,
                                    reply,
                                } => {
                                    let result: Result<Vec<ExtensionToolDef>> = async {
                                        runtime
                                            .load_extension_with_hostcalls(
                                                &extension_id,
                                                &path,
                                                execute_hostcall,
                                            )
                                            .await
                                            .map_err(anyhow::Error::new)?;
                                        Ok(runtime
                                            .get_registered_tools()
                                            .await
                                            .map_err(anyhow::Error::new)?
                                            .into_iter()
                                            .filter(|tool| tool.extension_id == extension_id)
                                            .collect())
                                    }
                                    .await;
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::Execute {
                                    extension_id,
                                    tool,
                                    call_id,
                                    input,
                                    context,
                                    hostcalls,
                                    reply,
                                } => {
                                    let ui = ui.clone();
                                    let result = runtime
                                        .execute_extension_tool_with_hostcalls(
                                            &extension_id,
                                            &tool,
                                            &call_id,
                                            input,
                                            context,
                                            move |request| {
                                                let ui = ui.clone();
                                                let hostcalls = Arc::clone(&hostcalls);
                                                async move {
                                                    execute_hostcall_with_state(
                                                        request, hostcalls, &ui,
                                                    )
                                                    .await
                                                }
                                            },
                                        )
                                        .await
                                        .map_err(anyhow::Error::new);
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::GetCommands { reply } => {
                                    let result = runtime
                                        .get_registered_commands()
                                        .await
                                        .map_err(anyhow::Error::new)
                                        .and_then(|value| {
                                            value
                                                .into_iter()
                                                .map(|item| {
                                                    serde_json::from_value(item)
                                                        .map_err(anyhow::Error::new)
                                                })
                                                .collect()
                                        });
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::GetShortcuts { reply } => {
                                    let result = runtime
                                        .get_registered_shortcuts()
                                        .await
                                        .map_err(anyhow::Error::new)
                                        .and_then(|value| {
                                            value
                                                .into_iter()
                                                .map(|item| {
                                                    serde_json::from_value(item)
                                                        .map_err(anyhow::Error::new)
                                                })
                                                .collect()
                                        });
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::ExecuteCommand {
                                    name,
                                    args,
                                    context,
                                    hostcalls,
                                    reply,
                                } => {
                                    let ui = ui.clone();
                                    let result = runtime
                                        .execute_extension_command_with_hostcalls(
                                            &name,
                                            &args,
                                            context,
                                            move |request| {
                                                let ui = ui.clone();
                                                let hostcalls = Arc::clone(&hostcalls);
                                                async move {
                                                    execute_hostcall_with_state(
                                                        request, hostcalls, &ui,
                                                    )
                                                    .await
                                                }
                                            },
                                        )
                                        .await
                                        .map_err(anyhow::Error::new);
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::DispatchEvent {
                                    name,
                                    payload,
                                    context,
                                    hostcalls,
                                    reply,
                                } => {
                                    let ui = ui.clone();
                                    let result = runtime
                                        .dispatch_extension_event_with_hostcalls(
                                            &name,
                                            payload,
                                            context,
                                            move |request| {
                                                let ui = ui.clone();
                                                let hostcalls = Arc::clone(&hostcalls);
                                                async move {
                                                    execute_hostcall_with_state(
                                                        request, hostcalls, &ui,
                                                    )
                                                    .await
                                                }
                                            },
                                        )
                                        .await
                                        .map_err(anyhow::Error::new);
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::ExecuteShortcut {
                                    shortcut,
                                    context,
                                    hostcalls,
                                    reply,
                                } => {
                                    let ui = ui.clone();
                                    let result = runtime
                                        .execute_extension_shortcut_with_hostcalls(
                                            &shortcut,
                                            context,
                                            move |request| {
                                                let ui = ui.clone();
                                                let hostcalls = Arc::clone(&hostcalls);
                                                async move {
                                                    execute_hostcall_with_state(
                                                        request, hostcalls, &ui,
                                                    )
                                                    .await
                                                }
                                            },
                                        )
                                        .await
                                        .map_err(anyhow::Error::new);
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::RenderTool {
                                    extension_id,
                                    tool,
                                    slot,
                                    payload,
                                    context,
                                    width,
                                    reply,
                                } => {
                                    let result = runtime
                                        .render_extension_tool_component(
                                            &extension_id,
                                            &tool,
                                            &slot,
                                            payload,
                                            context,
                                            width,
                                        )
                                        .await
                                        .map_err(anyhow::Error::new);
                                    let _ = reply.send(result);
                                }
                                JsRuntimeCommand::Shutdown => break,
                            }
                        }
                    })),
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                    }
                }
            })?;
        ready_rx
            .recv()
            .context("Pi extension runtime startup failed")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self { sender })
    }

    async fn load(
        &self,
        extension_id: String,
        path: std::path::PathBuf,
    ) -> Result<Vec<ExtensionToolDef>> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::Load {
                extension_id,
                path,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn execute(
        &self,
        extension_id: String,
        tool: String,
        call_id: String,
        input: serde_json::Value,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
    ) -> Result<serde_json::Value> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::Execute {
                extension_id,
                tool,
                call_id,
                input,
                context,
                hostcalls,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn get_commands(&self) -> Result<Vec<JsCommandDef>> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::GetCommands { reply })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn command(
        &self,
        name: String,
        args: String,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
    ) -> Result<serde_json::Value> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::ExecuteCommand {
                name,
                args,
                context,
                hostcalls,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn dispatch(
        &self,
        name: String,
        payload: serde_json::Value,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
    ) -> Result<serde_json::Value> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::DispatchEvent {
                name,
                payload,
                context,
                hostcalls,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn shortcuts(&self) -> Result<Vec<JsShortcutDef>> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::GetShortcuts { reply })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn shortcut(
        &self,
        shortcut: String,
        context: serde_json::Value,
        hostcalls: Arc<HostcallState>,
    ) -> Result<serde_json::Value> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::ExecuteShortcut {
                shortcut,
                context,
                hostcalls,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    async fn render_tool(
        &self,
        extension_id: String,
        tool: String,
        slot: String,
        payload: serde_json::Value,
        context: serde_json::Value,
        width: u16,
    ) -> Result<Option<Vec<String>>> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::RenderTool {
                extension_id,
                tool,
                slot,
                payload,
                context,
                width,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    fn shutdown(&self) {
        let _ = self.sender.send(JsRuntimeCommand::Shutdown);
    }
}

pub struct PiCompat {
    extensions: Vec<Arc<LoadedExtension>>,
    js_runtime: Option<JsRuntimeHandle>,
    js_extensions: Vec<JsExtension>,
    js_commands: Vec<CommandDef>,
    host_actions: Arc<HostcallState>,
    closed: std::sync::atomic::AtomicBool,
    call_sequence: std::sync::atomic::AtomicU64,
    ui: PiUiConfig,
    tool_calls: std::sync::Mutex<HashMap<String, (String, String)>>,
}

impl Default for PiCompat {
    fn default() -> Self {
        Self {
            extensions: Vec::new(),
            js_runtime: None,
            js_extensions: Vec::new(),
            js_commands: Vec::new(),
            host_actions: HostcallState::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
            call_sequence: std::sync::atomic::AtomicU64::new(0),
            ui: PiUiConfig::headless(),
            tool_calls: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ToolExecutor for PiCompat {
    type Error = anyhow::Error;

    fn tool_defs(&self) -> Vec<ToolDef> {
        let mut definitions = vec![ToolDef {
            name: "node".into(),
            description: format!(
                "Execute one complete TypeScript ES module in the Node-compatible QuickJS runtime. Pass the complete program in the `code` field. Import only modules listed in the catalog, using static top-level imports such as `import {{ web_search }} from \"web_access\"`. Never import `pi-extension`, use dynamic `import()`, or use `require()` for catalog modules. Call every async function with one object argument and await it. Rust tools additionally retain legacy positional compatibility. The output_schema is authoritative. The unified module catalog follows:\n{}",
                serde_json::to_string_pretty(&self.catalog())
                    .expect("tool metadata must serialize")
            ),
            input: ToolInput::Json(serde_json::json!({
                "type": "object",
                "properties": { "code": { "type": "string" } },
                "required": ["code"],
                "additionalProperties": false
            })),
        }];
        let mut names = BTreeSet::from(["node"]);
        for extension in &self.js_extensions {
            for tool in &extension.tools {
                if names.insert(tool.name.as_str()) {
                    definitions.push(ToolDef {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        input: ToolInput::Json(tool.parameters.clone()),
                    });
                }
            }
        }
        definitions
    }

    async fn call(
        &self,
        session: SessionId,
        name: &str,
        code: String,
    ) -> Result<ToolOutput, Self::Error> {
        if name != "node"
            && let Some((extension_id, tool)) = self.js_extensions.iter().find_map(|extension| {
                extension
                    .tools
                    .iter()
                    .find(|tool| tool.name == name)
                    .map(|tool| (extension.extension_id.clone(), tool.name.clone()))
            })
        {
            let runtime = self
                .js_runtime
                .as_ref()
                .context("Pi extension runtime unavailable")?;
            let input =
                serde_json::from_str(&code).unwrap_or_else(|_| serde_json::Value::String(code));
            let call_id = format!(
                "direct-{}-{}",
                session.0,
                self.call_sequence
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            let value = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                runtime.execute(
                    extension_id,
                    tool,
                    call_id,
                    input,
                    serde_json::json!({
                        "cwd": std::env::current_dir()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        "mode": self.ui.mode(),
                        "hasUI": self.ui.has_ui(),
                        "sessionId": session.0.to_string(),
                    }),
                    Arc::clone(&self.host_actions),
                ),
            )
            .await
            .context("Pi extension tool timed out")??;
            return serde_json::from_value(value).context("invalid Pi extension tool result");
        }

        let code = serde_json::from_str::<serde_json::Value>(&code)
            .ok()
            .and_then(|value| value["code"].as_str().map(ToOwned::to_owned))
            .unwrap_or(code);
        let output = self.execute(session, &code).await?;
        let text = match (output.stdout.is_empty(), output.stderr.is_empty()) {
            (true, true) => serde_json::to_string(&output)?,
            (false, true) => format!("stdout:\n{}", output.stdout),
            (true, false) => format!("stderr:\n{}", output.stderr),
            (false, false) => format!("stdout:\n{}\nstderr:\n{}", output.stdout, output.stderr),
        };
        Ok(ToolOutput {
            content: vec![MessageContent::text(text)],
            details: None,
        })
    }

    fn system_prompts(&self) -> Vec<String> {
        let mut prompts = self
            .extensions
            .iter()
            .map(|extension| extension.metadata.system_prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty())
            .collect::<Vec<_>>();
        prompts.shrink_to_fit();
        prompts
    }

    async fn drop_session(&self, session: SessionId) -> Result<(), Self::Error> {
        for extension in &self.extensions {
            unsafe { (extension.abi.drop_session)(session.0) };
        }
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(runtime) = &self.js_runtime {
            runtime.shutdown();
        }
        Ok(())
    }
}

impl PiCompat {
    pub fn with_ui(ui: PiUiConfig) -> Self {
        Self {
            ui,
            ..Self::default()
        }
    }

    fn pi_context(&self, ctx: &SessionContext) -> serde_json::Value {
        serde_json::json!({
            "cwd": ctx.cwd.to_string_lossy(), "mode": self.ui.mode(), "hasUI": self.ui.has_ui(),
            "sessionId": ctx.session_id.to_string(), "sessionEntries": ctx.entries,
            "sessionBranch": ctx.entries, "isIdle": ctx.idle, "hasPendingMessages": !ctx.idle,
        })
    }

    fn pi_event(event: &AgentEvent) -> Option<(&'static str, serde_json::Value)> {
        Some(match event {
            AgentEvent::SessionStart { session_id } => (
                "session_start",
                serde_json::json!({"reason":"startup","sessionId":session_id}),
            ),
            AgentEvent::AgentStart { run_id } => {
                ("agent_start", serde_json::json!({"runId":run_id}))
            }
            AgentEvent::TurnStart { run_id, turn_index } => (
                "turn_start",
                serde_json::json!({"runId":run_id,"turnIndex":turn_index}),
            ),
            AgentEvent::MessageStart {
                message_id,
                message,
            } => (
                "message_start",
                serde_json::json!({"messageId":message_id,"message":message}),
            ),
            AgentEvent::MessageUpdate {
                message_id,
                delta,
                usage,
                ..
            } => (
                "message_update",
                serde_json::json!({"messageId":message_id,"delta":delta,"usage":usage}),
            ),
            AgentEvent::MessageEnd {
                message_id,
                message,
            } => (
                "message_end",
                serde_json::json!({"messageId":message_id,"message":message}),
            ),
            AgentEvent::ToolExecutionStart { id, name, input } => (
                "tool_execution_start",
                serde_json::json!({"toolCallId":id,"toolName":name,"input":input}),
            ),
            AgentEvent::ToolExecutionUpdate { id, update } => (
                "tool_execution_update",
                serde_json::json!({"toolCallId":id,"partialResult":update}),
            ),
            AgentEvent::ToolExecutionEnd {
                id,
                name,
                result,
                is_error,
            } => (
                "tool_execution_end",
                serde_json::json!({"toolCallId":id,"toolName":name,"result":result,"isError":is_error}),
            ),
            AgentEvent::TurnEnd { run_id, turn_index } => (
                "turn_end",
                serde_json::json!({"runId":run_id,"turnIndex":turn_index}),
            ),
            AgentEvent::AgentEnd { run_id } => ("agent_end", serde_json::json!({"runId":run_id})),
            AgentEvent::AgentSettled { run_id } => {
                ("agent_settled", serde_json::json!({"runId":run_id}))
            }
            AgentEvent::SessionShutdown => {
                ("session_shutdown", serde_json::json!({"reason":"quit"}))
            }
            AgentEvent::QueueUpdate { .. }
            | AgentEvent::HookError { .. }
            | AgentEvent::PersistenceError { .. }
            | AgentEvent::SessionFatal { .. } => return None,
        })
    }

    pub fn load_tool(&mut self, path: impl AsRef<Path>) -> Result<()> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("PTC executor is closed");
        }
        let path = path.as_ref().canonicalize().with_context(|| {
            format!("extension path does not exist: {}", path.as_ref().display())
        })?;
        let library = unsafe { Library::new(&path) }
            .with_context(|| format!("load extension {}", path.display()))?;
        let abi = unsafe {
            let entry: libloading::Symbol<'_, unsafe extern "C" fn() -> *const ExtensionV1> =
                library
                    .get(b"e_agent_extension_v1\0")
                    .context("missing e_agent_extension_v1")?;
            let pointer = entry();
            if pointer.is_null() {
                bail!("e_agent_extension_v1 returned null");
            }
            *pointer
        };
        if abi.abi_version != EXTENSION_ABI_VERSION {
            bail!(
                "unsupported extension ABI {}, expected {}",
                abi.abi_version,
                EXTENSION_ABI_VERSION
            );
        }
        let metadata = unsafe {
            let buffer = (abi.metadata)();
            let bytes = copy_buffer(buffer)?;
            (abi.free_buffer)(buffer);
            serde_json::from_slice::<ToolExtension>(&bytes).context("invalid extension metadata")?
        };
        validate_metadata(&metadata)?;

        let loaded = Arc::new(LoadedExtension {
            metadata,
            abi,
            _library: ManuallyDrop::new(library),
        });
        if self
            .extensions
            .iter()
            .any(|extension| extension.metadata.name == loaded.metadata.name)
            || self
                .js_extensions
                .iter()
                .any(|extension| extension.module == loaded.metadata.name)
        {
            bail!("duplicate PTC module {}", loaded.metadata.name);
        }
        self.extensions.push(loaded);
        Ok(())
    }

    pub async fn load_extension(&mut self, path: impl AsRef<Path>) -> Result<()> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("PTC executor is closed");
        }
        let path = path.as_ref().canonicalize().with_context(|| {
            format!("extension path does not exist: {}", path.as_ref().display())
        })?;
        let module = module_name_for_path(&path)?;
        if self
            .extensions
            .iter()
            .any(|extension| extension.metadata.name == module)
            || self
                .js_extensions
                .iter()
                .any(|extension| extension.module == module)
        {
            bail!("duplicate PTC module {module}");
        }
        let extension_id = format!("ptc/{module}");
        if self.js_runtime.is_none() {
            self.js_runtime = Some(JsRuntimeHandle::start(self.ui.clone())?);
        }
        let runtime = self.js_runtime.as_ref().expect("runtime initialized");
        let tools = runtime
            .load(extension_id.clone(), path.clone())
            .await
            .with_context(|| format!("load Pi extension {}", path.display()))?;
        validate_js_tools(&module, &tools)?;
        let commands = runtime.get_commands().await?;
        self.js_commands = commands
            .into_iter()
            .map(|command| CommandDef {
                name: command.name,
                description: command.description,
            })
            .collect();
        let shortcuts = runtime.shortcuts().await?;
        let bindings = shortcuts
            .into_iter()
            .filter(|shortcut| shortcut.extension_id == extension_id)
            .map(|shortcut| (shortcut.shortcut, format!("pi.shortcut.{extension_id}")))
            .collect::<Vec<_>>();
        if !bindings.is_empty() {
            let _ = self
                .ui
                .execute_hostcall(
                    &extension_id,
                    "setKeybindings",
                    serde_json::json!({"entries": bindings}),
                )
                .await;
        }
        self.js_extensions.push(JsExtension {
            module,
            extension_id,
            tools,
        });
        Ok(())
    }

    pub async fn shortcut(&self, shortcut: &str, ctx: &SessionContext) -> Result<bool> {
        let Some(runtime) = &self.js_runtime else {
            return Ok(false);
        };
        runtime
            .shortcut(
                shortcut.to_string(),
                self.pi_context(ctx),
                Arc::clone(&self.host_actions),
            )
            .await
            .with_context(|| format!("execute Pi shortcut {shortcut}"))?;
        Ok(true)
    }

    fn catalog(&self) -> Vec<PtcToolCatalog> {
        let rust = self.extensions.iter().map(|extension| PtcToolCatalog {
            module: extension.metadata.name.clone(),
            source: "rust",
            functions: extension
                .metadata
                .functions
                .iter()
                .map(|function| PtcFunctionCatalog {
                    name: function.name.clone(),
                    input_schema: function.schema.clone(),
                    output_schema: function.output_schema.clone(),
                })
                .collect(),
        });
        let js = self.js_extensions.iter().map(|extension| PtcToolCatalog {
            module: extension.module.clone(),
            source: "pi-extension",
            functions: extension
                .tools
                .iter()
                .map(|tool| PtcFunctionCatalog {
                    name: tool.name.clone(),
                    input_schema: tool.parameters.clone(),
                    output_schema: serde_json::json!({
                        "type": "object",
                        "properties": { "content": { "type": "array" }, "details": {} },
                        "required": ["content"]
                    }),
                })
                .collect(),
        });
        rust.chain(js).collect()
    }

    pub fn tools(&self) -> Vec<ToolExtension> {
        self.extensions
            .iter()
            .map(|extension| extension.metadata.clone())
            .collect()
    }

    pub fn set_cancelled(&self, cancelled: bool) {
        for extension in &self.extensions {
            unsafe { (extension.abi.set_cancelled)(cancelled) };
        }
    }

    async fn execute(&self, session: SessionId, code: &str) -> Result<PTCOutput> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            bail!("PTC executor is closed");
        }
        let _guard = NODE.lock().await;
        self.set_cancelled(false);

        let mut modules = self
            .extensions
            .iter()
            .map(|extension| NativeModule {
                name: extension.metadata.name.clone(),
                functions: extension
                    .metadata
                    .functions
                    .iter()
                    .map(|function| NativeFunction {
                        name: function.name.clone(),
                        parameters: function.parameters.clone(),
                        required_parameters: function
                            .schema
                            .get("required")
                            .and_then(serde_json::Value::as_array)
                            .map_or(0, Vec::len),
                    })
                    .collect(),
            })
            .collect::<Vec<_>>();
        modules.extend(self.js_extensions.iter().map(|extension| {
            NativeModule {
                name: extension.module.clone(),
                functions: extension
                    .tools
                    .iter()
                    .map(|tool| NativeFunction {
                        name: tool.name.clone(),
                        parameters: Vec::new(),
                        required_parameters: 0,
                    })
                    .collect(),
            }
        }));
        let extensions = self
            .extensions
            .iter()
            .map(|extension| (extension.metadata.name.clone(), extension.clone()))
            .collect::<HashMap<_, _>>();
        let js_routes = self
            .js_extensions
            .iter()
            .flat_map(|extension| {
                extension.tools.iter().map(move |tool| {
                    (
                        (extension.module.clone(), tool.name.clone()),
                        extension.extension_id.clone(),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        let js_runtime = self.js_runtime.clone();
        let host_actions = Arc::clone(&self.host_actions);
        let sequence = Arc::new(std::sync::atomic::AtomicU64::new(
            self.call_sequence.load(std::sync::atomic::Ordering::SeqCst),
        ));
        let ui = self.ui.clone();
        let call: NativeCall = Arc::new(move |module, tool, input| {
            let extension = extensions.get(&module).cloned();
            let js_extension_id = js_routes.get(&(module.clone(), tool.clone())).cloned();
            let js_runtime = js_runtime.clone();
            let sequence = Arc::clone(&sequence);
            let host_actions = Arc::clone(&host_actions);
            let ui = ui.clone();
            Box::pin(async move {
                if let Some(extension) = extension {
                    return call_extension(extension, session, &tool, input)
                        .await
                        .map_err(|error| format!("{module}.{tool}: {error:#}"));
                }
                let extension_id =
                    js_extension_id.ok_or_else(|| format!("unknown tool {module}.{tool}"))?;
                let runtime = js_runtime
                    .as_ref()
                    .ok_or_else(|| format!("{module}.{tool}: JS runtime unavailable"))?;
                let call_id = format!(
                    "ptc-{}-{}",
                    session.0,
                    sequence.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                );
                let context = serde_json::json!({
                    "cwd": std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(),
                    "mode": ui.mode(), "hasUI": ui.has_ui(), "sessionId": session.0.to_string()
                });
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    runtime.execute(
                        extension_id,
                        tool.clone(),
                        call_id,
                        input,
                        context,
                        host_actions,
                    ),
                )
                .await
                .map_err(|_| format!("{module}.{tool}: timed out after 60 seconds"))?
                .map_err(|error| format!("{module}.{tool}: {error}"))
            })
        });

        let ProgramOutput { stdout, stderr } =
            execute_program_with_hostcalls(code, &modules, call, execute_hostcall)
                .await
                .map_err(anyhow::Error::new)?;
        Ok(PTCOutput { stdout, stderr })
    }
}

impl PiCompat {
    async fn render_tool_event(&self, event: &AgentEvent, ctx: &SessionContext) {
        let (id, extension, name, slot, payload) = match event {
            AgentEvent::ToolExecutionStart { id, name, input } => {
                let extension = self
                    .js_extensions
                    .iter()
                    .find(|extension| extension.tools.iter().any(|tool| tool.name == *name))
                    .map(|extension| extension.extension_id.clone());
                let Some(extension) = extension else { return };
                self.tool_calls
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(id.clone(), (extension.clone(), name.clone()));
                (
                    id.clone(),
                    extension.clone(),
                    name.clone(),
                    "call",
                    serde_json::json!({"toolCallId":id,"args":serde_json::from_str::<serde_json::Value>(input).unwrap_or_else(|_| serde_json::Value::String(input.clone()))}),
                )
            }
            AgentEvent::ToolExecutionUpdate { id, update } => {
                let Some((extension, name)) = self
                    .tool_calls
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(id)
                    .cloned()
                else {
                    return;
                };
                (
                    id.clone(),
                    extension,
                    name,
                    "result",
                    serde_json::json!({"toolCallId":id,"result":update,"isPartial":true}),
                )
            }
            AgentEvent::ToolExecutionEnd {
                id,
                name,
                result,
                is_error,
            } => {
                let extension = self
                    .tool_calls
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(id)
                    .map(|(extension, _)| extension)
                    .or_else(|| {
                        self.js_extensions
                            .iter()
                            .find(|extension| extension.tools.iter().any(|tool| tool.name == *name))
                            .map(|extension| extension.extension_id.clone())
                    });
                let Some(extension) = extension else { return };
                (
                    id.clone(),
                    extension,
                    name.clone(),
                    "result",
                    serde_json::json!({"toolCallId":id,"result":result,"isPartial":false,"isError":is_error}),
                )
            }
            _ => return,
        };
        let Some(runtime) = &self.js_runtime else {
            return;
        };
        let Ok(Some(lines)) = runtime
            .render_tool(
                extension.clone(),
                name,
                slot.into(),
                payload,
                self.pi_context(ctx),
                80,
            )
            .await
        else {
            return;
        };
        let _ = self
            .ui
            .execute_hostcall(
                &extension,
                "render",
                serde_json::json!({"key":id,"lines":lines}),
            )
            .await;
    }

    async fn dispatch_pi(
        &self,
        name: &str,
        payload: serde_json::Value,
        ctx: &SessionContext,
    ) -> Result<serde_json::Value> {
        let Some(runtime) = &self.js_runtime else {
            return Ok(serde_json::Value::Null);
        };
        runtime
            .dispatch(
                name.to_string(),
                payload,
                self.pi_context(ctx),
                Arc::clone(&self.host_actions),
            )
            .await
            .with_context(|| format!("dispatch Pi event {name}"))
    }
}

#[async_trait::async_trait(?Send)]
impl AgentHooks for PiCompat {
    async fn on_input(
        &self,
        message: &mut UserMessage,
        ctx: &SessionContext,
    ) -> Result<InputOutcome> {
        let text = message
            .content
            .iter()
            .filter_map(|part| match part {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let result = self
            .dispatch_pi(
                "input",
                serde_json::json!({"text":text,"source":"interactive"}),
                ctx,
            )
            .await?;
        if result["action"] == "handled" {
            return Ok(InputOutcome::Handled);
        }
        if result["action"] == "transform" {
            *message = UserMessage::text(result["text"].as_str().unwrap_or_default());
        }
        Ok(InputOutcome::Continue)
    }

    async fn before_agent_start(
        &self,
        input: &mut BeforeAgentStart,
        ctx: &SessionContext,
    ) -> Result<()> {
        let result = self
            .dispatch_pi(
                "before_agent_start",
                serde_json::json!({"prompt":input.prompt,"systemPrompt":input.system_prompt}),
                ctx,
            )
            .await?;
        if let Some(prompt) = result["systemPrompt"].as_str() {
            input.system_prompt = prompt.to_string();
        }
        if let Some(messages) = result.get("messages") {
            input.messages.extend(
                serde_json::from_value::<Vec<Message>>(messages.clone()).unwrap_or_default(),
            );
        }
        Ok(())
    }

    async fn on_context(&self, messages: &mut Vec<Message>, ctx: &SessionContext) -> Result<()> {
        let result = self
            .dispatch_pi("context", serde_json::json!({"messages":messages}), ctx)
            .await?;
        if let Some(changed) = result.get("messages") {
            *messages =
                serde_json::from_value(changed.clone()).context("invalid Pi context messages")?;
        }
        Ok(())
    }

    async fn on_tool_call(
        &self,
        call: &mut ToolCall,
        ctx: &SessionContext,
    ) -> Result<ToolCallOutcome> {
        let input = serde_json::from_str::<serde_json::Value>(&call.input)
            .unwrap_or_else(|_| serde_json::Value::String(call.input.clone()));
        let result = self
            .dispatch_pi(
                "tool_call",
                serde_json::json!({"toolCallId":call.id,"toolName":call.name,"input":input}),
                ctx,
            )
            .await?;
        if result["block"].as_bool() == Some(true) {
            return Ok(ToolCallOutcome::Block(
                result["reason"]
                    .as_str()
                    .unwrap_or("blocked by extension")
                    .to_string(),
            ));
        }
        if let Some(input) = result.get("input") {
            call.input = input
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or(serde_json::to_string(input)?);
        }
        Ok(ToolCallOutcome::Continue)
    }

    async fn on_tool_result(
        &self,
        result: &mut ToolResultMessage,
        ctx: &SessionContext,
    ) -> Result<()> {
        let changed = self.dispatch_pi("tool_result", serde_json::json!({"toolCallId":result.tool_use_id,"content":result.content,"details":result.details,"isError":result.is_error}), ctx).await?;
        if let Some(content) = changed.get("content") {
            result.content = serde_json::from_value(content.clone())
                .context("invalid Pi tool result content")?;
        }
        if changed.get("details").is_some() {
            result.details = changed.get("details").cloned();
        }
        if let Some(is_error) = changed["isError"].as_bool() {
            result.is_error = is_error;
        }
        Ok(())
    }

    async fn on_message_finalizing(
        &self,
        message: &mut Message,
        ctx: &SessionContext,
    ) -> Result<()> {
        let result = self
            .dispatch_pi("message_end", serde_json::json!({"message":message}), ctx)
            .await?;
        if let Some(changed) = result.get("message") {
            *message =
                serde_json::from_value(changed.clone()).context("invalid Pi final message")?;
        }
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl ExtensionHost for PiCompat {
    async fn observe(&self, event: &AgentEvent, ctx: &SessionContext) -> Result<()> {
        self.render_tool_event(event, ctx).await;
        let Some((name, payload)) = Self::pi_event(event) else {
            return Ok(());
        };
        self.dispatch_pi(name, payload, ctx).await?;
        Ok(())
    }

    fn commands(&self) -> Vec<CommandDef> {
        self.js_commands.clone()
    }
    async fn command(&self, name: &str, args: &str, ctx: &SessionContext) -> Result<()> {
        let runtime = self
            .js_runtime
            .as_ref()
            .context("Pi extension runtime unavailable")?;
        runtime
            .command(
                name.to_string(),
                args.to_string(),
                self.pi_context(ctx),
                Arc::clone(&self.host_actions),
            )
            .await?;
        Ok(())
    }
    fn take_host_actions(&self) -> Vec<HostAction> {
        self.host_actions.take()
    }
}

async fn execute_hostcall_with_state(
    request: HostcallRequest,
    state: Arc<HostcallState>,
    ui: &PiUiConfig,
) -> Vec<HostcallOutcome> {
    if let HostcallKind::Events { op } = &request.kind {
        let action = match op.as_str() {
            "appendEntry" => Some(HostAction::AppendEntry {
                kind: request.payload["customType"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                data: request
                    .payload
                    .get("data")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }),
            "sendUserMessage" => Some(HostAction::SendUserMessage {
                text: request.payload["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                deliver_as: request.payload["options"]["deliverAs"]
                    .as_str()
                    .unwrap_or("followUp")
                    .to_string(),
            }),
            "sendMessage" => Some(HostAction::SendMessage {
                message: request.payload.get("message").cloned().unwrap_or_default(),
                deliver_as: request.payload["options"]["deliverAs"]
                    .as_str()
                    .unwrap_or("followUp")
                    .to_string(),
                trigger_turn: request.payload["options"]["triggerTurn"]
                    .as_bool()
                    .unwrap_or(false),
            }),
            _ => None,
        };
        if let Some(action) = action {
            state
                .actions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(action);
            return vec![HostcallOutcome::Success(serde_json::Value::Null)];
        }
    }
    if matches!(request.kind, HostcallKind::Ui { .. }) {
        return ui.execute(request).await;
    }
    execute_hostcall(request).await
}

async fn execute_hostcall(request: HostcallRequest) -> Vec<HostcallOutcome> {
    let method = request.method();
    if matches!(request.kind, HostcallKind::Http) {
        let url = request.payload["url"].as_str().unwrap_or_default();
        let method = request.payload["method"].as_str().unwrap_or("GET");
        let client = reqwest::Client::new();
        let mut builder = client.request(method.parse().unwrap_or(reqwest::Method::GET), url);
        if let Some(headers) = request.payload["headers"].as_object() {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    builder = builder.header(name, value);
                }
            }
        }
        if let Some(body) = request.payload["body"].as_str() {
            builder = builder.body(body.to_string());
        } else if let Some(body) = request.payload["body_bytes"].as_str()
            && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body)
        {
            builder = builder.body(bytes);
        }
        return match builder.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let headers = response
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value.to_str().ok().map(|value| {
                            (
                                name.to_string(),
                                serde_json::Value::String(value.to_string()),
                            )
                        })
                    })
                    .collect::<serde_json::Map<_, _>>();
                match response.bytes().await {
                    Ok(bytes) => vec![HostcallOutcome::Success(serde_json::json!({
                        "status": status,
                        "headers": headers,
                        "body_bytes": base64::engine::general_purpose::STANDARD.encode(bytes),
                    }))],
                    Err(error) => vec![HostcallOutcome::Error {
                        code: "http_body".into(),
                        message: error.to_string(),
                    }],
                }
            }
            Err(error) => vec![HostcallOutcome::Error {
                code: "http".into(),
                message: error.to_string(),
            }],
        };
    }
    let HostcallKind::Exec { cmd } = request.kind else {
        return vec![HostcallOutcome::Error {
            code: "unsupported".to_string(),
            message: format!("PTC does not support {method} hostcalls"),
        }];
    };

    let args = request.payload["args"]
        .as_array()
        .map(|args| args.iter().filter_map(serde_json::Value::as_str))
        .into_iter()
        .flatten();
    let mut command = tokio::process::Command::new(cmd);
    command.args(args);
    if let Some(cwd) = request.payload["options"]["cwd"].as_str() {
        command.current_dir(cwd);
    }

    match command.output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            vec![
                HostcallOutcome::StreamChunk {
                    sequence: 0,
                    chunk: serde_json::json!({ "stdout": stdout, "stderr": stderr }),
                    is_final: false,
                },
                HostcallOutcome::Success(serde_json::json!({
                    "code": output.status.code(),
                    "stdout": stdout,
                    "stderr": stderr,
                    "killed": false,
                    "signal": null,
                })),
            ]
        }
        Err(error) => vec![HostcallOutcome::Error {
            code: error
                .raw_os_error()
                .map_or_else(|| "exec".to_string(), |code| code.to_string()),
            message: error.to_string(),
        }],
    }
}

unsafe fn copy_buffer(buffer: AbiBuffer) -> Result<Vec<u8>> {
    if buffer.ptr.is_null() {
        if buffer.len == 0 {
            return Ok(Vec::new());
        }
        bail!("extension returned a null buffer");
    }
    Ok(unsafe { std::slice::from_raw_parts(buffer.ptr, buffer.len) }.to_vec())
}

struct CallbackState {
    sender: tokio::sync::oneshot::Sender<std::result::Result<Vec<u8>, String>>,
    free_buffer: unsafe extern "C" fn(AbiBuffer),
}

unsafe extern "C" fn complete_call(user_data: *mut c_void, buffer: AbiBuffer, is_error: bool) {
    let state = unsafe { Box::from_raw(user_data.cast::<CallbackState>()) };
    let bytes = unsafe { copy_buffer(buffer) };
    unsafe { (state.free_buffer)(buffer) };
    let result = match bytes {
        Ok(bytes) if is_error => Err(String::from_utf8_lossy(&bytes).into_owned()),
        Ok(bytes) => Ok(bytes),
        Err(error) => Err(format!("{error:#}")),
    };
    let _ = state.sender.send(result);
}

async fn call_extension(
    extension: Arc<LoadedExtension>,
    session: SessionId,
    tool: &str,
    input: serde_json::Value,
) -> Result<serde_json::Value> {
    let input = serde_json::to_vec(&input)?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let state = Box::new(CallbackState {
        sender,
        free_buffer: extension.abi.free_buffer,
    });
    unsafe {
        (extension.abi.start_call)(
            session.0,
            tool.as_ptr(),
            tool.len(),
            input.as_ptr(),
            input.len(),
            complete_call,
            Box::into_raw(state).cast(),
        );
    }
    let bytes = receiver
        .await
        .context("extension dropped its callback")?
        .map_err(anyhow::Error::msg)?;
    serde_json::from_slice(&bytes).context("extension returned invalid result JSON")
}

fn module_name_for_path(path: &Path) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .context("extension entry has no UTF-8 file stem")?;
    let name = stem.replace('-', "_");
    if !valid_module_name(&name) {
        bail!(
            "invalid PTC module name derived from {}: {name}",
            path.display()
        );
    }
    Ok(name)
}

fn valid_module_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch == '-' || ch.is_ascii_alphanumeric())
}

fn valid_function_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphanumeric())
}

fn validate_js_tools(module: &str, tools: &[ExtensionToolDef]) -> Result<()> {
    let mut names = BTreeSet::new();
    for tool in tools {
        if !valid_function_name(&tool.name) {
            bail!("{module}.{} is not a JavaScript identifier", tool.name);
        }
        if !names.insert(tool.name.as_str()) {
            bail!("{module} declares duplicate tool {}", tool.name);
        }
        if tool.description.trim().is_empty() {
            bail!("{module}.{} description is empty", tool.name);
        }
        if tool
            .parameters
            .get("type")
            .and_then(serde_json::Value::as_str)
            != Some("object")
        {
            bail!("{module}.{} input schema must be an object", tool.name);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiCapability {
    pub name: &'static str,
    pub status: CapabilityStatus,
}

impl PiCompat {
    pub fn compatibility_matrix() -> &'static [PiCapability] {
        &[
            PiCapability {
                name: "session_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "session_shutdown",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "input",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "before_agent_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "context",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "agent_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "agent_end",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "agent_settled",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "turn_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "turn_end",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "message_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "message_update",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "message_end",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "tool_execution_start",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "tool_execution_update",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "tool_execution_end",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "tool_call",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "tool_result",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "registerTool",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "registerCommand",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "sendUserMessage",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "sendMessage",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "appendEntry",
                status: CapabilityStatus::Supported,
            },
            PiCapability {
                name: "events.emit/on",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "registerProvider",
                status: CapabilityStatus::Unsupported,
            },
            PiCapability {
                name: "registerShortcut",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "registerMessageRenderer",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "registerEntryRenderer",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "registerMarkdownTransformer",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "onTerminalInput",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "registerFlag",
                status: CapabilityStatus::Unsupported,
            },
            PiCapability {
                name: "pi-ui-0.84.2",
                status: CapabilityStatus::Partial,
            },
            PiCapability {
                name: "terminal-images",
                status: CapabilityStatus::Unsupported,
            },
            PiCapability {
                name: "undocumented-pi-internals",
                status: CapabilityStatus::Unsupported,
            },
        ]
    }

    pub fn unsupported_capabilities() -> Vec<&'static str> {
        Self::compatibility_matrix()
            .iter()
            .filter(|item| item.status == CapabilityStatus::Unsupported)
            .map(|item| item.name)
            .collect()
    }
}

fn validate_metadata(extension: &ToolExtension) -> Result<()> {
    if extension.name.trim().is_empty() {
        bail!("extension name is empty");
    }
    if !valid_module_name(&extension.name) {
        bail!("invalid PTC module name: {}", extension.name);
    }
    if extension.description.trim().is_empty() {
        bail!("{} extension description is empty", extension.name);
    }
    let mut names = BTreeSet::new();
    for function in &extension.functions {
        if !valid_function_name(&function.name) {
            bail!(
                "{}.{} is not a JavaScript identifier",
                extension.name,
                function.name
            );
        }
        if !names.insert(function.name.as_str()) {
            bail!(
                "{} declares duplicate tool {}",
                extension.name,
                function.name
            );
        }
        if function.schema.get("type").and_then(|value| value.as_str()) != Some("object") {
            bail!(
                "{}.{} input schema must be an object",
                extension.name,
                function.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use e_agent_core::{
        AgentEvent, AgentHooks, InputOutcome, MessageContent, SessionContext, ToolExecutor,
        UserMessage, tool::extension::ExtensionHost,
    };
    use e_agent_extension::SessionId;
    use tokio_util::sync::CancellationToken;

    use super::{CapabilityStatus, PiCompat, PiUiConfig};

    fn frame_text(frame: &e_agent_tui::render::SemanticFrame) -> String {
        (0..frame.size.height)
            .map(|row| {
                frame.cells[usize::from(row) * usize::from(frame.size.width)
                    ..usize::from(row + 1) * usize::from(frame.size.width)]
                    .iter()
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn context() -> SessionContext {
        SessionContext {
            session_id: SessionId::next(),
            cwd: PathBuf::from("."),
            entries: Vec::new(),
            idle: true,
            signal: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn fixture_maps_input_tool_and_notification_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.ts");
        std::fs::write(&path, r#"
let starts = 0;
export default function (pi) {
  pi.on("input", async (event) => ({ action: "transform", text: event.text + " A" }));
  pi.on("agent_start", async () => { starts += 1; });
  pi.registerCommand("fixture", { description: "fixture", handler: async () => {} });
  pi.registerTool({ name: "starts", description: "count", parameters: { type: "object", properties: {} }, async execute() { return { content: [{ type: "text", text: String(starts) }] }; } });
}
"#).unwrap();
        let mut compat = PiCompat::default();
        compat.load_extension(&path).await.unwrap();
        let ctx = context();
        let mut message = UserMessage::text("hello");
        assert_eq!(
            compat.on_input(&mut message, &ctx).await.unwrap(),
            InputOutcome::Continue
        );
        assert!(matches!(&message.content[0], MessageContent::Text { text } if text == "hello A"));
        compat
            .observe(&AgentEvent::AgentStart { run_id: 1 }, &ctx)
            .await
            .unwrap();
        assert_eq!(compat.commands().len(), 1);
        assert!(compat.tool_defs().iter().any(|tool| tool.name == "starts"));
        let direct = compat
            .call(ctx.session_id, "starts", "{}".into())
            .await
            .unwrap();
        assert!(matches!(&direct.content[0], MessageContent::Text { text } if text == "1"));
        let output = compat.call(ctx.session_id, "node", serde_json::json!({"code":"import { starts } from \"fixture\"; console.log((await starts({})).content[0].text);"}).to_string()).await.unwrap();
        assert!(matches!(&output.content[0], MessageContent::Text { text } if text.contains('1')));
    }

    #[tokio::test]
    async fn pinned_widget_placement_fixture_runs_without_source_adaptation() {
        let capabilities = e_agent_tui::ui_protocol::native_capabilities();
        let (client, mut server) = e_agent_tui::broker::channel(capabilities.clone());
        let mut compat = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi-0.84.2/extensions/widget-placement.ts");
        compat.load_extension(fixture).await.unwrap();

        let broker = tokio::spawn(async move {
            let mut operations = Vec::new();
            for _ in 0..2 {
                let envelope = server.recv().await.expect("widget request");
                operations.push(envelope.operation.clone());
                server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
            }
            operations
        });
        let ctx = context();
        compat
            .observe(
                &AgentEvent::SessionStart {
                    session_id: ctx.session_id.to_string(),
                },
                &ctx,
            )
            .await
            .unwrap();

        let operations = broker.await.unwrap();
        assert!(
            operations.contains(&e_agent_tui::ui_protocol::UiOperation::Contribution(
                e_agent_tui::ui_protocol::Contribution::Set {
                    slot: "widget".into(),
                    key: "widget-above".into(),
                    content: "Above editor widget".into(),
                },
            ))
        );
        assert!(
            operations.contains(&e_agent_tui::ui_protocol::UiOperation::Contribution(
                e_agent_tui::ui_protocol::Contribution::Set {
                    slot: "below-widget".into(),
                    key: "widget-below".into(),
                    content: "Below editor widget".into(),
                },
            ))
        );
    }

    #[tokio::test]
    async fn custom_component_routes_input_and_disposes_through_the_broker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.ts");
        std::fs::write(
            &path,
            r#"
export default function (pi) {
  pi.registerCommand("custom", {
    description: "custom",
    handler: async (_args, ctx) => {
      ctx.ui.onTerminalInput((key) => ({ data: key === "x" ? "y" : key }));
      return ctx.ui.custom(
        async (_tui, _theme, _keybindings, done) => {
        await Promise.resolve();
        return {
          render: () => ["ready"],
          handleInput: (key) => { if (key === "y") done("accepted"); },
          dispose: () => { throw new Error("dispose failure"); },
        };
      },
      {
        overlay: true,
        onHandle: (handle) => {
          handle.setHidden(true);
          handle.setHidden(false);
          handle.focus();
          handle.unfocus();
        },
      },
    );
    },
  });
}
"#,
        )
        .unwrap();
        let capabilities = e_agent_tui::ui_protocol::native_capabilities();
        let (client, mut server) = e_agent_tui::broker::channel(capabilities.clone());
        let mut compat = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        compat.load_extension(&path).await.unwrap();

        let ctx = context();
        let command = compat.command("custom", "", &ctx);
        tokio::pin!(command);
        let mut saw_overlay = false;
        let mut saw_input_poll = false;
        let mut saw_hide = false;
        let mut saw_hidden = [false; 2];
        let mut saw_focus = false;
        let mut saw_unfocus = false;
        loop {
            tokio::select! {
                result = &mut command => {
                    result.unwrap();
                    break;
                }
                envelope = server.recv() => {
                    let envelope = envelope.expect("custom component request");
                    match envelope.operation {
                        e_agent_tui::ui_protocol::UiOperation::TerminalInput { .. } => {
                            saw_input_poll = true;
                            assert!(server.queue_input_poll(envelope.request));
                            assert!(server.reply_input(e_agent_tui::input::InputEvent::Key(
                                e_agent_tui::input::KeyEvent {
                                    code: e_agent_tui::input::KeyCode::Char('x'),
                                    modifiers: e_agent_tui::input::Modifiers::default(),
                                    kind: e_agent_tui::input::KeyKind::Press,
                                },
                            )));
                        }
                        e_agent_tui::ui_protocol::UiOperation::Overlay { action, .. } => {
                            match action {
                                e_agent_tui::ui_protocol::OverlayAction::Show { content, .. } => {
                                    saw_overlay = content == "ready";
                                }
                                e_agent_tui::ui_protocol::OverlayAction::Hide => saw_hide = true,
                                e_agent_tui::ui_protocol::OverlayAction::SetHidden(hidden) => {
                                    saw_hidden[usize::from(hidden)] = true;
                                }
                                e_agent_tui::ui_protocol::OverlayAction::Focus => saw_focus = true,
                                e_agent_tui::ui_protocol::OverlayAction::Unfocus => saw_unfocus = true,
                            }
                            server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
                        }
                        other => panic!("unexpected custom operation: {other:?}"),
                    }
                }
            }
        }
        assert!(saw_overlay);
        assert!(saw_input_poll);
        assert!(saw_hide);
        assert_eq!(saw_hidden, [true, true]);
        assert!(saw_focus);
        assert!(saw_unfocus);
    }

    #[tokio::test]
    async fn tool_renderer_flows_from_agent_event_to_cached_native_render() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("render.ts");
        std::fs::write(
            &path,
            r#"
export default function (pi) {
  pi.registerTool({
    name: "rendered",
    description: "rendered",
    parameters: { type: "object", properties: { label: { type: "string" } } },
    async execute() { return { content: [{ type: "text", text: "done" }] }; },
    renderCall(args, _theme, context) {
      context.state.count = (context.state.count || 0) + 1;
      return { render: () => [`call:${args.label}:${context.state.count}`] };
    },
    renderResult(result, options, _theme, context) {
      return { render: () => [`result:${result.text}:${options.isPartial}:${context.state.count}`] };
    },
  });
}
"#,
        )
        .unwrap();
        let capabilities = e_agent_tui::ui_protocol::native_capabilities();
        let (client, mut server) = e_agent_tui::broker::channel(capabilities.clone());
        let mut compat = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        compat.load_extension(path).await.unwrap();
        let broker = tokio::spawn(async move {
            let envelope = server.recv().await.expect("render request");
            let operation = envelope.operation.clone();
            server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
            operation
        });
        let ctx = context();
        compat
            .observe(
                &AgentEvent::ToolExecutionStart {
                    id: "tool-1".into(),
                    name: "rendered".into(),
                    input: serde_json::json!({"label":"x"}).to_string(),
                },
                &ctx,
            )
            .await
            .unwrap();
        let operation = broker.await.unwrap();
        let e_agent_tui::ui_protocol::UiOperation::Frame { key, frame, .. } = operation else {
            panic!("expected semantic frame")
        };
        assert_eq!(key, "tool-1");
        assert_eq!(frame_text(&frame), "call:x:1");
    }

    #[tokio::test]
    async fn message_and_entry_renderers_publish_isolated_cached_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("entries.ts");
        std::fs::write(
            &path,
            r#"
export default function (pi) {
  pi.registerMessageRenderer("status", (message) => ({ render: () => [`message:${message.content}`] }));
  pi.registerEntryRenderer("card", (entry) => ({ render: () => [`entry:${entry.data.value}`] }));
  pi.registerMessageRenderer("broken", () => { throw new Error("broken"); });
  pi.registerCommand("publish", {
    description: "publish",
    handler: async () => {
      pi.sendMessage({ customType: "status", content: "ok", display: true });
      pi.sendMessage({ customType: "broken", content: "fallback", display: true });
      pi.appendEntry("card", { value: "saved" });
    },
  });
}
"#,
        )
        .unwrap();
        let capabilities = e_agent_tui::ui_protocol::native_capabilities();
        let (client, mut server) = e_agent_tui::broker::channel(capabilities.clone());
        let mut compat = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        compat.load_extension(path).await.unwrap();
        let ctx = context();
        let command = compat.command("publish", "", &ctx);
        tokio::pin!(command);
        let mut frames = Vec::new();
        loop {
            tokio::select! {
                result = &mut command => {
                    result.unwrap();
                    break;
                }
                envelope = server.recv() => {
                    let envelope = envelope.expect("render request");
                    if let e_agent_tui::ui_protocol::UiOperation::Frame { frame, .. } = envelope.operation {
                        frames.push(frame_text(&frame));
                    }
                    server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
                }
            }
        }
        assert!(frames.contains(&"message:ok".to_string()));
        assert!(frames.contains(&"entry:saved".to_string()));
        assert!(!frames.iter().any(|frame| frame.contains("fallback")));
        assert_eq!(compat.take_host_actions().len(), 3);
    }

    #[tokio::test]
    async fn pinned_modal_editor_fixture_mounts_without_source_adaptation() {
        let capabilities = e_agent_tui::ui_protocol::native_capabilities();
        let (client, mut server) = e_agent_tui::broker::channel(capabilities.clone());
        let mut compat = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi-0.84.2/extensions/modal-editor.ts");
        compat.load_extension(fixture).await.unwrap();
        let broker = tokio::spawn(async move {
            let envelope = server.recv().await.expect("editor frame");
            let operation = envelope.operation.clone();
            server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
            (server, operation)
        });
        let ctx = context();
        compat
            .observe(
                &AgentEvent::SessionStart {
                    session_id: ctx.session_id.to_string(),
                },
                &ctx,
            )
            .await
            .unwrap();
        let (mut server, operation) = broker.await.unwrap();
        assert!(matches!(
            operation,
            e_agent_tui::ui_protocol::UiOperation::CustomEditor { content: Some(_) }
        ));
        assert!(server.publish_input(e_agent_tui::input::InputEvent::Text("a".into())));
        let envelope = tokio::time::timeout(std::time::Duration::from_secs(1), server.recv())
            .await
            .expect("editor input timeout")
            .expect("updated editor frame");
        assert!(matches!(
            &envelope.operation,
            e_agent_tui::ui_protocol::UiOperation::CustomEditor { content: Some(content) }
                if content.contains('a')
        ));
        server.reply(envelope.request, e_agent_tui::ui_protocol::UiReply::Ack);
    }

    #[test]
    fn ui_mode_is_explicit() {
        let ctx = context();
        let headless = PiCompat::default();
        assert_eq!(headless.pi_context(&ctx)["mode"], "print");
        assert_eq!(headless.pi_context(&ctx)["hasUI"], false);

        let capabilities = e_agent_tui::ui_protocol::UiCapabilities::default();
        let (client, _server) = e_agent_tui::broker::channel(capabilities.clone());
        let interactive = PiCompat::with_ui(PiUiConfig::interactive(client, capabilities));
        assert_eq!(interactive.pi_context(&ctx)["mode"], "tui");
        assert_eq!(interactive.pi_context(&ctx)["hasUI"], true);
    }

    #[test]
    fn compatibility_matrix_is_explicit() {
        let matrix = PiCompat::compatibility_matrix();
        assert!(
            matrix
                .iter()
                .any(|item| item.name == "input" && item.status == CapabilityStatus::Supported)
        );
        assert!(
            matrix.iter().any(
                |item| item.name == "pi-ui-0.84.2" && item.status == CapabilityStatus::Partial
            )
        );
        assert!(PiCompat::unsupported_capabilities().contains(&"registerProvider"));
    }
}
