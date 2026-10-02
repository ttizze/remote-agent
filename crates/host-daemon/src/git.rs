use anyhow::{Context as _, Result, anyhow};
use std::{
    path::Path,
    process::{Command, Output},
};

pub(crate) fn text(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = output(cwd, args)?;
    String::from_utf8(output.stdout).context("git returned non-UTF-8 output")
}

pub(crate) fn output(cwd: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        // Background status reads must not compete with checkout writes.
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    #[test]
    fn status_reads_preserve_the_index_while_observing_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        text(root, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        let path = root.join("tracked.txt");
        fs::write(&path, "tracked\n").unwrap();
        text(root, &["add", "tracked.txt"]).unwrap();
        text(
            root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        let index_path = root.join(".git/index");
        let index = fs::read(&index_path).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)),
            )
            .unwrap();
        assert!(
            text(root, &["status", "--porcelain=v1"])
                .unwrap()
                .is_empty()
        );
        assert_eq!(fs::read(&index_path).unwrap(), index);
        fs::write(&path, "changed\n").unwrap();
        assert_eq!(
            text(root, &["status", "--porcelain=v1"]).unwrap(),
            " M tracked.txt\n"
        );
        assert_eq!(fs::read(index_path).unwrap(), index);
    }
}
