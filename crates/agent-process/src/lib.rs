//! All owned processes run under a small companion supervisor. Its stdin is
//! a lifetime pipe owned by the Host; EOF also covers Host SIGKILL/crashes.
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::process::Command;

/// Standalone processes must not inherit a launcher's blocked shutdown or
/// child-exit signals. Call before creating runtime or worker threads.
#[cfg(unix)]
pub fn reset_signal_mask() -> io::Result<()> {
    // SAFETY: initialize a local signal set and change only the calling thread.
    let error = unsafe {
        let mut signals = std::mem::zeroed();
        libc::sigemptyset(&mut signals);
        libc::pthread_sigmask(libc::SIG_SETMASK, &signals, std::ptr::null_mut())
    };
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

pub fn command(program: &Path) -> io::Result<Command> {
    let mut command = supervisor_command()?;
    command.arg(program);
    Ok(command)
}

fn supervisor_command() -> io::Result<Command> {
    let mut command = Command::new(supervisor_path()?);
    // Closing its input lets the supervisor terminate and reap the provider group.
    command.kill_on_drop(false);
    Ok(command)
}

/// Keep browser interaction on the requesting client. Providers may otherwise
/// open a second, host-local OAuth URL, even when they also print a manual URL.
pub fn suppress_browser(command: &mut Command) -> io::Result<()> {
    command
        .env("BROWSER", supervisor_path()?)
        .env("AGENT_SUPPRESS_BROWSER", "1");
    Ok(())
}

fn supervisor_path() -> io::Result<PathBuf> {
    let supervisor = runtime_directory()?.join(format!(
        "agent-provider-supervisor{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !supervisor.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "agent-provider-supervisor must be built and installed beside the Host; process execution is unavailable",
        ));
    }
    Ok(supervisor)
}

/// Installed Hosts use their executable directory. Test launchers supply the
/// public build directory because Cargo may execute tests from shared storage.
pub fn runtime_directory() -> io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let configured = std::env::var_os("AGENT_RUNTIME_DIRECTORY");
    runtime_directory_from(executable.parent(), configured.as_deref().map(Path::new))
}

fn runtime_directory_from(
    executable_directory: Option<&Path>,
    configured: Option<&Path>,
) -> io::Result<PathBuf> {
    if let Some(directory) = configured {
        if !directory.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "AGENT_RUNTIME_DIRECTORY must be absolute",
            ));
        }
        return Ok(directory.to_path_buf());
    }
    executable_directory
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("Host executable directory is unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_resources_follow_the_install_or_the_explicit_launch_directory() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("installed");
        let public = root.path().join("target/debug");
        assert_eq!(
            runtime_directory_from(Some(&installed), None).unwrap(),
            installed
        );
        assert_eq!(
            runtime_directory_from(Some(&installed), Some(&public)).unwrap(),
            public
        );
        assert_eq!(
            runtime_directory_from(None, Some(Path::new("relative")))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(runtime_directory_from(None, None).is_err());
    }
}

/// On Windows the Host owns a Job Object for the supervisor and all PTY
/// descendants. Unix cleanup is performed by the supervisor on lifetime EOF.
pub fn terminal_command() -> io::Result<process_wrap::tokio::CommandWrap> {
    let mut command = supervisor_command()?;
    command
        .arg("--agent-pty")
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
    Started,
    Output { data: Vec<u8> },
    Ack { id: u64, error: Option<String> },
    Exited { code: u32 },
    Failed { message: String },
}
