use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

pub(crate) const SIGPIPE_TRAMPOLINE_EXEC_FAILURE_PREFIX: &str = "pi-sigpipe-reset: exec failed:";

pub(crate) fn command_with_default_sigpipe_in_dir(
    program: impl AsRef<OsStr>,
    _cwd: &Path,
) -> std::io::Result<Command> {
    Ok(Command::new(program))
}

pub(crate) fn isolate_command_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

pub(crate) fn kill_process_group_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
