use std::{collections::BTreeSet, ffi::CStr, path::Path};

use anyhow::{Context, Result};
use e_agent_tool::{ToolFunction, ToolModule};
use pyo3::{
    prelude::*,
    types::{PyDict, PyModule},
};
use serde_json::Value;

static CALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
pub struct ToolExecutor {
    modules: Vec<ToolModule>,
}

impl ToolExecutor {
    pub fn load(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path
            .as_ref()
            .canonicalize()
            .with_context(|| format!("tool path does not exist: {}", path.as_ref().display()))?;
        let (module, functions) = Python::attach(|py| {
            let module = load_module(py, &path)?;
            let functions = metadata(module.bind(py))?;
            Ok::<_, PyErr>((module, functions))
        })?;
        self.register(&module, functions)
    }

    pub fn register(&mut self, module: &Py<PyModule>, functions: Vec<ToolFunction>) -> Result<()> {
        let tool_module = Python::attach(|py| {
            let module = module.bind(py);
            let name = module.name()?.to_string();
            validate_module(module, &functions)?;
            py.import("sys")?
                .getattr("modules")?
                .set_item(&name, module)?;
            Ok::<_, PyErr>(ToolModule {
                name,
                description: doc(module.as_any())?,
                functions,
            })
        })?;

        if let Some(existing) = self
            .modules
            .iter_mut()
            .find(|module| module.name == tool_module.name)
        {
            *existing = tool_module;
        } else {
            self.modules.push(tool_module);
        }
        Ok(())
    }

    pub fn tools(&self) -> Vec<ToolModule> {
        self.modules.clone()
    }

    pub async fn call(&self, code: &CStr) -> Result<String> {
        // sys.stdout and sys.stderr are process-global, so captures must not overlap.
        let _guard = CALL_LOCK.lock().await;
        let code = code.to_owned();

        Ok(tokio::task::spawn_blocking(move || {
            Python::attach(|py| {
                let globals = PyDict::new(py);
                globals.set_item("__name__", "__main__")?;
                globals.set_item("__file__", "<tool>")?;

                let sys = py.import("sys")?;
                let stdout = sys.getattr("stdout")?;
                let stderr = sys.getattr("stderr")?;
                let output = py.import("io")?.getattr("StringIO")?.call0()?;
                sys.setattr("stdout", &output)?;
                sys.setattr("stderr", &output)?;

                let run_result = py.run(&code, Some(&globals), Some(&globals));
                let restore_stdout = sys.setattr("stdout", stdout);
                let restore_stderr = sys.setattr("stderr", stderr);

                run_result?;
                restore_stdout?;
                restore_stderr?;
                output.call_method0("getvalue")?.extract()
            })
        })
        .await??)
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

fn metadata(module: &Bound<'_, PyModule>) -> PyResult<Vec<ToolFunction>> {
    let name = module.name()?.to_string();
    let value = module.getattr("__e_agent_tools__").map_err(|_| {
        pyo3::exceptions::PyImportError::new_err(format!(
            "{name} does not define __e_agent_tools__"
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
    serde_json::from_str(&json).map_err(|error| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid __e_agent_tools__: {error}"))
    })
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

fn doc(value: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(value
        .getattr("__doc__")?
        .extract::<Option<String>>()?
        .unwrap_or_default()
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use std::{ffi::CString, path::PathBuf};

    use pyo3::prelude::*;

    use super::ToolExecutor;

    #[test]
    #[ignore = "run after powershell -File scripts/build-tool.ps1 -Debug"]
    fn loads_compiled_rust_extension_file() {
        crate::initialize_python().unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug/my_ext.pyd");
        let mut executor = ToolExecutor::default();
        executor.load(path).unwrap();
        executor
            .load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pure_tools"))
            .unwrap();

        let tools = executor.tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "my_ext");
        assert_eq!(tools[0].functions.len(), 2);
        assert_eq!(
            tools[0].functions[0].schema["properties"]["city"]["description"],
            "需要查询实时天气的城市名称"
        );
    }

    #[test]
    fn loads_and_runs_pure_python_package() {
        crate::initialize_python().unwrap();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pure_tools");
        let mut executor = ToolExecutor::default();
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
                let code = CString::new(
                    "import asyncio, pure_tools\nasync def main():\n    print(await pure_tools.multiply(6, 7))\nasyncio.run(main())",
                )
                .unwrap();
                assert_eq!(executor.call(&code).await.unwrap(), "42\n");

                let code = CString::new("raise ValueError('bad code')").unwrap();
                assert!(executor.call(&code).await.is_err());
                let code = CString::new("print('restored')").unwrap();
                assert_eq!(executor.call(&code).await.unwrap(), "restored\n");
                Ok(())
            })
        })
        .unwrap();
    }
}
