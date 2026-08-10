use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use e_agent_tool::{Result, anyhow};
#[cfg(windows)]
use std::path::Path;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::mpsc,
};

const MAX_LINES: usize = 2_000;
const MAX_BYTES: usize = 50 * 1024;
const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1_000.0;
static OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

pub async fn run(command: String, timeout: Option<f64>) -> Result<String> {
    if timeout
        .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > MAX_TIMEOUT_SECONDS)
    {
        return Err(anyhow!(
            "timeout must be between 0 and {MAX_TIMEOUT_SECONDS} seconds"
        ));
    }

    let shell = find_git_bash().await?;
    let mut command_process = Command::new(shell);
    command_process
        .args(["--noprofile", "--norc", "-c", &command])
        .current_dir(std::env::current_dir()?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command_process.process_group(0);
    let mut child = command_process.spawn()?;
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("capture stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("capture stderr"))?;
    let (sender, receiver) = mpsc::channel(16);
    let stdout_task = tokio::spawn(pump(stdout, sender.clone()));
    let stderr_task = tokio::spawn(pump(stderr, sender.clone()));
    drop(sender);
    let capture_task = tokio::spawn(capture(receiver));

    let mut timed_out = false;
    let status = match timeout {
        Some(seconds) => {
            match tokio::time::timeout(Duration::from_secs_f64(seconds), child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    timed_out = true;
                    if let Some(pid) = pid {
                        terminate_process_tree(pid).await;
                    }
                    let _ = child.kill().await;
                    child.wait().await?
                }
            }
        }
        None => child.wait().await?,
    };

    stdout_task.await??;
    stderr_task.await??;
    let rendered = capture_task.await??;
    if timed_out {
        return Err(anyhow!(
            "{}{}command timed out after {} seconds",
            rendered,
            separator(&rendered),
            timeout.unwrap()
        ));
    }
    if !status.success() {
        return Err(anyhow!(
            "{}{}command exited with code {}",
            rendered,
            separator(&rendered),
            status
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

fn separator(output: &str) -> &'static str {
    if output.is_empty() { "" } else { "\n\n" }
}

async fn pump(mut reader: impl AsyncRead + Unpin, sender: mpsc::Sender<Vec<u8>>) -> Result<()> {
    let mut buffer = vec![0; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        if sender.send(buffer[..read].to_vec()).await.is_err() {
            return Ok(());
        }
    }
}

async fn capture(mut receiver: mpsc::Receiver<Vec<u8>>) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "e-agent-bash-{}-{}.log",
        std::process::id(),
        OUTPUT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = tokio::fs::File::create(&path).await?;
    let mut tail = VecDeque::with_capacity(MAX_BYTES);
    let mut total_bytes = 0;
    let mut total_newlines = 0;
    let mut last_byte = None;

    while let Some(chunk) = receiver.recv().await {
        file.write_all(&chunk).await?;
        total_bytes += chunk.len();
        total_newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
        last_byte = chunk.last().copied().or(last_byte);
        tail.extend(chunk);
        if tail.len() > MAX_BYTES {
            tail.drain(..tail.len() - MAX_BYTES);
        }
    }
    file.flush().await?;
    drop(file);

    let total_lines = total_newlines + usize::from(total_bytes > 0 && last_byte != Some(b'\n'));
    let truncated = total_bytes > MAX_BYTES || total_lines > MAX_LINES;
    let mut bytes: Vec<_> = tail.into();
    if total_lines > MAX_LINES {
        bytes = last_lines(&bytes, MAX_LINES).to_vec();
    }
    let mut output = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        output.push_str(&format!(
            "\n\n[Output truncated. Full output: {}]",
            path.display()
        ));
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(output)
}

fn last_lines(bytes: &[u8], limit: usize) -> &[u8] {
    let mut seen = usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'));
    for (index, byte) in bytes.iter().enumerate().rev() {
        if *byte == b'\n' {
            if seen == limit {
                return &bytes[index + 1..];
            }
            seen += 1;
        }
    }
    bytes
}

#[cfg(windows)]
async fn terminate_process_tree(pid: u32) {
    let task = Command::new("taskkill.exe")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[cfg(not(windows))]
async fn terminate_process_tree(pid: u32) {
    // SAFETY: a negative PID targets the process group created above; SIGKILL has no pointer inputs.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}

#[cfg(windows)]
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
        if output.status.success() {
            if let Some(path) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|path| !path.is_empty() && Path::new(path).is_file())
            {
                return Ok(PathBuf::from(path));
            }
        }
    }
    Err(anyhow!("Git Bash was not found; tried: {attempted:?}"))
}

#[cfg(not(windows))]
async fn find_git_bash() -> Result<PathBuf> {
    if Command::new("bash")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
    {
        Ok(PathBuf::from("bash"))
    } else {
        Err(anyhow!("bash was not found on PATH"))
    }
}

#[cfg(test)]
mod tests {
    use super::last_lines;

    #[test]
    fn keeps_requested_tail_lines() {
        assert_eq!(last_lines(b"one\ntwo\nthree\n", 2), b"two\nthree\n");
        assert_eq!(last_lines(b"one\ntwo\nthree", 2), b"two\nthree");
        assert_eq!(last_lines(b"one\ntwo", 2), b"one\ntwo");
    }
}
