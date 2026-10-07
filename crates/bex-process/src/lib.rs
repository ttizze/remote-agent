//! All owned processes run under a small companion supervisor. Its stdin is
//! a lifetime pipe owned by the Host; EOF also covers Host SIGKILL/crashes.
use std::{io, path::Path};
use tokio::process::Command;

pub fn command(program: &Path) -> io::Result<Command> {
    let mut command = supervisor_command()?;
    command.arg(program);
    Ok(command)
}

fn supervisor_command() -> io::Result<Command> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("Host executable directory is unavailable"))?;
    let directory = if directory.file_name().is_some_and(|name| name == "deps") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let supervisor = directory.join(format!(
        "bex-provider-supervisor{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !supervisor.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "bex-provider-supervisor must be built and installed beside the Host; process execution is unavailable",
        ));
    }
    let mut command = Command::new(supervisor);
    // Do not kill the supervisor on drop: closing its input lets it terminate
    // and reap the entire provider process group first.
    command.kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}
/// Separate unsolicited query acknowledgements from user operations and resizes.
pub const TERMINAL_QUERY_REPLY_ID: u64 = u64::MAX;

/// On Windows the Host owns a Job Object for the supervisor and all PTY
/// descendants. Unix cleanup is performed by the supervisor on lifetime EOF.
pub fn terminal_command() -> io::Result<process_wrap::tokio::CommandWrap> {
    let mut command = supervisor_command()?;
    command
        .arg("--bex-pty")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    let wrapped = process_wrap::tokio::CommandWrap::from(command);
    #[cfg(windows)]
    let wrapped = {
        let mut wrapped = wrapped;
        wrapped
            .wrap(process_wrap::tokio::JobObject)
            .wrap(process_wrap::tokio::KillOnDrop);
        wrapped
    };
    // Mutable on Windows; retain one return expression on all platforms.
    Ok(wrapped)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PtyCommand {
    Start {
        command: Vec<String>,
        cwd: String,
        rows: u16,
        cols: u16,
        /// The shell's whole environment.
        env: std::collections::BTreeMap<String, String>,
    },
    Write {
        id: u64,
        data: Vec<u8>,
    },
    Resize {
        id: u64,
        rows: u16,
        cols: u16,
    },
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PtyEvent {
    Started { pid: Option<u32> },
    Output { data: Vec<u8> },
    Ack { id: u64, error: Option<String> },
    Exited { code: u32 },
    Failed { message: String },
}
