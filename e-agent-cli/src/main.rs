use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use e_agent_core::{LifecycleEvent, Session, UserMessage};
use e_agent_provider_openai::OpenAIProvider;
use e_agent_tool_ptc::ProgrammaticToolExecutor;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

const SYSTEM_PROMPT: &str = r#"你是运行在 e（一套 coding agent harness，编码代理宿主程序）中的专家级编码助手。你通过读取文件、执行命令、编辑代码以及写入新文件来帮助用户。注意使用utf-8编码"#;

#[derive(Debug, Parser)]
#[command(about = "e-agent")]
struct Cli {
    /// 单次执行的提示词；提供后不读取 stdin
    #[arg(short = 'p', long)]
    prompt: Option<String>,
    /// 恢复已有 JSONL session
    #[arg(long)]
    session: Option<std::path::PathBuf>,
    /// JSONL 日志目录（默认 ~/.e/logs）
    #[arg(long)]
    log_dir: Option<PathBuf>,
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

    let model = std::env::var("E_MODULE_BIG").context("get model failed")?;
    let cwd = std::env::current_dir().context("get current directory failed")?;
    let mut session = Session::open(
        provider,
        tool_executor,
        cwd,
        model,
        SYSTEM_PROMPT,
        cli.session,
    )?;
    let session_id = session.id();
    let log = JsonlLog::open(cli.log_dir, session_id)?;
    session.set_lifecycle_handler(move |event| log.write(event));
    println!("session id: {session_id}");
    session.set_message_handler(print_message);
    session.resume_pending().await?;
    if let Some(prompt) = cli.prompt {
        session.run_one_trun(UserMessage::text(prompt)).await?;
    } else {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Some(line) = lines.next_line().await? {
            let prompt = line.trim();
            if prompt == "/exit" {
                break;
            }
            if prompt.is_empty() {
                continue;
            }
            session.run_one_trun(UserMessage::text(prompt)).await?;
        }
    }
    eprintln!("session: {}", session.path().display());
    session.close().await?;
    Ok(())
}

struct JsonlLog(Mutex<std::fs::File>);

impl JsonlLog {
    fn open(dir: Option<PathBuf>, session_id: impl std::fmt::Display) -> Result<Arc<Self>> {
        let dir = dir.unwrap_or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".e/logs")
        });
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(format!("{session_id}.jsonl")))?;
        Ok(Arc::new(Self(Mutex::new(file))))
    }

    fn write(&self, event: &LifecycleEvent) -> Result<()> {
        let mut file = self
            .0
            .lock()
            .map_err(|_| anyhow!("JSONL log lock poisoned"))?;
        serde_json::to_writer(&mut *file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

fn print_message(message: &e_agent_core::Message) {
    for content in message.content() {
        match content {
            e_agent_core::MessageContent::Text { text } => println!("text: {text}"),
            e_agent_core::MessageContent::Thinking { thinking, .. } => {
                println!("thinking: {thinking}")
            }
            e_agent_core::MessageContent::ToolUse { name, input, .. } => {
                println!("tool use: {name}\n{input}")
            }
        }
    }
}
