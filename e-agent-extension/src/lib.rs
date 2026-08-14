extern crate self as e_agent_extension;

mod abi;
mod context;
#[cfg(test)]
mod macro_tests;
mod state;

use std::future::Future;

pub use abi::{AbiBuffer, CompletionCallback, EXTENSION_ABI_VERSION, ExtensionV1, free_buffer};
pub use anyhow::{Context, Result, anyhow};
pub use context::{Cancelled, cancel, cancelled, progress, reset, subscribe_cancel};
pub use e_agent_macros::{extension, state, tool};
pub use schemars::JsonSchema;
pub use serde::{Deserialize, Serialize};
pub use state::{
    SessionId, SessionStates, clear_current_session, current_session, set_current_session,
};

use serde_json::Value;

#[doc(hidden)]
pub mod __private {
    pub use anyhow;
    pub use schemars;
    pub use serde;
    pub use serde_json;
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub requires_await: bool,
    pub parameters: Vec<String>,
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
    const PARAMETERS: &'static [&'static str];

    fn call(input: Self::Input) -> impl Future<Output = Result<Self::Output>> + Send + 'static;
}

pub fn tool_function<T: Tool>() -> Result<ToolFunction> {
    Ok(ToolFunction {
        name: T::NAME.to_string(),
        description: T::DESCRIPTION.to_string(),
        requires_await: true,
        parameters: T::PARAMETERS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        schema: serde_json::to_value(schemars::schema_for!(T::Input))?,
        output_schema: serde_json::to_value(schemars::schema_for!(T::Output))?,
    })
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("e-agent-extension")
            .build()
            .expect("create extension runtime")
    })
}

#[doc(hidden)]
pub unsafe fn start_tool_call<T: Tool>(
    session: u64,
    input: &[u8],
    callback: CompletionCallback,
    user_data: *mut std::ffi::c_void,
) where
    T::Input: serde::de::DeserializeOwned,
{
    let input = match serde_json::from_slice::<T::Input>(input) {
        Ok(input) => input,
        Err(error) => {
            unsafe {
                callback(
                    user_data,
                    AbiBuffer::from_string(format!("invalid input: {error}")),
                    true,
                );
            }
            return;
        }
    };
    let user_data = user_data as usize;
    let call = runtime().spawn(async move {
        set_current_session(SessionId(session));
        tracing::debug!(tool = T::NAME, "extension tool started");
        progress(T::NAME, "started");
        match context::until_cancelled(T::call(input)).await {
            Ok(output) => serde_json::to_vec(&output)
                .map_err(|error| format!("serialize tool output: {error}")),
            Err(error) => Err(format!("{error:#}")),
        }
    });
    runtime().spawn(async move {
        let result = match call.await {
            Ok(result) => result,
            Err(error) => Err(format!("tool task failed: {error}")),
        };
        let (buffer, is_error) = match result {
            Ok(bytes) => {
                progress(T::NAME, "completed");
                (AbiBuffer::from_vec(bytes), false)
            }
            Err(error) => (AbiBuffer::from_string(error), true),
        };
        unsafe {
            callback(user_data as *mut std::ffi::c_void, buffer, is_error);
        }
    });
}
