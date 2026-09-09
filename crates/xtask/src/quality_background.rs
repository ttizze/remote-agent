use crate::command;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;
use xtask::Result;

async fn git(arguments: &[&str]) -> Result<String> {
    Ok(
        String::from_utf8(command::output(Command::new("git").args(arguments)).await?)?
            .trim_end()
            .to_owned(),
    )
}

async fn state_directory() -> Result<PathBuf> {
    Ok(
        PathBuf::from(git(&["rev-parse", "--path-format=absolute", "--git-common-dir"]).await?)
            .join("bex-quality"),
    )
}

fn lock(state: &Path) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(state.join("worker.lock"))?;
    // File ownership keeps the advisory lock alive; process exit releases it.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(file))
    } else {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error.into())
        }
    }
}

fn save(state: &Path, commit: &str, value: Value) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(state.join("results"))?;
    serde_json::to_writer_pretty(&mut file, &value)?;
    file.flush()?;
    file.persist(state.join("results").join(format!("{commit}.json")))?;
    Ok(())
}

struct Request {
    path: PathBuf,
    commit: String,
    root: String,
    modified: std::time::SystemTime,
}

fn requests(state: &Path) -> Result<Vec<Request>> {
    let mut requests = Vec::new();
    for entry in fs::read_dir(state.join("queue"))? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "request")
        {
            continue;
        }
        let contents = fs::read_to_string(&path)?;
        let (commit, root) = contents.split_once('\n').ok_or("invalid quality request")?;
        if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid quality commit".into());
        }
        requests.push(Request {
            modified: entry.metadata()?.modified()?,
            path,
            commit: commit.into(),
            root: root.into(),
        });
    }
    requests.sort_by(|a, b| a.modified.cmp(&b.modified).then(a.commit.cmp(&b.commit)));
    Ok(requests)
}

fn has_requests(state: &Path) -> Result<bool> {
    // Another worker may consume entries while we do not hold the lock.
    // Inspect names only; opening those files would race with their removal.
    for entry in fs::read_dir(state.join("queue"))? {
        if entry?
            .path()
            .extension()
            .is_some_and(|extension| extension == "request")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn worker() -> Result<()> {
    let state = state_directory().await?;
    for directory in ["queue", "results", "logs", "worktrees"] {
        fs::create_dir_all(state.join(directory))?;
    }
    loop {
        let Some(guard) = lock(&state)? else {
            return Ok(());
        };
        loop {
            let mut pending = requests(&state)?;
            let Some(request) = pending.pop() else { break };
            // Coalesce only this source worktree. Other branches retain their checks.
            for older in pending.iter().filter(|older| older.root == request.root) {
                save(
                    &state,
                    &older.commit,
                    json!({"commit": older.commit,
                    "status": "superseded", "supersededBy": request.commit}),
                )?;
                fs::remove_file(&older.path)?;
            }
            let log = state.join("logs").join(format!("{}.log", request.commit));
            save(
                &state,
                &request.commit,
                json!({"commit": request.commit,
                "status": "running", "log": log}),
            )?;
            fs::remove_file(request.path)?;
            let result = check(&state, &request.commit, &log).await;
            save(
                &state,
                &request.commit,
                json!({"commit": request.commit,
                "status": if result.is_ok() { "passed" } else { "failed" },
                "log": log, "error": result.err().map(|error| error.to_string())}),
            )?;
        }
        // Release before checking again: an enqueuer may have observed our lock
        // immediately before we found the queue empty.
        drop(guard);
        if !has_requests(&state)? {
            return Ok(());
        }
    }
}

async fn check(state: &Path, commit: &str, log: &Path) -> Result<()> {
    let directory = tempfile::tempdir_in(state.join("worktrees"))?;
    let checkout = directory.path().join("checkout");
    let checkout_str = checkout.to_str().ok_or("non-UTF-8 checkout path")?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(log)?;
    command::run(
        Command::new("git")
            .args(["worktree", "add", "--detach", checkout_str, commit])
            .stdout(file.try_clone()?)
            .stderr(file.try_clone()?),
    )
    .await?;
    let result = tokio::time::timeout(Duration::from_secs(3600), async {
        let mut result = Ok(());
        for (cwd, args) in [
            (
                checkout.join("apps/server"),
                ["mix", "deps.get", "--check-locked"],
            ),
            (checkout.clone(), ["cargo", "xtask", "quality"]),
        ] {
            let check = command::run(
                Command::new("nix")
                    .arg("develop")
                    .arg(&checkout)
                    .arg("--command")
                    .args(args)
                    .current_dir(cwd)
                    .env_remove("CARGO")
                    .env_remove("RUSTC")
                    .env_remove("RUSTDOC")
                    .env("CARGO_TARGET_DIR", state.join("cargo-target"))
                    .env("MIX_DEPS_PATH", state.join("mix-deps"))
                    .env("MIX_BUILD_PATH", state.join("mix-build"))
                    .stdin(std::process::Stdio::null())
                    .stdout(file.try_clone()?)
                    .stderr(file.try_clone()?),
            )
            .await;
            result = result.and(check);
        }
        result
    })
    .await;
    let cleanup = command::run(
        Command::new("git")
            .args(["worktree", "remove", "--force", checkout_str])
            .stdout(file.try_clone()?)
            .stderr(file),
    )
    .await;
    result??;
    cleanup
}

pub async fn status(wait: bool) -> Result<()> {
    let state = state_directory().await?;
    let mut commit = git(&["rev-parse", "HEAD"]).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
    loop {
        let result_path = state.join("results").join(format!("{commit}.json"));
        let mut result = if result_path.exists() {
            serde_json::from_slice::<Value>(&fs::read(result_path)?)?
        } else {
            json!({"commit": commit, "status": "missing"})
        };
        if state
            .join("queue")
            .join(format!("{commit}.request"))
            .exists()
        {
            result["status"] = "queued".into();
        }
        if state
            .join("results")
            .join(format!("{commit}.bootstrap-failed"))
            .exists()
        {
            result["status"] = "failed".into();
            result["error"] = "worker startup failed; inspect bootstrapLog".into();
        } else if result["status"] == "running" && lock(&state)?.is_some() {
            result["status"] = "interrupted".into();
        }
        let pending = matches!(result["status"].as_str(), Some("queued" | "running"));
        if wait && pending && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let dirty = !git(&["status", "--porcelain"]).await?.is_empty();
        let current = git(&["rev-parse", "HEAD"]).await?;
        if current != commit {
            commit = current;
            continue;
        }
        result["workingTreeDirty"] = dirty.into();
        result["bootstrapLog"] = state
            .join("logs")
            .join(format!("{commit}.bootstrap.log"))
            .to_string_lossy()
            .into_owned()
            .into();
        println!("{}", serde_json::to_string_pretty(&result)?);
        return if result["status"] == "passed" && !dirty {
            Ok(())
        } else {
            Err("current worktree does not have a clean, passed quality result".into())
        };
    }
}
