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

use crate::{
    message::{MessageContent, ToolDef, ToolInput},
    tool::{ToolExecutor, ToolOutput},
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
        reply: tokio::sync::oneshot::Sender<Result<serde_json::Value>>,
    },
    Shutdown,
}

#[derive(Clone)]
struct JsRuntimeHandle {
    sender: tokio::sync::mpsc::UnboundedSender<JsRuntimeCommand>,
}

impl JsRuntimeHandle {
    fn start() -> Result<Self> {
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
                        while let Some(command) = receiver.recv().await {
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
                                    reply,
                                } => {
                                    let result = runtime
                                        .execute_extension_tool_with_hostcalls(
                                            &extension_id,
                                            &tool,
                                            &call_id,
                                            input,
                                            context,
                                            execute_hostcall,
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
    ) -> Result<serde_json::Value> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.sender
            .send(JsRuntimeCommand::Execute {
                extension_id,
                tool,
                call_id,
                input,
                context,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("Pi extension runtime is shut down"))?;
        receive.await.context("Pi extension runtime stopped")?
    }

    fn shutdown(&self) {
        let _ = self.sender.send(JsRuntimeCommand::Shutdown);
    }
}

#[derive(Default)]
pub struct ProgrammaticToolExecutor {
    extensions: Vec<Arc<LoadedExtension>>,
    js_runtime: Option<JsRuntimeHandle>,
    js_extensions: Vec<JsExtension>,
    closed: std::sync::atomic::AtomicBool,
    call_sequence: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait(?Send)]
impl ToolExecutor for ProgrammaticToolExecutor {
    type Error = anyhow::Error;

    fn tool_defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "node".into(),
            description: format!(
                "Execute one complete TypeScript ES module as a program in the Node-compatible QuickJS runtime. Use static top-level ES imports. Call every async function with one object argument and await it. Rust tools additionally retain legacy positional compatibility. The output_schema is authoritative. The unified module catalog follows:\n{}",
                serde_json::to_string_pretty(&self.catalog())
                    .expect("tool metadata must serialize")
            ),
            input: ToolInput::Text,
        }]
    }

    async fn call(
        &self,
        session: SessionId,
        _name: &str,
        code: String,
    ) -> Result<ToolOutput, Self::Error> {
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

impl ProgrammaticToolExecutor {
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
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

    pub async fn load_pi_extension(&mut self, path: impl AsRef<Path>) -> Result<()> {
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
            self.js_runtime = Some(JsRuntimeHandle::start()?);
        }
        let runtime = self.js_runtime.as_ref().expect("runtime initialized");
        let tools = runtime
            .load(extension_id.clone(), path.clone())
            .await
            .with_context(|| format!("load Pi extension {}", path.display()))?;
        if tools.is_empty() {
            bail!("Pi extension {module} registered no tools");
        }
        validate_js_tools(&module, &tools)?;
        self.js_extensions.push(JsExtension {
            module,
            extension_id,
            tools,
        });
        Ok(())
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
        let sequence = Arc::new(std::sync::atomic::AtomicU64::new(
            self.call_sequence.load(std::sync::atomic::Ordering::SeqCst),
        ));
        let call: NativeCall = Arc::new(move |module, tool, input| {
            let extension = extensions.get(&module).cloned();
            let js_extension_id = js_routes.get(&(module.clone(), tool.clone())).cloned();
            let js_runtime = js_runtime.clone();
            let sequence = Arc::clone(&sequence);
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
                    "mode": "print", "hasUI": false, "sessionId": session.0.to_string()
                });
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    runtime.execute(extension_id, tool.clone(), call_id, input, context),
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
    use std::{path::PathBuf, process::Command, sync::Arc};

    use e_agent_extension::SessionId;
    use e_agent_node_runtime::{NativeCall, NativeFunction, NativeModule, execute_program};

    use crate::tool::ToolExecutor;

    use super::ProgrammaticToolExecutor;

    static TEST_EXECUTION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn build_extension(package: &str, library_name: &str) -> PathBuf {
        static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = BUILD.lock().unwrap_or_else(|error| error.into_inner());
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let target = root.join(format!("target/{library_name}-node-test"));
        assert!(
            Command::new(env!("CARGO"))
                .args(["build", "-p", package, "--target-dir"])
                .arg(&target)
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        #[cfg(windows)]
        let library = target.join(format!("debug/{library_name}.dll"));
        #[cfg(target_os = "linux")]
        let library = target.join(format!("debug/lib{library_name}.so"));
        #[cfg(target_os = "macos")]
        let library = target.join(format!("debug/lib{library_name}.dylib"));
        library
    }

    fn built_executor() -> ProgrammaticToolExecutor {
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load(build_extension("e-todo", "todo")).unwrap();
        executor
    }

    #[test]
    fn describes_unified_object_calls_and_output_schemas() {
        let description = ProgrammaticToolExecutor::default()
            .tool_defs()
            .remove(0)
            .description;
        assert!(description.contains("static top-level ES imports"));
        assert!(description.contains("one object argument"));
        assert!(description.contains("legacy positional compatibility"));
        assert!(description.contains("output_schema"));
    }

    #[tokio::test]
    async fn exposes_node_and_runs_typescript_with_native_module() {
        let _guard = TEST_EXECUTION.lock().await;
        let executor = built_executor();
        assert_eq!(executor.tool_defs()[0].name, "node");
        let node_description = executor.tool_defs().remove(0).description;
        assert!(node_description.contains("one complete TypeScript ES module"));
        assert!(node_description.contains("one object argument"));
        let fixture = tempfile::NamedTempFile::new_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::write(fixture.path(), "file-data").unwrap();
        let path = serde_json::to_string(&fixture.path().to_string_lossy()).unwrap();
        let code = format!(
            r#"
import * as todo from "todo";
import {{ readFileSync }} from "node:fs";
import {{ basename }} from "node:path";
await todo.create_todo_list(["node"]);
const value: Array<{{ content: string }}> = await todo.list();
console.log(basename("/tmp/file.txt"), readFileSync({path}, "utf8"), value[0].content);
console.error("err");
"#
        );
        let output = executor.execute(SessionId::next(), &code).await.unwrap();
        assert_eq!(output.stdout, "file.txt file-data node\n");
        assert_eq!(output.stderr, "err\n");
    }

    #[tokio::test]
    async fn awaits_async_child_process_hostcalls() {
        let _guard = TEST_EXECUTION.lock().await;
        let executor = ProgrammaticToolExecutor::default();
        #[cfg(windows)]
        let (command, args) = ("cmd", serde_json::json!(["/C", "echo", "hostcall-ok"]));
        #[cfg(not(windows))]
        let (command, args) = ("printf", serde_json::json!(["hostcall-ok\\n"]));
        let code = format!(
            r#"
import {{ execFile }} from "node:child_process";
import {{ promisify }} from "node:util";
const stdout = await promisify(execFile)({command:?}, {args});
console.log(stdout.trim());
"#
        );

        let output = executor.execute(SessionId::next(), &code).await.unwrap();
        assert_eq!(output.stdout, "hostcall-ok\n");
        assert!(output.stderr.is_empty());
    }

    #[tokio::test]
    async fn preserves_long_native_tool_errors() {
        let marker = "native-error-tail-marker";
        let message = format!("{}{marker}", "x".repeat(512));
        let call: NativeCall = Arc::new(move |_, _, _| {
            let message = message.clone();
            Box::pin(async move { Err(message) })
        });
        let modules = [NativeModule {
            name: "long_error".to_string(),
            functions: vec![NativeFunction {
                name: "fail".to_string(),
                parameters: Vec::new(),
                required_parameters: 0,
            }],
        }];

        let error = execute_program(
            "import { fail } from \"long_error\"; await fail();",
            &modules,
            call,
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains(marker), "truncated error: {error}");
    }

    #[tokio::test]
    async fn calls_native_tools_with_rust_parameter_order() {
        let _guard = TEST_EXECUTION.lock().await;
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load(build_extension("e-todo", "todo")).unwrap();
        let todo = &executor.tools()[0];
        let update = todo
            .functions
            .iter()
            .find(|function| function.name == "update")
            .unwrap();
        assert_eq!(update.parameters, ["index", "status"]);
        assert_eq!(update.output_schema["type"], "null");
        let output = executor
            .execute(
                SessionId::next(),
                r#"
import { create_todo_list, update, list } from "todo";
await create_todo_list(["inspect"]);
await update(0, "in_progress");
console.log(await list());
"#,
            )
            .await
            .unwrap();
        assert_eq!(
            output.stdout,
            "[{\"content\":\"inspect\",\"status\":\"in_progress\"}]\n"
        );

        let object_output = executor
            .execute(
                SessionId::next(),
                r#"
import { create_todo_list, update, list } from "todo";
await create_todo_list({ content: ["inspect"] });
await update({ index: 0, status: "in_progress" });
console.log(await list({}));
"#,
            )
            .await
            .unwrap();
        assert_eq!(
            object_output.stdout,
            "[{\"content\":\"inspect\",\"status\":\"in_progress\"}]\n"
        );

        let error = executor
            .execute(
                SessionId::next(),
                r#"import { list } from "todo"; await list(1);"#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("todo.list expects 0 positional arguments or one object, received 1"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn loads_calls_and_preserves_pi_extension_state() {
        let _guard = TEST_EXECUTION.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("my_extension.ts");
        std::fs::write(
            &entry,
            r#"
let count = 0;
export default async function (pi) {
  await Promise.resolve();
  pi.registerTool({
    name: "greet",
    description: "Greet and count",
    parameters: { type: "object", properties: { name: { type: "string" } }, required: ["name"] },
    async execute(_id, params) {
      count += 1;
      return { content: [{ type: "text", text: `Hello ${params.name}` }], details: { name: params.name, count } };
    }
  });
}
"#,
        )
        .unwrap();
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load_pi_extension(&entry).await.unwrap();
        let first = executor.execute(SessionId::next(), r#"import { greet } from "my_extension"; const result = await greet({ name: "Pi" }); console.log(result.content[0].text, result.details.count);"#).await.unwrap();
        let second = executor.execute(SessionId::next(), r#"import { greet } from "my_extension"; const result = await greet({ name: "Again" }); console.log(result.details.count);"#).await.unwrap();
        assert_eq!(first.stdout, "Hello Pi 1\n");
        assert_eq!(second.stdout, "2\n");
    }

    #[tokio::test]
    async fn pi_extension_throw_rejects_with_qualified_name() {
        let _guard = TEST_EXECUTION.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("thrower.ts");
        std::fs::write(&entry, r#"export default function (pi) { pi.registerTool({ name: "fail", description: "Fail", parameters: { type: "object", properties: {} }, async execute() { throw new Error("boom"); } }); }"#).unwrap();
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load_pi_extension(&entry).await.unwrap();
        let error = executor
            .execute(
                SessionId::next(),
                r#"import { fail } from "thrower"; await fail({});"#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("thrower.fail"), "{error}");
        assert!(error.contains("boom"), "{error}");
    }

    #[tokio::test]
    async fn rejects_duplicate_rust_and_pi_module_names() {
        let _guard = TEST_EXECUTION.lock().await;
        let directory = tempfile::tempdir().unwrap();
        let entry = directory.path().join("todo.ts");
        std::fs::write(&entry, r#"export default function (pi) { pi.registerTool({ name: "other", description: "Other", parameters: { type: "object", properties: {} }, async execute() { return { content: [] }; } }); }"#).unwrap();
        let mut executor = built_executor();
        let error = executor
            .load_pi_extension(&entry)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicate PTC module todo"), "{error}");
    }

    #[test]
    fn loads_every_workspace_extension() {
        let mut executor = ProgrammaticToolExecutor::default();
        for (package, library) in [("e-web-access", "web_access"), ("e-todo", "todo")] {
            executor
                .load(build_extension(package, library))
                .unwrap_or_else(|error| panic!("load {library}: {error:#}"));
        }
        assert_eq!(
            executor
                .tools()
                .iter()
                .map(|extension| extension.name.as_str())
                .collect::<Vec<_>>(),
            ["web_access", "todo"]
        );
    }

    #[tokio::test]
    async fn validates_input_and_recovers_after_javascript_error() {
        let _guard = TEST_EXECUTION.lock().await;
        let executor = built_executor();
        let error = executor
            .execute(
                SessionId::next(),
                r#"import { create_todo_list } from "todo"; await create_todo_list(undefined);"#,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("todo.create_todo_list"), "{error}");
        assert!(error.contains("invalid input"), "{error}");

        let error = executor
            .execute(SessionId::next(), "throw new Error('boom')")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("boom"), "{error}");
        assert_eq!(
            executor
                .execute(SessionId::next(), "console.log('restored')")
                .await
                .unwrap()
                .stdout,
            "restored\n"
        );
    }

    #[tokio::test]
    async fn isolates_drops_and_reloads_state() {
        let _guard = TEST_EXECUTION.lock().await;
        let probe = build_extension("e-todo", "todo");
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load(&probe).unwrap();
        let first = SessionId::next();
        let second = SessionId::next();
        let remember = |value: &str| {
            format!(
                "import {{ create_todo_list }} from 'todo'; await create_todo_list([{value:?}]); console.log({value:?});"
            )
        };
        let recall = "import { list } from 'todo'; console.log((await list())[0]?.content ?? '');";
        assert_eq!(
            executor
                .execute(first, &remember("a"))
                .await
                .unwrap()
                .stdout,
            "a\n"
        );
        assert_eq!(executor.execute(second, recall).await.unwrap().stdout, "\n");
        executor.drop_session(first).await.unwrap();
        let error = executor
            .execute(first, recall)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("PTC executor is closed"), "{error}");
        assert!(executor.load(&probe).is_err());
        assert_eq!(executor.tools().len(), 1);
        assert_eq!(executor.system_prompts().len(), 1);
    }
}
