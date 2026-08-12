use crate::{
    message::UserMessage, provider::OpenAIProvider, session::Session,
    tool::ptc::ProgrammaticToolExecutor,
};

mod message;
mod provider;
mod session;
mod tool;

use anyhow::{Context, Result, anyhow};

const SYSTEM_PROMPT: &str = r#"你是运行在 e（一套 coding agent harness，编码代理宿主程序）中的专家级编码助手。你通过读取文件、执行命令、编辑代码以及写入新文件来帮助用户。"#;
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().context("load .env failed")?;
    let provider = OpenAIProvider::new();
    let mut tool_executor = ProgrammaticToolExecutor::default();
    let tool_paths = std::env::var_os("E_AGENT_TOOL_PATHS")
        .ok_or_else(|| anyhow!("E_AGENT_TOOL_PATHS is not set"))?;
    let tool_paths: Vec<_> = std::env::split_paths(&tool_paths).collect();
    for path in tool_paths {
        tool_executor.load(&path).map_err(|err| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "load {} failed: {:#}",
                path.display(),
                err
            ))
        })?;
    }

    let mut session = Session::new(provider, tool_executor, SYSTEM_PROMPT);
    session
        .run_one_trun(UserMessage::text("commit一下代码"))
        .await?;
    session.close().await?;

    Ok(())
}
