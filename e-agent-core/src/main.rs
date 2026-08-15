use crate::{
    message::UserMessage, provider::OpenAIProvider, session::Session,
    tool::ptc::ProgrammaticToolExecutor,
};

mod message;
mod provider;
mod session;
mod tool;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

const SYSTEM_PROMPT: &str = r#"你是运行在 e（一套 coding agent harness，编码代理宿主程序）中的专家级编码助手。你通过读取文件、执行命令、编辑代码以及写入新文件来帮助用户。注意使用utf-8编码"#;

#[derive(Debug, Parser)]
#[command(about = "e-agent")]
struct Cli {
    /// 要发送给 agent 的提示词
    prompt: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    dotenvy::dotenv().context("load .env failed")?;
    let provider = OpenAIProvider::new().context("create openai provider failed")?;
    let mut tool_executor = ProgrammaticToolExecutor::default();
    let tool_paths = std::env::var_os("E_AGENT_TOOL_PATHS")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    for path in tool_paths {
        tool_executor
            .load(&path)
            .with_context(|| format!("load {} failed", path.display()))?;
    }
    if let Some(paths) = std::env::var_os("E_AGENT_EXTENSION_PATHS") {
        for path in std::env::split_paths(&paths) {
            tool_executor
                .load_pi_extension(&path)
                .await
                .with_context(|| format!("load Pi extension {} failed", path.display()))?;
        }
    }
    if tool_executor.tools().is_empty() && std::env::var_os("E_AGENT_EXTENSION_PATHS").is_none() {
        return Err(anyhow!(
            "E_AGENT_TOOL_PATHS or E_AGENT_EXTENSION_PATHS must be set"
        ));
    }

    let mut session = Session::new(provider, tool_executor, SYSTEM_PROMPT);
    session.run_one_trun(UserMessage::text(cli.prompt)).await?;
    session.close().await?;

    Ok(())
}
