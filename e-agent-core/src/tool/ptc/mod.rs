use std::{collections::BTreeSet, ffi::CString, path::Path};

use anyhow::{Context, Result};
use e_agent_tool::{SessionId, ToolExtension, ToolFunction};
use pyo3::{
    prelude::*,
    types::{PyDict, PyModule},
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    message::{MessageContent, ToolDef, ToolInput},
    tool::{ToolExecutor, ToolOutput},
};

#[derive(Debug, Clone, Serialize)]
pub struct PTCOutput {
    stdout: String,
    stderr: String,
}

/// Serializes Python execution, which owns process-wide stdio and session state.
static PYTHON: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One loaded extension: its model-visible metadata and its Python module.
struct LoadedExtension {
    metadata: ToolExtension,
    module: Py<PyModule>,
}

#[derive(Default)]
pub struct ProgrammaticToolExecutor {
    extensions: Vec<LoadedExtension>,
}

#[async_trait::async_trait]
impl ToolExecutor for ProgrammaticToolExecutor {
    type Error = anyhow::Error;
    fn tool_defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "python".into(),
            description: format!(
                "Run a complete Python script in a normal Python runtime. This is the single interface for both \
                 general-purpose Python execution and all externally provided tools. You may use Python built-ins, \
                 the standard library, and installed packages directly for tasks such as computation, parsing, data \
                 transformation, control flow, network access, filesystem reads and writes, and other suitable work. \
                 The externally provided tools are exposed as the Python modules and functions listed below; import \
                 those modules when their specialized capabilities are useful. Prefer one coherent script that \
                 combines ordinary Python with these tools, chains dependent operations, and keeps intermediate \
                 values in memory instead of printing them merely to pass them into another tool call. Registered \
                 async functions must be awaited, typically via asyncio.run(...). Tool results are JSON-compatible \
                 Python values; JSON Schema objects are dicts, so access fields with result[\"field\"], not \
                 attributes. Call tool functions with normal Python keyword arguments, for example \
                 fn(path=\"file\"), rather than passing a single dict as a positional argument. Print only the \
                 final useful result needed to answer the user.\nSupported tool packages:\n{}",
                serde_json::to_string(&self.tools()).unwrap()
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
        let output = self.execute(session, code).await?;
        let content = serde_json::to_string(&output).context("serialize ptc output failed")?;
        Ok(ToolOutput {
            content: vec![MessageContent::text(content)],
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
        Python::attach(|py| {
            for extension in &self.extensions {
                let module = extension.module.bind(py);
                if let Ok(drop) = module.getattr("__e_agent_drop_session__") {
                    drop.call1((session.0,)).with_context(|| {
                        format!("drop session in {} failed", extension.metadata.name)
                    })?;
                }
            }
            Ok(())
        })
    }
}

impl ProgrammaticToolExecutor {
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path
            .as_ref()
            .canonicalize()
            .with_context(|| format!("tool path does not exist: {}", path.as_ref().display()))?;
        let (module, metadata) = Python::attach(|py| {
            let module = load_module(py, &path)?;
            let metadata = metadata(module.bind(py))?;
            Ok::<_, PyErr>((module, metadata))
        })?;
        self.register(module, metadata)
    }

    pub fn register(&mut self, module: Py<PyModule>, metadata: ToolExtension) -> Result<()> {
        let metadata = Python::attach(|py| {
            let bound = module.bind(py);
            let name = bound.name()?.to_string();
            validate_module(bound, &metadata.functions)?;
            py.import("sys")?
                .getattr("modules")?
                .set_item(&name, bound)?;
            Ok::<_, PyErr>(ToolExtension { name, ..metadata })
        })?;

        let loaded = LoadedExtension { metadata, module };
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

    /// Bind `session` in every loaded extension for the duration of one script.
    fn bind_session(py: Python<'_>, extensions: &[&str], session: SessionId) -> PyResult<()> {
        let modules = py.import("sys")?.getattr("modules")?;
        for name in extensions {
            let module = modules.get_item(name)?;
            if let Ok(bind) = module.getattr("__e_agent_set_session__") {
                bind.call1((session.0,))?;
            }
        }
        Ok(())
    }

    fn unbind_session(py: Python<'_>, extensions: &[&str]) {
        let Ok(modules) = py.import("sys").and_then(|sys| sys.getattr("modules")) else {
            return;
        };
        for name in extensions {
            if let Ok(module) = modules.get_item(name)
                && let Ok(clear) = module.getattr("__e_agent_clear_session__")
            {
                let _ = clear.call0();
            }
        }
    }

    async fn execute(&self, session: SessionId, code: impl Into<Vec<u8>>) -> Result<PTCOutput> {
        let code =
            CString::new(code).context("python code raw bytes contains an internal 0 byte")?;
        let names: Vec<String> = self
            .extensions
            .iter()
            .map(|extension| extension.metadata.name.clone())
            .collect();
        // Output capture and the hidden current-session slot are both process
        // wide, so only one script may run at a time.
        //
        // ponytail: one global lock across every executor and session, which the
        // shared interpreter already implies; revisit if independent sessions
        // ever need concurrent Python execution.
        let _guard = PYTHON.lock().await;
        let (stdout, stderr) =
            tokio::task::spawn_blocking(move || -> PyResult<(String, String)> {
                Python::attach(|py| {
                    let names: Vec<&str> = names.iter().map(String::as_str).collect();
                    let globals = PyDict::new(py);
                    globals.set_item("__name__", "__main__")?;
                    globals.set_item("__file__", "<tool>")?;

                    let sys = py.import("sys")?;
                    let origin_stdout = sys.getattr("stdout")?;
                    let origin_stderr = sys.getattr("stderr")?;
                    let stdout = py.import("io")?.getattr("StringIO")?.call0()?;
                    let stderr = py.import("io")?.getattr("StringIO")?.call0()?;
                    sys.setattr("stdout", &stdout)?;
                    sys.setattr("stderr", &stderr)?;

                    let bind_result = Self::bind_session(py, &names, session);
                    let run_result = bind_result
                        .and_then(|()| py.run(code.as_c_str(), Some(&globals), Some(&globals)));
                    Self::unbind_session(py, &names);
                    let restore_stdout = sys.setattr("stdout", origin_stdout);
                    let restore_stderr = sys.setattr("stderr", origin_stderr);

                    restore_stdout?;
                    restore_stderr?;
                    run_result?;

                    let stdout: String = stdout.call_method0("getvalue")?.extract()?;
                    let stderr: String = stderr.call_method0("getvalue")?.extract()?;

                    Ok((stdout, stderr))
                })
            })
            .await
            .context("Python interpreter failed to execute to completion.")?
            .context("Python code execution failed")?;
        let output = PTCOutput { stdout, stderr };
        Ok(output)
    }
}

fn load_module(py: Python<'_>, path: &Path) -> PyResult<Py<PyModule>> {
    let is_package = path.is_dir();
    let source = if is_package {
        path.join("__init__.py")
    } else {
        path.to_owned()
    };
    if is_package && !source.is_file() {
        return Err(pyo3::exceptions::PyImportError::new_err(format!(
            "{} has no __init__.py",
            path.display()
        )));
    }

    let name = module_name(path)?;
    let source = source.to_string_lossy();
    let util = py.import("importlib.util")?;
    let spec = if is_package || path.extension().is_some_and(|ext| ext == "py") {
        let kwargs = PyDict::new(py);
        if is_package {
            kwargs.set_item(
                "submodule_search_locations",
                [path.to_string_lossy().to_string()],
            )?;
        }
        util.call_method(
            "spec_from_file_location",
            (&name, source.as_ref()),
            Some(&kwargs),
        )?
    } else {
        let loader = py
            .import("importlib.machinery")?
            .getattr("ExtensionFileLoader")?
            .call1((&name, source.as_ref()))?;
        util.call_method1("spec_from_loader", (&name, loader))?
    };
    if spec.is_none() {
        return Err(pyo3::exceptions::PyImportError::new_err(format!(
            "cannot create module spec for {}",
            path.display()
        )));
    }

    let module = util
        .call_method1("module_from_spec", (&spec,))?
        .cast_into::<PyModule>()?;
    let modules = py.import("sys")?.getattr("modules")?;
    modules.set_item(&name, &module)?;
    let loader = spec.getattr("loader")?;
    if let Err(error) = loader.call_method1("exec_module", (&module,)) {
        modules.del_item(&name).ok();
        return Err(error);
    }
    Ok(module.unbind())
}

fn module_name(path: &Path) -> PyResult<String> {
    let name = if path.is_dir() {
        path.file_name()
    } else {
        path.file_stem()
    }
    .and_then(|name| name.to_str())
    .and_then(|name| name.split('.').next())
    .filter(|name| !name.is_empty())
    .ok_or_else(|| {
        pyo3::exceptions::PyImportError::new_err(format!(
            "cannot determine module name from {}",
            path.display()
        ))
    })?;
    Ok(name.to_string())
}

fn metadata(module: &Bound<'_, PyModule>) -> PyResult<ToolExtension> {
    let name = module.name()?.to_string();
    let value = module.getattr("__e_agent_extension__").map_err(|_| {
        pyo3::exceptions::PyImportError::new_err(format!(
            "{name} does not define __e_agent_extension__"
        ))
    })?;
    let json = match value.extract::<String>() {
        Ok(json) => json,
        Err(_) => module
            .py()
            .import("json")?
            .call_method1("dumps", (value,))?
            .extract()?,
    };
    let extension: ToolExtension = serde_json::from_str(&json).map_err(|error| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "invalid __e_agent_extension__ in {name}: {error}"
        ))
    })?;
    if extension.description.trim().is_empty() {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "{name} extension description is empty"
        )));
    }
    let mut seen = BTreeSet::new();
    for function in &extension.functions {
        if !seen.insert(function.name.as_str()) {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{name} declares duplicate tool {}",
                function.name
            )));
        }
    }
    Ok(extension)
}

fn validate_module(module: &Bound<'_, PyModule>, functions: &[ToolFunction]) -> PyResult<()> {
    let inspect = module.py().import("inspect")?;
    let is_routine = inspect.getattr("isroutine")?;
    let signature = inspect.getattr("signature")?;
    let empty = inspect.getattr("_empty")?;
    let declared: BTreeSet<_> = functions
        .iter()
        .map(|function| function.name.as_str())
        .collect();
    let mut exported = BTreeSet::new();

    for (name, value) in module.dict().iter() {
        let name: String = name.extract()?;
        if !name.starts_with('_') && is_routine.call1((&value,))?.is_truthy()? {
            exported.insert(name);
        }
    }

    if exported.iter().map(String::as_str).collect::<BTreeSet<_>>() != declared {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "{} exports {:?}, but typed tools are {:?}",
            module.name()?,
            exported,
            declared
        )));
    }

    for function in functions {
        let callable = module.getattr(function.name.as_str())?;
        let parameters = signature.call1((callable,))?.getattr("parameters")?;
        let mut actual = BTreeSet::new();
        let mut required = BTreeSet::new();
        for item in parameters.call_method0("items")?.try_iter()? {
            let (name, parameter): (String, Bound<'_, PyAny>) = item?.extract()?;
            let kind: String = parameter.getattr("kind")?.getattr("name")?.extract()?;
            if matches!(kind.as_str(), "VAR_POSITIONAL" | "VAR_KEYWORD") {
                continue;
            }
            if parameter.getattr("default")?.is(&empty) {
                required.insert(name.clone());
            }
            actual.insert(name);
        }

        if function.schema["type"] != "object" {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{} input schema must be an object",
                function.name
            )));
        }
        let expected: BTreeSet<_> = function.schema["properties"]
            .as_object()
            .into_iter()
            .flat_map(|properties| properties.keys().cloned())
            .collect();
        let expected_required: BTreeSet<_> = function.schema["required"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();

        if actual != expected || required != expected_required {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "{} signature does not match its generated input schema",
                function.name
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, process::Command};

    use e_agent_tool::SessionId;
    use pyo3::prelude::*;

    use crate::tool::ToolExecutor;

    use super::ProgrammaticToolExecutor;

    async fn execute_and_get_output(
        executor: &ProgrammaticToolExecutor,
        code: impl ToString,
    ) -> String {
        let output = executor
            .call(SessionId::next(), "", code.to_string())
            .await
            .unwrap()
            .content
            .pop()
            .unwrap();
        match output {
            crate::message::MessageContent::Text { text } => text,
            _ => unreachable!(),
        }
    }

    #[test]
    fn loads_and_runs_basic_tools() {
        Python::initialize();
        let executor = built_executor();

        let tools = executor.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "basic_tools");
        assert_eq!(
            tools[0]
                .functions
                .iter()
                .map(|function| function.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "write", "edit", "bash"]
        );
        assert_eq!(
            tools[0].functions[0].schema["required"],
            serde_json::json!(["path"])
        );
        assert_eq!(
            tools[0].functions[3].schema["required"],
            serde_json::json!(["command"])
        );

        let fixture =
            std::env::temp_dir().join(format!("e-agent-basic-tools-test-{}", std::process::id()));
        std::fs::create_dir_all(&fixture).unwrap();
        let file_path = fixture.join("sample.txt");
        let image_path = fixture.join("sample.png");
        let marker_path = fixture.join("orphan.txt");
        std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nfixture").unwrap();
        let file = serde_json::to_string(&file_path.to_string_lossy().replace('\\', "/")).unwrap();
        let image =
            serde_json::to_string(&image_path.to_string_lossy().replace('\\', "/")).unwrap();
        let marker =
            serde_json::to_string(&marker_path.to_string_lossy().replace('\\', "/")).unwrap();
        let code = format!(
            r#"import asyncio, os, basic_tools
async def main():
    marker = {marker}
    print(await basic_tools.write({file}, "alpha\n"))
    print(await basic_tools.read({file}))
    print(await basic_tools.edit({file}, [{{"old_text": "alpha", "new_text": "beta"}}]))
    ordered = await basic_tools.bash("printf 'out-1\\n'; sleep 0.1; printf 'err-1\\n' >&2; sleep 0.1; printf 'out-2\\n'")
    print("ordered=" + ordered)
    image = await basic_tools.read({image})
    print("image=" + image["mime_type"] + ":" + image["data"][:4])
    print(await basic_tools.bash("for i in $(seq 1 2100); do echo $i; done"))
    print("utf8=" + (await basic_tools.bash("printf '中文\\n'"))[:2])
    inherited = await basic_tools.bash("(sleep 5; echo late) & echo early", timeout=10)
    print("inherited=" + inherited.strip())
    try:
        await basic_tools.bash(f"(sleep 1; printf orphan > '{{marker}}') & wait", timeout=0.1)
    except RuntimeError as error:
        print("timeout=" + str(error).splitlines()[-1])
    await asyncio.sleep(1.2)
    print("orphan=" + str(os.path.exists(marker)))
    print(await basic_tools.read({file}, 1, 1))
    fuzzy = await basic_tools.write({file}, "let a = \u201cx\u201d;\n")
    result = await basic_tools.edit({file}, [{{"old_text": 'let a = "x";', "new_text": "let a = 1;"}}])
    print("fuzzy=" + str(result["used_fuzzy_match"]) + " line=" + str(result["first_changed_line"]))
    print("diff=" + result["diff"].replace("\n", "|"))
asyncio.run(main())"#
        );

        Python::attach(|py| {
            pyo3_async_runtimes::tokio::run(py, async move {
                let output = executor.execute(SessionId::next(), code).await.unwrap();
                let output = format!("{}{}", output.stdout, output.stderr);
                assert!(output.contains("Successfully wrote"));
                assert!(output.contains("Successfully replaced"));
                assert!(output.contains("ordered=out-1\nerr-1\nout-2"));
                assert!(output.contains("image=image/png:iVBO"));
                assert!(output.contains("[Output truncated. Full output:"));
                assert!(output.contains("utf8=\u{4e2d}\u{6587}"));
                assert!(output.contains("inherited=early"));
                assert!(output.contains("fuzzy=True line=1"));
                assert!(output.contains("+1 let a = 1;"));
                assert!(output.contains("command timed out after 0.1 seconds"));
                assert!(output.contains("orphan=False"));
                Ok(())
            })
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.join("sample.txt")).unwrap(),
            "let a = 1;\n"
        );
        std::fs::remove_dir_all(fixture).unwrap();
    }

    /// Build the basic tools extension and load it into a fresh executor.
    fn built_executor() -> ProgrammaticToolExecutor {
        let mut executor = ProgrammaticToolExecutor::default();
        executor
            .load(build_extension("e-agent-basic-tools", "basic_tools"))
            .unwrap();
        executor
    }

    /// Build one extension cdylib and stage it as an importable Python module.
    ///
    /// Windows locks a loaded `.pyd`, so the copy only happens once per build.
    fn build_extension(package: &str, library_name: &str) -> PathBuf {
        static BUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = BUILD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let target = root.join(format!("target/{library_name}-test"));
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", package, "--target-dir"])
            .arg(&target)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        #[cfg(windows)]
        let (library, extension) = (
            target.join(format!("debug/{library_name}.dll")),
            target.join(format!("debug/{library_name}.pyd")),
        );
        #[cfg(target_os = "linux")]
        let (library, extension) = {
            let library = target.join(format!("debug/lib{library_name}.so"));
            (library.clone(), library)
        };
        #[cfg(target_os = "macos")]
        let (library, extension) = (
            target.join(format!("debug/lib{library_name}.dylib")),
            target.join(format!("debug/{library_name}.so")),
        );
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        let (library, extension) = {
            let library = target.join(format!("debug/lib{library_name}.so"));
            (library.clone(), library)
        };
        if library != extension
            && std::fs::copy(&library, &extension).is_err()
            && !extension.is_file()
        {
            panic!("cannot stage {}", extension.display());
        }
        extension
    }

    /// Every workspace extension loads and exposes its tools after migration.
    #[test]
    fn loads_every_workspace_extension() {
        Python::initialize();
        let mut executor = ProgrammaticToolExecutor::default();
        executor
            .load(build_extension("e-agent-basic-tools", "basic_tools"))
            .unwrap();
        executor
            .load(build_extension("e-my-ext", "my_ext"))
            .unwrap();
        executor
            .load(build_extension("e-web-access", "web_access"))
            .unwrap();

        let extensions = executor.tools();
        let tools: Vec<(&str, Vec<&str>)> = extensions
            .iter()
            .map(|extension| {
                (
                    extension.name.as_str(),
                    extension
                        .functions
                        .iter()
                        .map(|function| function.name.as_str())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            tools,
            [
                ("basic_tools", vec!["read", "write", "edit", "bash"]),
                ("my_ext", vec!["weather", "get_attraction"]),
                ("web_access", vec!["web_search", "web_fetch"]),
            ]
        );
        // Doc comments stay the tool description and #[desc] stays the parameter description.
        assert_eq!(
            extensions[1].functions[0].description,
            "异步查询指定城市的实时天气"
        );
        assert_eq!(
            extensions[1].functions[0].schema["properties"]["city"]["description"],
            "需要查询实时天气的城市名称"
        );
        assert_eq!(executor.system_prompts().len(), 3);

        // Executing a migrated stateless tool still works end to end.
        Python::attach(|py| {
            pyo3_async_runtimes::tokio::run(py, async move {
                let code = "import asyncio, basic_tools
async def main():
    print(await basic_tools.bash(\"printf migrated\"))
asyncio.run(main())";
                assert!(
                    executor
                        .execute(SessionId::next(), code)
                        .await
                        .unwrap()
                        .stdout
                        .contains("migrated")
                );
                Ok(())
            })
        })
        .unwrap();
    }

    #[test]
    fn loads_and_runs_pure_python_package() {
        Python::initialize();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pure_tools");
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load(path).unwrap();

        let tools = executor.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "pure_tools");
        assert_eq!(tools[0].description, "Pure Python test tools.");
        assert_eq!(tools[0].functions[0].name, "multiply");
        assert_eq!(
            tools[0].functions[0].schema["properties"]["x"]["description"],
            "first factor"
        );

        Python::attach(|py| {
            pyo3_async_runtimes::tokio::run(py, async move {
                let code =
                    "import asyncio, pure_tools\nasync def main():\n    print(await pure_tools.multiply(6, 7))\nasyncio.run(main())";
                assert_eq!(executor.execute(SessionId::next(), code).await.unwrap().stdout, "42\n");

                // let code = "raise ValueError('bad code')";
                // execute_and_get_output(&executor, code).await;
                let code = "print('restored')";
                assert_eq!(executor.execute(SessionId::next(), code).await.unwrap().stdout, "restored\n");
                Ok(())
            })
        })
        .unwrap();
    }

    /// Extension metadata, including its system prompt, round-trips through PyO3.
    #[test]
    fn round_trips_extension_metadata() {
        Python::initialize();
        let mut executor = ProgrammaticToolExecutor::default();
        executor
            .load(build_extension("e-state-probe", "state_probe"))
            .unwrap();

        let extensions = executor.tools();
        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0].name, "state_probe");
        assert_eq!(extensions[0].description, "Remember values per session");
        assert_eq!(
            extensions[0].system_prompt,
            "Use state_probe to remember values inside one session."
        );
        assert_eq!(
            extensions[0]
                .functions
                .iter()
                .map(|function| function.name.as_str())
                .collect::<Vec<_>>(),
            ["remember", "recall"]
        );
        // The state parameter stays out of the model-visible schema.
        assert_eq!(
            extensions[0].functions[0].schema["required"],
            serde_json::json!(["value"])
        );
        assert!(
            !extensions[0].functions[0].schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("state")
        );
        assert_eq!(
            extensions[0].functions[1].schema["properties"]
                .as_object()
                .map_or(0, |properties| properties.len()),
            0
        );

        // The Python signatures must match the schemas, with no state parameter.
        // `validate_module` already enforces this during load; assert it directly.
        Python::attach(|py| {
            let signature = py.import("inspect").unwrap().getattr("signature").unwrap();
            let module = py.import("state_probe").unwrap();
            for (tool, expected) in [("remember", "(value)"), ("recall", "()")] {
                let rendered = signature
                    .call1((module.getattr(tool).unwrap(),))
                    .unwrap()
                    .str()
                    .unwrap()
                    .to_string();
                assert_eq!(rendered, expected);
            }
        });
    }

    /// State is shared inside one session, isolated across sessions, and dropped per session.
    #[test]
    fn isolates_and_drops_session_state() {
        Python::initialize();
        let mut executor = ProgrammaticToolExecutor::default();
        executor
            .load(build_extension("e-state-probe", "state_probe"))
            .unwrap();

        let first = SessionId::next();
        let second = SessionId::next();
        // Tool coroutines must be created inside a running loop, so every script
        // awaits them from one `main()`.
        let remember = |value: &str| {
            format!(
                "import asyncio, state_probe
async def main():
    print(await state_probe.remember({value}))
asyncio.run(main())",
                value = serde_json::to_string(value).unwrap()
            )
        };
        let recall = "import asyncio, state_probe
async def main():
    print('recall=' + await state_probe.recall())
asyncio.run(main())";

        Python::attach(|py| {
            pyo3_async_runtimes::tokio::run(py, async move {
                assert_eq!(
                    executor.execute(first, remember("a")).await.unwrap().stdout,
                    "a
"
                );
                assert_eq!(
                    executor.execute(first, remember("b")).await.unwrap().stdout,
                    "a,b
"
                );

                // A second session cannot observe the first session's state.
                assert_eq!(
                    executor.execute(second, recall).await.unwrap().stdout,
                    "recall=
"
                );
                assert_eq!(
                    executor
                        .execute(second, remember("z"))
                        .await
                        .unwrap()
                        .stdout,
                    "z
"
                );
                assert_eq!(
                    executor.execute(first, recall).await.unwrap().stdout,
                    "recall=a,b
"
                );

                // Dropping one session clears only that session's slot.
                executor.drop_session(first).await.unwrap();
                assert_eq!(
                    executor.execute(first, recall).await.unwrap().stdout,
                    "recall=
"
                );
                assert_eq!(
                    executor.execute(second, recall).await.unwrap().stdout,
                    "recall=z
"
                );
                Ok(())
            })
        })
        .unwrap();
    }

    /// A failing script still clears the bound session, and the next session's
    /// state is unaffected.
    #[test]
    fn clears_bound_session_on_error() {
        Python::initialize();
        let mut executor = ProgrammaticToolExecutor::default();
        executor
            .load(build_extension("e-state-probe", "state_probe"))
            .unwrap();

        let failing = SessionId::next();
        let next = SessionId::next();
        let remember = "import asyncio, state_probe
async def main():
    await state_probe.remember('leaked')
    raise ValueError('boom')
asyncio.run(main())";
        let recall = "import asyncio, state_probe
async def main():
    print('recall=' + await state_probe.recall())
asyncio.run(main())";

        Python::attach(|py| {
            pyo3_async_runtimes::tokio::run(py, async move {
                assert!(executor.execute(failing, remember).await.is_err());

                // The failed session kept its own state, and no session is bound now.
                assert_eq!(e_agent_tool::current_session(), SessionId(0));
                assert_eq!(
                    executor.execute(next, recall).await.unwrap().stdout,
                    "recall=
"
                );
                assert_eq!(
                    executor.execute(failing, recall).await.unwrap().stdout,
                    "recall=leaked
"
                );
                Ok(())
            })
        })
        .unwrap();
    }

    /// Reloading one extension replaces its metadata instead of duplicating it,
    /// and system prompts follow load order.
    #[test]
    fn replaces_reloaded_extension_and_orders_prompts() {
        Python::initialize();
        let probe = build_extension("e-state-probe", "state_probe");
        let pure = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pure_tools");
        let mut executor = ProgrammaticToolExecutor::default();
        executor.load(&probe).unwrap();
        executor.load(&pure).unwrap();
        executor.load(&probe).unwrap();

        let names: Vec<_> = executor
            .tools()
            .into_iter()
            .map(|extension| extension.name)
            .collect();
        assert_eq!(names, ["state_probe", "pure_tools"]);
        assert_eq!(
            executor.system_prompts(),
            [
                "Use state_probe to remember values inside one session.",
                "Use pure_tools.multiply for exact integer products.",
            ]
        );
    }

    /// A missing or malformed metadata export is reported with the module name.
    #[test]
    fn rejects_module_without_extension_metadata() {
        Python::initialize();
        let directory = std::env::temp_dir().join(format!(
            "e-agent-bad-extension-{}-{}",
            std::process::id(),
            SessionId::next()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let module = directory.join("no_metadata.py");
        std::fs::write(
            &module,
            "async def noop():
    return None
",
        )
        .unwrap();

        let error = format!(
            "{:#}",
            ProgrammaticToolExecutor::default()
                .load(&module)
                .unwrap_err()
        );
        assert!(
            error.contains("no_metadata does not define __e_agent_extension__"),
            "unexpected error: {error}"
        );

        std::fs::write(
            &module,
            "async def noop():
    return None

__e_agent_extension__ = '{'
",
        )
        .unwrap();
        let error = format!(
            "{:#}",
            ProgrammaticToolExecutor::default()
                .load(&module)
                .unwrap_err()
        );
        assert!(
            error.contains("invalid __e_agent_extension__ in no_metadata"),
            "unexpected error: {error}"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
