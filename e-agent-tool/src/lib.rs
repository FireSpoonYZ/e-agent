extern crate self as e_agent_tool;

use std::future::Future;

pub use anyhow::{Context, Result, anyhow};
pub use e_agent_macros::tool;
pub use schemars::JsonSchema;
pub use serde::{Deserialize, Serialize};

use pyo3::prelude::*;
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
    pub schema: Value,
    pub output_schema: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolModule {
    pub name: String,
    pub description: String,
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
        schema: serde_json::to_value(schemars::schema_for!(T::Input))?,
        output_schema: serde_json::to_value(schemars::schema_for!(T::Output))?,
    })
}

pub fn run<T: Tool>(py: Python, input: T::Input) -> PyResult<Py<PyAny>> {
    let locals = pyo3_async_runtimes::TaskLocals::with_running_loop(py)?.copy_context(py)?;
    pyo3_async_runtimes::tokio::future_into_py_with_locals(py, locals, async move {
        tracing::debug!(tool = T::NAME, "Python tool started");
        let output = T::call(input)
            .await
            .map_err(|err| pyo3::exceptions::PyRuntimeError::new_err(format!("{err:#}")))?;
        tracing::debug!(tool = T::NAME, "Python tool completed");
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

#[macro_export]
macro_rules! extension {
    ($name:ident, [$($tool:ident),* $(,)?]) => {
        #[$crate::__private::pyo3::pymodule]
        pub fn $name(
            module: &$crate::__private::pyo3::Bound<'_, $crate::__private::pyo3::types::PyModule>
        ) -> $crate::__private::pyo3::PyResult<()> {
            use $crate::__private::pyo3::types::PyModuleMethods as _;

            $(module.add_function($crate::__private::pyo3::wrap_pyfunction!($tool::python, module)?)?;)*
            let functions = vec![$($crate::tool_function::<$tool::Definition>()
                .map_err(|err| $crate::__private::pyo3::exceptions::PyValueError::new_err(format!("{err:#}")))?),*];
            let metadata = $crate::__private::serde_json::to_string(&functions)
                .map_err(|err| $crate::__private::pyo3::exceptions::PyValueError::new_err(err.to_string()))?;
            module.add("__e_agent_tools__", metadata)
        }
    };
}
