use anyhow::{Context as _, Result, anyhow};
use std::{
    io::Read as _,
    path::Path,
    process::{Command, Output, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

/// A Git command stopped because its operation was cancelled.
#[derive(Debug, thiserror::Error)]
#[error("Git command cancelled")]
pub(crate) struct Cancelled;

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

/// Runs Git like [`output`] and stops it, with every process it started, once
/// `cancel` fires; the error is then [`Cancelled`]. It returns only after Git
/// has exited, so nothing it does outlives the call.
pub(crate) fn cancellable(cwd: &Path, args: &[&str], cancel: &CancellationToken) -> Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd);
    let output = run_cancellable(command, cancel)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(anyhow!(failure(&output)))
}

/// Runs `command` to completion, whatever its exit status, unless `cancel`
/// fires first: then it stops the command with every process it started and
/// fails with [`Cancelled`] once they have exited.
pub(crate) fn run_cancellable(mut command: Command, cancel: &CancellationToken) -> Result<Output> {
    if cancel.is_cancelled() {
        return Err(Cancelled.into());
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().context("failed to run git")?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = read(child.stderr.take().map(|pipe| Box::new(pipe) as _));
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.is_cancelled() {
            // The group also holds the processes Git started, such as the
            // clones of `submodule update`.
            #[cfg(unix)]
            // SAFETY: signals the process group this call created.
            unsafe {
                libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(Cancelled.into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Whether `name` is a branch name Git accepts, as `git check-ref-format
/// --branch` decides for a literal name: `refs/heads/<name>` is a valid ref
/// name, and the name neither starts with `-` nor is `HEAD`. Shorthands such as
/// `@` and `@{-1}` are not resolved and are rejected.
pub(crate) fn valid_branch_name(name: &str) -> bool {
    !name.starts_with('-')
        && !matches!(name, "HEAD" | "@")
        && valid_ref_name(&format!("refs/heads/{name}"))
}

/// `git check-ref-format` without options.
fn valid_ref_name(name: &str) -> bool {
    let forbidden = |c: char| {
        c.is_ascii_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    };
    name != "@"
        && !name.contains("..")
        && !name.contains("@{")
        && !name.ends_with('.')
        && !name.chars().any(forbidden)
        && name.split('/').count() >= 2
        && name.split('/').all(|component| {
            !component.is_empty() && !component.starts_with('.') && !component.ends_with(".lock")
        })
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
    use proptest::prelude::*;

    /// Git's verdict on a literal branch name: `git branch` rejects a leading
    /// `-` and `HEAD`, reads `@` as `HEAD`, and checks the rest as a ref name.
    fn git_accepts_branch(name: &str) -> bool {
        let directory = tempfile::tempdir().unwrap();
        !name.starts_with('-')
            && !matches!(name, "HEAD" | "@")
            && Command::new("git")
                .args(["check-ref-format", &format!("refs/heads/{name}")])
                .current_dir(directory.path())
                .output()
                .unwrap()
                .status
                .success()
    }

    #[test]
    fn branch_names_follow_git_check_ref_format() {
        for (name, valid) in [
            ("feature/search", true),
            ("agent/session-0123456789ab", true),
            ("日本語", true),
            ("@", false),
            ("-D", false),
            ("--force", false),
            ("HEAD", false),
            ("@{-1}", false),
            ("a..b", false),
            ("a/.hidden", false),
            ("topic.lock", false),
            ("topic/", false),
            ("/topic", false),
            ("a//b", false),
            ("topic.", false),
            ("a b", false),
            ("a:b", false),
            ("a~1", false),
            ("a^", false),
            ("a?", false),
            ("a*", false),
            ("a[b", false),
            ("a\\b", false),
            ("a\u{7f}", false),
            ("", false),
        ] {
            assert_eq!(valid_branch_name(name), valid, "{name:?}");
            assert_eq!(git_accepts_branch(name), valid, "Git disagrees on {name:?}");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn branch_names_match_git(name in "[-a-zA-Z0-9./@{}~^:?*\\[\\\\ \t\u{7f}éあ]{0,12}") {
            prop_assert_eq!(valid_branch_name(&name), git_accepts_branch(&name), "{:?}", name);
        }
    }
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
