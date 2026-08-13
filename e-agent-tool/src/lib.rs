extern crate self as e_agent_tool;

mod context;
#[cfg(test)]
mod macro_tests;
mod state;

use std::future::Future;

pub use anyhow::{Context, Result, anyhow};
pub use context::{Cancelled, cancel, cancelled, progress, reset, subscribe_cancel};
pub use e_agent_macros::{extension, state, tool};
pub use schemars::JsonSchema;
pub use serde::{Deserialize, Serialize};
pub use state::{
    SessionId, SessionStates, clear_current_session, current_session, set_current_session,
};

use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[doc(hidden)]
pub mod __private {
    pub use anyhow;
    pub use pyo3;
    pub use schemars;
    pub use serde;
    pub use serde_json;
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub requires_await: bool,
    pub schema: Value,
    pub output_schema: Value,
}

/// Model-visible description of one loaded extension.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolExtension {
    pub name: String,
    pub description: String,
    /// Extra system-prompt text contributed by the extension. May be empty.
    #[serde(default)]
    pub system_prompt: String,
    pub functions: Vec<ToolFunction>,
}

pub trait Tool {
    type Input: JsonSchema + Send + 'static;
    type Output: JsonSchema + Serialize + Send + 'static;

    const NAME: &'static str;
    const DESCRIPTION: &'static str;

    fn call(input: Self::Input) -> impl Future<Output = Result<Self::Output>> + Send;
}

pub fn tool_function<T: Tool>() -> Result<ToolFunction> {
    Ok(ToolFunction {
        name: T::NAME.to_string(),
        description: T::DESCRIPTION.to_string(),
        requires_await: true,
        schema: serde_json::to_value(schemars::schema_for!(T::Input))?,
        output_schema: serde_json::to_value(schemars::schema_for!(T::Output))?,
    })
}

#[doc(hidden)]
pub fn input_from_python<T: DeserializeOwned>(
    py: Python<'_>,
    input: &Bound<'_, PyDict>,
) -> PyResult<T> {
    let input: String = py
        .import("json")?
        .call_method1("dumps", (input,))?
        .extract()?;
    serde_json::from_str(&input).map_err(|err| {
        pyo3::exceptions::PyValueError::new_err(format!("invalid tool input: {err}"))
    })
}

pub fn run<T: Tool>(py: Python, input: T::Input) -> PyResult<Py<PyAny>> {
    let locals = pyo3_async_runtimes::TaskLocals::with_running_loop(py)?.copy_context(py)?;
    pyo3_async_runtimes::tokio::future_into_py_with_locals(py, locals, async move {
        tracing::debug!(tool = T::NAME, "Python tool started");
        context::progress(T::NAME, "started");
        let output = context::until_cancelled(T::call(input))
            .await
            .map_err(|err| pyo3::exceptions::PyRuntimeError::new_err(format!("{err:#}")))?;
        tracing::debug!(tool = T::NAME, "Python tool completed");
        context::progress(T::NAME, "completed");
        let output = serde_json::to_string(&output).map_err(|err| {
            pyo3::exceptions::PyValueError::new_err(format!("serialize tool output: {err}"))
        })?;
        Python::attach(|py| {
            py.import("json")?
                .call_method1("loads", (output,))
                .map(Bound::unbind)
        })
    })
    .map(Bound::unbind)
}
