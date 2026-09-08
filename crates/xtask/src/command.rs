use std::{
    path::PathBuf,
    process::{ExitStatus, Stdio},
};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
};
use xtask::Result;

pub struct ChildProcess {
    child: Child,
    running: bool,
}

impl ChildProcess {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command.process_group(0).kill_on_drop(true);
        Ok(Self {
            child: command.spawn()?,
            running: true,
        })
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        let status = self.child.wait().await?;
        self.running = false;
        Ok(status)
    }

    pub fn exited(&mut self) -> Result<bool> {
        if self.child.try_wait()?.is_some() {
            self.running = false;
        }
        Ok(!self.running)
    }

    pub async fn stop(&mut self) -> Result<()> {
        if let Some(id) = self.child.id() {
            // The UI fixture shuts down its Host and relay on SIGINT. Signal
            // only that owner first; Drop kills the process group on failure.
            unsafe {
                libc::kill(id as libc::pid_t, libc::SIGINT);
            }
            tokio::time::timeout(std::time::Duration::from_secs(10), self.wait()).await??;
        }
        Ok(())
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        if self.running
            && let Some(id) = self.child.id()
        {
            // Every managed child starts a fresh group. A failed build or
            // interrupted test must not leave its subprocesses running.
            unsafe {
                libc::kill(-(id as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

pub async fn status(command: &mut Command) -> Result<ExitStatus> {
    ChildProcess::spawn(command)?.wait().await
}

pub async fn run(command: &mut Command) -> Result<()> {
    let status = status(command).await?;
    if !status.success() {
        let program = command.as_std().get_program();
        return Err(format!("{} exited with {status}", program.to_string_lossy()).into());
    }
    Ok(())
}

pub async fn output(command: &mut Command) -> Result<Vec<u8>> {
    command.stdout(Stdio::piped());
    let mut child = ChildProcess::spawn(command)?;
    let mut stdout = child.child.stdout.take().unwrap();
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).await?;
    let status = child.wait().await?;
    if !status.success() {
        let program = command.as_std().get_program();
        return Err(format!("{} exited with {status}", program.to_string_lossy()).into());
    }
    Ok(bytes)
}

pub fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

pub async fn target_directory() -> Result<PathBuf> {
    let output = output(
        cargo()
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(xtask::repository_root()),
    )
    .await?;
    let metadata: serde_json::Value = serde_json::from_slice(&output)?;
    Ok(PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("Cargo metadata omitted target_directory")?,
    ))
}
