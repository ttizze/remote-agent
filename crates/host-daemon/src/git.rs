use anyhow::{Context as _, Result, anyhow};
use std::{
    path::Path,
    process::{Command, Output},
};

pub(crate) fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    command
}

pub(crate) fn text(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = output(cwd, args)?;
    String::from_utf8(output.stdout).context("git returned non-UTF-8 output")
}

pub(crate) fn output(cwd: &Path, args: &[&str]) -> Result<Output> {
    let output = command(cwd)
        .args(args)
        .output()
        .context("failed to run git")?;
    if output.status.success() {
        return Ok(output);
    }
    Err(anyhow!(failure(&output)))
}

pub(crate) fn failure(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        "git command failed".to_owned()
    } else {
        message
    }
}
