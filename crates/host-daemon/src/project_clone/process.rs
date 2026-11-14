use super::*;
use std::process::Stdio;
use tokio::io::AsyncReadExt;

pub(super) async fn cleanup(entry: &mut Entry) -> Result<()> {
    let Some(path) = &entry.staging else {
        return Ok(());
    };
    let destination = Path::new(&entry.state.request.destination);
    if !tokio::fs::try_exists(path).await? {
        entry.staging = None;
        return Ok(());
    }
    ensure!(
        path.parent() == destination.parent()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".bex-clone-"))
            && !tokio::fs::symlink_metadata(path)
                .await?
                .file_type()
                .is_symlink()
            && tokio::fs::read_to_string(path.join("owner")).await? == entry.state.request.id,
        "clone staging ownership could not be verified"
    );
    tokio::fs::remove_dir_all(path).await?;
    entry.staging = None;
    Ok(())
}

pub(super) async fn clone(
    owner: &Clones,
    request: &CloneProject,
    checkout: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    ensure!(!cancel.is_cancelled(), "Clone cancelled");
    owner
        .progress(&request.id, ProjectClonePhase::Fetching, None)
        .await?;
    let mut command = agent_process::command(Path::new("git"))?;
    command
        .args(["clone", "--progress", "--", &request.repository])
        .arg(checkout)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
        .env("GIT_ALLOW_PROTOCOL", "file:git:ssh:http:https")
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    agent_process::suppress_browser(&mut command)?;
    let child = command.spawn().context("Could not start Git clone")?;
    monitor(owner, &request.id, child, cancel).await
}

async fn monitor(
    owner: &Clones,
    id: &str,
    mut child: tokio::process::Child,
    cancel: &CancellationToken,
) -> Result<()> {
    let lifetime = child.stdin.take();
    let mut stderr = child
        .stderr
        .take()
        .context("Git progress pipe unavailable")?;
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(3600));
    tokio::pin!(deadline);
    let mut line = Vec::new();
    let mut buffer = [0u8; 2048];
    let mut stderr_open = true;
    let result = loop {
        let count = tokio::select! {
            biased;
            _ = cancel.cancelled() => break Err(anyhow::anyhow!("Clone cancelled")),
            _ = &mut deadline => break Err(anyhow::anyhow!("Clone timed out. Retry to continue.")),
            result = child.wait() => break result.map(|_| ()).map_err(anyhow::Error::from),
            result = stderr.read(&mut buffer), if stderr_open => match result { Ok(count) => count, Err(_) => break Err(anyhow::anyhow!("Could not read Git progress")) },
        };
        if count == 0 {
            stderr_open = false;
            continue;
        }
        for byte in &buffer[..count] {
            if matches!(byte, b'\r' | b'\n') {
                if let Some((phase, progress)) = parse_progress(&line)
                    && let Err(error) = owner.progress(id, phase, Some(progress)).await
                {
                    drop(lifetime);
                    let _ = child.wait().await;
                    return Err(error);
                }
                line.clear();
            } else if line.len() < 4096 {
                line.push(*byte);
            }
        }
    };
    // EOF on the lifetime pipe terminates and reaps the entire supervised tree.
    drop(lifetime);
    let status = child.wait().await.context("Could not wait for Git clone")?;
    result?;
    ensure!(
        status.success(),
        "Git clone failed (exit {}). Check repository access and retry.",
        status.code().unwrap_or(-1)
    );
    Ok(())
}

fn parse_progress(line: &[u8]) -> Option<(ProjectClonePhase, CloneProgress)> {
    let text = std::str::from_utf8(line).ok()?.trim();
    let phase = if text.starts_with("Receiving objects:") {
        ProjectClonePhase::Fetching
    } else if text.starts_with("Resolving deltas:") {
        ProjectClonePhase::Resolving
    } else if text.starts_with("Checking out files:") || text.starts_with("Updating files:") {
        ProjectClonePhase::Checkout
    } else {
        return None;
    };
    let (_, rest) = text.split_once('(')?;
    let (counts, _) = rest.split_once(')')?;
    let (current, total) = counts.split_once('/')?;
    let current = current.parse::<u64>().ok()?;
    let total = total.parse::<u64>().ok()?;
    (total > 0 && current <= total).then_some((phase, CloneProgress { current, total }))
}

/// Publishing must fail if another process creates the destination during clone.
pub(super) fn publish(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: both NUL-terminated paths live through the call. No pointers escape.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    // Windows directory moves fail when the target exists, including an empty folder.
    #[cfg(windows)]
    {
        std::fs::rename(source, destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_during_progress_reaps_the_supervised_process() {
        let directory = tempfile::tempdir().unwrap();
        let owner = Clones::new(ProjectStore::fixture(
            directory.path().join("worktrees.json"),
        ));
        let id = uuid::Uuid::new_v4().to_string();
        owner
            .registry()
            .await
            .unwrap()
            .lock()
            .await
            .entries
            .push(Entry {
                state: ProjectCloneState {
                    request: CloneProject {
                        id: id.clone(),
                        repository: "/fixture".into(),
                        destination: directory.path().join("checkout").to_str().unwrap().into(),
                    },
                    phase: ProjectClonePhase::Fetching,
                    progress: None,
                    error: None,
                },
                staging: None,
            });
        let mut command = agent_process::command(Path::new("sh")).unwrap();
        command
            .args([
                "-c",
                "printf 'Receiving objects: 50%% (1/2)\\r' >&2; exec sleep 30",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = command.spawn().unwrap();
        let cancel = CancellationToken::new();
        let mut changes = owner.subscribe();
        let task = tokio::spawn({
            let owner = owner.clone();
            let cancel = cancel.clone();
            let id = id.clone();
            async move { monitor(&owner, &id, child, &cancel).await }
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while owner.read().await.unwrap().clones[0].progress.is_none() {
                changes.changed().await.unwrap();
            }
            cancel.cancel();
            assert!(
                task.await
                    .unwrap()
                    .unwrap_err()
                    .to_string()
                    .contains("cancelled")
            );
        })
        .await
        .expect("running clone process is cancelled and reaped");
    }
    #[test]
    fn progress_is_bounded_and_unrelated_stderr_is_not_retained() {
        assert_eq!(
            parse_progress(b"Receiving objects:  40% (4/10), 2 KiB"),
            Some((
                ProjectClonePhase::Fetching,
                CloneProgress {
                    current: 4,
                    total: 10
                }
            ))
        );
        assert_eq!(
            parse_progress(b"Resolving deltas: 100% (1/1), done."),
            Some((
                ProjectClonePhase::Resolving,
                CloneProgress {
                    current: 1,
                    total: 1
                }
            ))
        );
        for line in [
            "token secret",
            "Receiving objects: (2/1)",
            "Receiving objects: (0/0)",
            "Receiving objects: (x/10)",
        ] {
            assert_eq!(parse_progress(line.as_bytes()), None);
        }
    }
    #[test]
    fn publication_never_replaces_an_existing_empty_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("file"), "original").unwrap();
        std::fs::create_dir(&destination).unwrap();
        assert!(publish(&source, &destination).is_err());
        assert!(source.join("file").exists());
        assert!(!destination.join("file").exists());
        std::fs::remove_dir(&destination).unwrap();
        publish(&source, &destination).unwrap();
        assert_eq!(
            std::fs::read_to_string(destination.join("file")).unwrap(),
            "original"
        );
    }
}
