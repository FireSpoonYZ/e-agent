use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCalls, ChatCompletionRequestAssistantMessage,
        ChatCompletionRequestMessage, ChatCompletionRequestToolMessageArgs, ChatCompletionTool,
        ChatCompletionTools, CreateChatCompletionRequestArgs, FunctionObjectArgs, ReasoningEffort,
    },
};
use pyo3::{PyResult, Python};
use serde::Deserialize;
use std::{ffi::CString, path::PathBuf};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

mod error;
mod tool;

const PROMPT: &str = r#"你是运行在 e（一套 coding agent harness，编码代理宿主程序）中的专家级编码助手。你通过读取文件、执行命令、编辑代码以及写入新文件来帮助用户。"#;

#[derive(Debug, PartialEq)]
struct ModuleConfig {
    model: String,
    reasoning_effort: ReasoningEffort,
}

#[derive(Deserialize)]
struct PythonArgs {
    code: String,
}

#[tokio::main]
async fn main() -> PyResult<()> {
    dotenvy::dotenv().ok();
    init_logging();

    if let Err(err) = f().await {
        error!(error = %format!("{err:#}"), "agent failed");
        return Err(pyo3::exceptions::PyRuntimeError::new_err(err.to_string()));
    }

    info!("agent stopped cleanly");
    Ok(())
}

async fn f() -> error::Result<()> {
    initialize_python()?;

    let mut tool_executor = tool::ToolExecutor::default();
    let tool_path = std::env::var("E_AGENT_TOOL_PATH")
        .map_err(|err| error::Error::Config(format!("E_AGENT_TOOL_PATH is not set: {err}")))?;
    tool_executor
        .load(tool_path)
        .map_err(|err| pyo3::exceptions::PyRuntimeError::new_err(format!("{err:#}")))?;

    println!("schema:\n{:#?}\n\n", tool_executor.tools());

    let config = OpenAIConfig::default().with_api_base("https://congee.pro/v1");
    let http_client = reqwest::ClientBuilder::new()
        .user_agent("codex_cli_rs/0.125.0")
        .build()?;
    let client = Client::with_config(config).with_http_client(http_client);
    let python_tool = python_tool()?;
    let mut messages = vec![
        ChatCompletionRequestMessage::System(
            format!(
                "{}\n可以在`python`工具中使用的package：\n{:?}",
                PROMPT,
                tool_executor.tools()
            )
            .into(),
        ),
        ChatCompletionRequestMessage::User(
            "你好，请帮我查询一下今天北京的天气，然后根据天气推荐一个合适的旅游景点。".into(),
        ),
    ];

    let module_config = module_config("E_MODULE_BIG")
        .map_err(|err| pyo3::exceptions::PyRuntimeError::new_err(format!("{err:#}")))?;

    for i in 1..=5 {
        info!(
            round = i,
            message_count = messages.len(),
            "starting chat completion request"
        );

        let response = client
            .chat()
            .create(
                CreateChatCompletionRequestArgs::default()
                    .model(&module_config.model)
                    .messages(messages.clone())
                    .tools(vec![python_tool.clone()])
                    .parallel_tool_calls(false)
                    .reasoning_effort(module_config.reasoning_effort.clone())
                    .build()?,
            )
            .await?;

        let message = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| error::Error::InvalidResponse("模型没有返回 choice".to_string()))?
            .message;
        let calls = message.tool_calls.clone().unwrap_or_default();

        if calls.is_empty() {
            let answer = message
                .content
                .ok_or_else(|| error::Error::InvalidResponse("模型没有返回文本".to_string()))?;
            info!(round = i, answer = %answer, "agent produced final answer");
            return Ok(());
        }

        if i == 5 {
            return Err(error::Error::InvalidResponse(
                "超过最大工具调用轮数".to_string(),
            ));
        }

        messages.push(
            ChatCompletionRequestAssistantMessage {
                content: message.content.map(Into::into),
                refusal: message.refusal,
                tool_calls: Some(calls.clone()),
                ..Default::default()
            }
            .into(),
        );

        for call in calls {
            let (id, output) = match call {
                ChatCompletionMessageToolCalls::Function(call) => {
                    info!(
                        round = i,
                        tool = %call.function.name,
                        tool_call_id = %call.id,
                        "model requested function tool"
                    );
                    let output = if call.function.name != "python" {
                        format!("error: unknown tool {}", call.function.name)
                    } else {
                        match serde_json::from_str::<PythonArgs>(&call.function.arguments) {
                            Ok(args) => {
                                debug!(
                                    round = i,
                                    tool_call_id = %call.id,
                                    code = %args.code,
                                    "executing Python code"
                                );
                                match CString::new(args.code) {
                                    Ok(code) => match tool_executor.call(code.as_c_str()).await {
                                        Ok(output) if output.is_empty() => {
                                            "(no output)".to_string()
                                        }
                                        Ok(output) => output,
                                        Err(err) => format!("error: {err:#}"),
                                    },
                                    Err(err) => format!("error: {err}"),
                                }
                            }
                            Err(err) => format!("error: invalid arguments: {err}"),
                        }
                    };
                    (call.id, output)
                }
                ChatCompletionMessageToolCalls::Custom(call) => {
                    warn!(
                        round = i,
                        tool_call_id = %call.id,
                        "model requested unsupported custom tool"
                    );
                    (call.id, "error: custom tools are not supported".to_string())
                }
            };

            if output.starts_with("error:") {
                warn!(
                    round = i,
                    tool_call_id = %id,
                    output = %output.trim(),
                    "Python execution failed"
                );
            } else {
                info!(
                    round = i,
                    tool_call_id = %id,
                    output = %output.trim(),
                    "Python execution completed"
                );
            }
            messages.push(
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(id)
                    .content(output)
                    .build()?
                    .into(),
            );
        }
    }

    unreachable!()
}

fn module_config(variable: &str) -> error::Result<ModuleConfig> {
    let value = std::env::var(variable)
        .map_err(|err| error::Error::Config(format!("{variable} is not set: {err}")))?;
    parse_module_config(variable, &value)
}

fn parse_module_config(variable: &str, value: &str) -> error::Result<ModuleConfig> {
    let (model, effort) = value.split_once(':').ok_or_else(|| {
        error::Error::Config(format!(
            "{variable} must use the format model:reasoning_effort"
        ))
    })?;
    if model.is_empty() {
        return Err(error::Error::Config(format!(
            "{variable} model must not be empty"
        )));
    }
    let reasoning_effort = match effort {
        "none" => ReasoningEffort::None,
        "minimal" => ReasoningEffort::Minimal,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "xhigh" => ReasoningEffort::Xhigh,
        _ => {
            return Err(error::Error::Config(format!(
                "{variable} has unsupported reasoning effort: {effort}"
            )));
        }
    };
    Ok(ModuleConfig {
        model: model.to_string(),
        reasoning_effort,
    })
}

fn initialize_python() -> error::Result<()> {
    dotenvy::dotenv().ok();
    let home = PathBuf::from(
        std::env::var("PYTHON_HOME")
            .map_err(|err| error::Error::Config(format!("PYTHON_HOME is not set: {err}")))?,
    );
    if !home.join("Lib").is_dir() || !home.join("python3.dll").is_file() {
        return Err(error::Error::Config(format!(
            "PYTHON_HOME is not a CPython installation: {}",
            home.display()
        )));
    }
    // Python reads PYTHONHOME only during interpreter initialization.
    unsafe { std::env::set_var("PYTHONHOME", home) };
    Python::initialize();
    Ok(())
}

fn init_logging() {
    let level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    let filter = if matches!(
        level.as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    ) {
        EnvFilter::new(format!("warn,e_agent={level}"))
    } else {
        EnvFilter::new(level)
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_level(true)
        .with_ansi(false)
        .json()
        .init();
}

fn python_tool() -> error::Result<ChatCompletionTools> {
    Ok(ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObjectArgs::default()
            .name("python")
            .description(
                "Execute one complete Python script. Complete the user's task in one call whenever \
                 possible: chain dependent functions in the same script and pass intermediate values \
                 directly instead of printing them for another round. Tool outputs are JSON-compatible \
                 Python values; JSON Schema objects are dicts, so access fields with result[\"field\"], \
                 not attributes. Import the listed tool package explicitly, use asyncio.run(...) for \
                 async functions, and print only the final useful values.",
            )
            .parameters(serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "Complete Python script, executed with __name__ set to __main__"
                    }
                },
                "required": ["code"],
                "additionalProperties": false
            }))
            .strict(true)
            .build()?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_big_module_config() {
        assert_eq!(
            parse_module_config("E_MODULE_BIG", "gpt-5.6-sol:medium").unwrap(),
            ModuleConfig {
                model: "gpt-5.6-sol".to_string(),
                reasoning_effort: ReasoningEffort::Medium,
            }
        );
        assert!(parse_module_config("E_MODULE_BIG", "gpt-5.6-sol:ultra").is_err());
    }

    #[test]
    fn exposes_python_as_the_only_public_tool() {
        let tools = vec![python_tool().unwrap()];
        assert_eq!(tools.len(), 1);
        assert_eq!(
            serde_json::to_value(&tools).unwrap()[0]["function"]["name"],
            "python"
        );
    }
}
