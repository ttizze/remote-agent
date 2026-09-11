use std::{
    path::Path,
    process::{Command, Output},
};

pub(crate) fn text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = output(cwd, args)?;
    String::from_utf8(output.stdout).map_err(|_| "git returned non-UTF-8 output".to_owned())
}

pub(crate) fn output(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    Err(failure(&output))
}

pub(crate) fn failure(output: &Output) -> String {
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.is_empty() {
        "git command failed".to_owned()
    } else {
        message
    }
}
