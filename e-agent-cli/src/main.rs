use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use e_agent_core::{AgentEvent, MessageDelta, Session, UserMessage};
use e_agent_pi_compat::PiCompat;
use e_agent_provider_openai::OpenAIProvider;

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
    let mut tool_executor = PiCompat::default();
    let tool_paths = std::env::var_os("E_AGENT_TOOL_PATHS")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    for path in tool_paths {
        tool_executor
            .load_tool(&path)
            .with_context(|| format!("load {} failed", path.display()))?;
    }
    if let Some(paths) = std::env::var_os("E_AGENT_EXTENSION_PATHS") {
        for path in std::env::split_paths(&paths) {
            tool_executor
                .load_extension(&path)
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
    let mut events = session.subscribe();
    let session_id = session.id();
    let session_path = session.path().to_owned();
    let print_mode = cli.prompt.is_some();
    let log = JsonlLog::open(cli.log_dir, session_id)?;
    let observer = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let safe_event = sanitize_event(&event)?;
                    log.write(&safe_event)?;
                    if print_mode {
                        print_event(&event);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    log.write(&serde_json::json!({"event":"observer_lagged","skipped":skipped}))?;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    if print_mode {
        println!("session id: {session_id}");
    } else {
        eprintln!("session id: {session_id}");
    }
    session.resume_pending().await?;
    if let Some(prompt) = cli.prompt {
        session.run_one_trun(UserMessage::text(prompt)).await?;
        eprintln!("session: {}", session_path.display());
        session.close().await?;
        drop(session);
    } else {
        tokio::task::LocalSet::new()
            .run_until(async move { e_agent_tui::run(session.attach()).await })
            .await?;
    }
    observer.await??;
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

    fn write(&self, event: &impl serde::Serialize) -> Result<()> {
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

fn sanitize_event(event: &AgentEvent) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(event)?;
    redact_json(&mut value);
    Ok(value)
}

fn redact_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                let key = key.to_ascii_lowercase();
                if [
                    "api_key",
                    "apikey",
                    "token",
                    "secret",
                    "password",
                    "authorization",
                    "headers",
                ]
                .iter()
                .any(|needle| key.contains(needle))
                {
                    *value = serde_json::Value::String("[redacted]".into());
                } else {
                    redact_json(value);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_json),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use e_agent_core::AgentEvent;

    #[test]
    fn diagnostic_sanitization_redacts_secret_fields() {
        let event = AgentEvent::ToolExecutionUpdate {
            id: "call".into(),
            update: serde_json::json!({"api_key":"secret", "nested":{"authorization":"Bearer secret"}}),
        };
        let value = sanitize_event(&event).unwrap();
        assert_eq!(value["update"]["api_key"], "[redacted]");
        assert_eq!(value["update"]["nested"]["authorization"], "[redacted]");
    }
}

fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::MessageUpdate {
            delta: MessageDelta::Text(text),
            ..
        } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::MessageEnd {
            message: e_agent_core::Message::Assistant(_),
            ..
        } => println!(),
        AgentEvent::ToolExecutionStart { name, .. } => eprintln!("tool: {name}"),
        AgentEvent::SessionFatal { error } => eprintln!("session fatal: {error}"),
        _ => {}
    }
}
