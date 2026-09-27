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

pub(crate) fn inside_work_tree(cwd: &Path) -> Result<Output> {
    command(cwd)
        .args(["rev-parse", "--is-inside-work-tree"])
        .env("LC_ALL", "C")
        .output()
        .context("failed to run git")
}

pub(crate) fn untracked_diff(cwd: &Path, path: &str) -> Result<Output> {
    let output = command(cwd)
        .args([
            "diff",
            "--no-index",
            "--numstat",
            "-z",
            "--patch",
            "--no-ext-diff",
            "--no-color",
            "--",
            "/dev/null",
            path,
        ])
        .output()
        .context("failed to run git")?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(anyhow!("cannot read untracked file diff"));
    }
    Ok(output)
}
