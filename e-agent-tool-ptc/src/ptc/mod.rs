use std::{
    collections::{BTreeSet, HashMap},
    ffi::c_void,
    mem::ManuallyDrop,
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use e_agent_extension::{AbiBuffer, EXTENSION_ABI_VERSION, ExtensionV1, SessionId, ToolExtension};
use e_agent_node_runtime::{
    HostcallKind, HostcallOutcome, HostcallRequest, NativeCall, NativeFunction, NativeModule,
    ProgramOutput, execute_program_with_hostcalls,
};
use libloading::Library;
use serde::Serialize;

use e_agent_core::{
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

#[derive(Default)]
pub struct ProgrammaticToolExecutor {
    extensions: Vec<Arc<LoadedExtension>>,
}

#[async_trait::async_trait(?Send)]
impl ToolExecutor for ProgrammaticToolExecutor {
    type Error = anyhow::Error;

    fn tool_defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "node".into(),
            description: format!(
                "Execute one complete TypeScript ES module as a program in the Node-compatible QuickJS runtime. Pass the complete program in the `code` field. Write normal program logic: declare variables and functions, use conditionals and loops, transform data, handle errors, and combine results. A single program may import extension modules and make multiple native tool calls, using the result of one call in later calls; batch related work in one program when useful. Top-level await, console.log, console.error, and supported Node built-ins are available. Native extension functions use the positional parameters listed in their metadata and return Promises. The program runs in one isolated execution and its stdout/stderr are captured. This is not a complete Node.js or npm runtime.\n\nPTC native-module rules (follow exactly):\n- Import only modules listed below with a static top-level ES import, for example `import {{ list, update }} from \"todo\";` or `import * as todo from \"todo\";`. Never import `pi-extension`. Do not use `await import(...)`, `require(...)`, or dynamic module lookup for loaded extensions.\n- Call native functions with positional JavaScript arguments in the order shown by `parameters`; do not pass the metadata object. For example, use `await update(0, \"completed\")`, not `update({{ index: 0, status: \"completed\" }})`.\n- Every native function is async and must be awaited before its value is used. The `output_schema` field is authoritative. Functions whose output schema is `null` resolve to `null` and have no useful result; call them as `await update(0, \"completed\")` and do not print or assign the result. Use `console.log` only for meaningful values returned by a function or for the final program result.\n- The following JSON describes the loaded modules, functions, positional parameters, input schemas, and output schemas:\n{}",
                serde_json::to_string_pretty(&self.tools()).expect("tool metadata must serialize")
            ),
            input: ToolInput::Json(serde_json::json!({
                "type": "object",
                "properties": { "code": { "type": "string" } },
                "required": ["code"],
                "additionalProperties": false
            })),
        }]
    }

    async fn call(
        &self,
        session: SessionId,
        _name: &str,
        code: String,
    ) -> Result<ToolOutput, Self::Error> {
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
        self.extensions
            .iter()
            .map(|extension| extension.metadata.system_prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty())
            .collect()
    }

    async fn drop_session(&self, session: SessionId) -> Result<(), Self::Error> {
        for extension in &self.extensions {
            unsafe { (extension.abi.drop_session)(session.0) };
        }
        Ok(())
    }
}

impl ProgrammaticToolExecutor {
    pub fn native_modules(&self) -> Vec<NativeModule> {
        self.extensions
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
            .collect()
    }

    pub fn native_call(&self, session: SessionId) -> NativeCall {
        let extensions = self
            .extensions
            .iter()
            .map(|extension| (extension.metadata.name.clone(), extension.clone()))
            .collect::<HashMap<_, _>>();
        Arc::new(move |module, tool, input| {
            let extension = extensions.get(&module).cloned();
            Box::pin(async move {
                let extension =
                    extension.ok_or_else(|| format!("unknown native extension {module}"))?;
                call_extension(extension, session, &tool, input)
                    .await
                    .map_err(|error| format!("{module}.{tool}: {error:#}"))
            })
        })
    }

    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
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
        if let Some(existing) = self
            .extensions
            .iter_mut()
            .find(|extension| extension.metadata.name == loaded.metadata.name)
        {
            *existing = loaded;
        } else {
            self.extensions.push(loaded);
        }
        Ok(())
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
        let _guard = NODE.lock().await;
        self.set_cancelled(false);

        let modules = self.native_modules();
        let call = self.native_call(session);

        let ProgramOutput { stdout, stderr } =
            execute_program_with_hostcalls(code, &modules, call, execute_hostcall)
                .await
                .map_err(anyhow::Error::new)?;
        Ok(PTCOutput { stdout, stderr })
    }
}

pub async fn execute_hostcall(request: HostcallRequest) -> Vec<HostcallOutcome> {
    let method = request.method();
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

fn validate_metadata(extension: &ToolExtension) -> Result<()> {
    if extension.name.trim().is_empty() {
        bail!("extension name is empty");
    }
    if extension.description.trim().is_empty() {
        bail!("{} extension description is empty", extension.name);
    }
    let mut names = BTreeSet::new();
    for function in &extension.functions {
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

    use e_agent_core::tool::ToolExecutor;

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
    fn describes_static_imports_positional_calls_and_output_schemas() {
        let description = ProgrammaticToolExecutor::default()
            .tool_defs()
            .remove(0)
            .description;
        assert!(description.contains("static top-level ES import"));
        assert!(description.contains("do not pass the metadata object"));
        assert!(description.contains("output schema is `null`"));
        assert!(description.contains("input schemas, and output schemas"));
    }

    #[tokio::test]
    async fn exposes_node_and_runs_typescript_with_native_module() {
        let _guard = TEST_EXECUTION.lock().await;
        let executor = built_executor();
        assert_eq!(executor.tool_defs()[0].name, "node");
        let node_description = executor.tool_defs().remove(0).description;
        assert!(node_description.contains("one complete TypeScript ES module"));
        assert!(node_description.contains("multiple native tool calls"));
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

        let output = executor
            .execute(
                SessionId::next(),
                r#"import { list } from "todo"; await list({});"#,
            )
            .await
            .unwrap();
        assert!(output.stdout.is_empty());
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
        assert_eq!(executor.execute(first, recall).await.unwrap().stdout, "\n");
        executor.load(&probe).unwrap();
        assert_eq!(executor.tools().len(), 1);
        assert_eq!(executor.system_prompts().len(), 1);
    }
}
