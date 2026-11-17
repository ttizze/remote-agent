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
        .context("gitを実行できませんでした。")?;
    checked_output(output)
}

/// Bound both process completion and output EOF. The supervisor owns cleanup;
/// cancellable async pipes cannot strand a blocking reader after the deadline.
pub(crate) fn bounded(cwd: &Path, args: &[&str], limit: std::time::Duration) -> Result<Output> {
    // One scoped I/O worker replaces the two uninterruptible pipe threads and
    // also permits synchronous callers that already run inside a Tokio runtime.
    let output = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(bounded_output(cwd, args, limit))
            })
            .join()
    })
    .map_err(|_| anyhow!("Git I/O worker failed"))??;
    checked_output(output)
}

async fn bounded_output(cwd: &Path, args: &[&str], limit: std::time::Duration) -> Result<Output> {
    use tokio::io::AsyncReadExt;
    let mut child = agent_process::non_interactive_command(Path::new("git"))?
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run git")?;
    let lifetime = child.stdin.take();
    let mut stdout = child.stdout.take().context("Git stdout unavailable")?;
    let mut stderr = child.stderr.take().context("Git stderr unavailable")?;
    let result = tokio::time::timeout(limit, async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (status, read_out, read_err) = tokio::join!(
            child.wait(),
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
        );
        read_out?;
        read_err?;
        Ok(Output {
            status: status?,
            stdout: out,
            stderr: err,
        })
    })
    .await;
    // EOF requests cleanup of the owned process group / Job Object. Preserve
    // that owner until cleanup completes, including on an output timeout.
    drop(lifetime);
    match result {
        Ok(output) => output,
        Err(_) => {
            let _ = child.wait().await;
            Err(anyhow!("git {} timed out", args.join(" ")))
        }
    }
}

fn checked_output(output: Output) -> Result<Output> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(anyhow!(failure(&output)))
    }
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

    #[tokio::test]
    async fn bounded_git_preserves_output_and_failures_inside_an_existing_runtime() {
        let directory = tempfile::tempdir().unwrap();
        let output = bounded(directory.path(), &["--version"], Duration::from_secs(5)).unwrap();
        assert!(output.stdout.starts_with(b"git version "));
        assert!(
            bounded(
                directory.path(),
                &["not-a-git-command"],
                Duration::from_secs(5),
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_git_covers_a_running_root_and_descendant_output_after_root_exit() {
        for (alias, root_exits) in [
            ("alias.fixture=!sleep 3 & printf ready", true),
            ("alias.fixture=!sleep 3", false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().to_owned();
            let (send, receive) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = bounded(&root, &["-c", alias, "fixture"], Duration::from_millis(100));
                let _ = send.send(result);
            });
            // Both fixtures naturally finish after three seconds, even on a
            // regression. The deadline also covers pipes held by descendants.
            let result = receive.recv_timeout(Duration::from_secs(2));
            worker.join().unwrap();
            match result.expect("Git completion and pipe EOF must both remain bounded") {
                Ok(output) => {
                    assert!(root_exits, "a running Git root must time out");
                    assert_eq!(output.stdout, b"ready");
                }
                Err(error) => assert!(error.to_string().contains("timed out"), "{error}"),
            }
        }
    }

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
