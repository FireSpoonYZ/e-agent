use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use e_agent_tool::{Result, anyhow};
use tokio::process::Command;

const MAX_LINES: usize = 2_000;
const MAX_BYTES: usize = 50 * 1024;
static OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

pub async fn run(command: String, timeout: Option<f64>) -> Result<String> {
    if timeout.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        return Err(anyhow!(
            "timeout must be a positive finite number of seconds"
        ));
    }

    let shell = find_git_bash().await?;
    let mut child = Command::new(shell);
    child
        .args(["--noprofile", "--norc", "-c", &command])
        .current_dir(std::env::current_dir()?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = match timeout {
        Some(seconds) => tokio::time::timeout(Duration::from_secs_f64(seconds), child.output())
            .await
            .map_err(|_| anyhow!("command timed out after {seconds} seconds"))??,
        None => child.output().await?,
    };
    let mut full = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !full.is_empty() && !full.ends_with('\n') {
            full.push('\n');
        }
        full.push_str(&stderr);
    }
    let rendered = truncate_tail(&full).await?;
    if !output.status.success() {
        return Err(anyhow!(
            "{}{}command exited with code {}",
            rendered,
            if rendered.is_empty() { "" } else { "\n\n" },
            output
                .status
                .code()
                .map_or_else(|| "unknown".to_string(), |code| code.to_string())
        ));
    }
    Ok(if rendered.is_empty() {
        "(no output)".to_string()
    } else {
        rendered
    })
}

async fn find_git_bash() -> Result<PathBuf> {
    let mut attempted = Vec::new();
    if let Ok(output) = Command::new("where.exe").arg("git").output().await {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let candidate = Path::new(line.trim())
                .parent()
                .map(|directory| directory.join("../bin/bash.exe"));
            if let Some(candidate) = candidate {
                attempted.push(candidate.clone());
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    for candidate in [
        PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
    ] {
        attempted.push(candidate.clone());
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Ok(output) = Command::new("where.exe").arg("bash").output().await {
        if let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next() {
            return Ok(PathBuf::from(path.trim()));
        }
    }
    Err(anyhow!("Git Bash was not found; tried: {attempted:?}"))
}

async fn truncate_tail(output: &str) -> Result<String> {
    let lines: Vec<_> = output.lines().collect();
    let by_lines = lines.len() > MAX_LINES;
    let start = lines.len().saturating_sub(MAX_LINES);
    let mut rendered = lines[start..].join("\n");
    let by_bytes = rendered.len() > MAX_BYTES;
    if by_bytes {
        let mut boundary = rendered.len() - MAX_BYTES;
        while !rendered.is_char_boundary(boundary) {
            boundary += 1;
        }
        rendered = rendered[boundary..].to_string();
    }
    if by_lines || by_bytes {
        let path = std::env::temp_dir().join(format!(
            "e-agent-bash-{}-{}.log",
            std::process::id(),
            OUTPUT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        tokio::fs::write(&path, output).await?;
        rendered.push_str(&format!(
            "\n\n[Output truncated. Full output: {}]",
            path.display()
        ));
    }
    Ok(rendered)
}
