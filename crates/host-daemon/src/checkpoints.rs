//! Git checkpoints in hidden refs (T3 `CheckpointStore` over the Git driver).
//! Private indexes leave the user's index and HEAD untouched.
use anyhow::{Context, Result, anyhow};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, Weak},
};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

/// Serializes Git writes per checkout.
#[derive(Default)]
pub(crate) struct Checkpoints {
    workspaces: Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>,
}

const DURABLE: &[&str] = &[
    "-c",
    "core.fsync=objects,reference",
    "-c",
    "core.fsyncMethod=fsync",
];
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
/// Patches are cut at this size; summaries fail instead.
const DIFF_LIMIT: usize = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiffFormat {
    Patch,
    Numstat,
}

struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run(cwd: &Path, args: &[&str], index: Option<&Path>, limit: usize) -> Result<Output> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env("GIT_AUTHOR_NAME", "Remote Agent")
        .env("GIT_AUTHOR_EMAIL", "remote-agent@users.noreply.github.com")
        .env("GIT_COMMITTER_NAME", "Remote Agent")
        .env(
            "GIT_COMMITTER_EMAIL",
            "remote-agent@users.noreply.github.com",
        )
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().context("start checkpoint Git operation")?;
    async fn read(pipe: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![];
        pipe.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
        Ok(bytes)
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (status, stdout, stderr) =
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::try_join!(
                async { child.wait().await.map_err(anyhow::Error::from) },
                read(stdout, limit),
                read(stderr, 64 * 1024)
            )
        })
        .await
        .context("checkpoint Git operation timed out")??;
    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}
async fn git(cwd: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
    let output = run(cwd, args, index, OUTPUT_LIMIT).await?;
    if output.stdout.len() > OUTPUT_LIMIT {
        return Err(anyhow!("checkpoint Git output exceeds its limit"));
    }
    if !output.success {
        return Err(anyhow!(
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        ));
    }
    Ok(output.stdout)
}
async fn text(cwd: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    Ok(String::from_utf8(git(cwd, args, index).await?)?
        .trim()
        .into())
}
async fn commit_of(cwd: &Path, reference: &str) -> Result<Option<String>> {
    let output = run(
        cwd,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
        None,
        4096,
    )
    .await?;
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((output.success && !commit.is_empty()).then_some(commit))
}

pub(crate) async fn checkout_root(cwd: &Path) -> Result<PathBuf> {
    Ok(dunce::canonicalize(
        text(cwd, &["rev-parse", "--show-toplevel"], None).await?,
    )?)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RestorePath {
    path: String,
    existed: bool,
}
fn validate_restore_path(root: &Path, scope: &Path, path: &str) -> Result<()> {
    let relative = Path::new(path);
    if !relative
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
        || !root.join(relative).starts_with(scope)
    {
        return Err(anyhow!("checkpoint path escapes its scope"));
    }
    let target = root.join(relative);
    if target.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Err(anyhow!(
            "checkpoint restore cannot replace a directory or submodule with a file"
        ));
    }
    let mut parent = target.parent();
    while let Some(directory) = parent.filter(|p| *p != root) {
        if directory.symlink_metadata().is_ok_and(|m| !m.is_dir()) {
            return Err(anyhow!(
                "checkpoint restore path has a non-directory ancestor"
            ));
        }
        parent = directory.parent();
    }
    Ok(())
}
fn recover_files(root: &Path, directory: &Path, journal: &[RestorePath]) -> Result<()> {
    for entry in journal {
        let path = Path::new(&entry.path);
        if !path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
        {
            return Err(anyhow!("invalid restore recovery path"));
        }
        let target = root.join(path);
        let backup = directory.join("backup").join(path);
        if backup.symlink_metadata().is_ok() {
            if target.symlink_metadata().is_ok() {
                std::fs::remove_file(&target)?;
            }
            std::fs::create_dir_all(target.parent().context("recovery parent missing")?)?;
            std::fs::rename(backup, target)?;
        } else if !entry.existed && target.symlink_metadata().is_ok() {
            std::fs::remove_file(target)?;
        }
    }
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

/// Restored files with the originals kept aside until `commit`. Dropping it
/// puts the originals back; the journal also survives a crash.
pub(crate) struct RestoredFiles {
    root: PathBuf,
    directory: PathBuf,
    journal: Vec<RestorePath>,
    committed: bool,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}
impl RestoredFiles {
    pub(crate) fn undo(mut self) -> Result<()> {
        recover_files(&self.root, &self.directory, &self.journal)?;
        self.committed = true;
        Ok(())
    }
    pub(crate) fn commit(mut self) -> Result<()> {
        let completed = self
            .directory
            .with_file_name("checkpoint-restore-completed");
        if completed.exists() {
            std::fs::remove_dir_all(&completed)?;
        }
        std::fs::rename(&self.directory, &completed)?;
        self.committed = true;
        if std::fs::remove_dir_all(completed).is_err() {
            tracing::warn!(
                operation = "checkpoint.restore.cleanup",
                message = "completed restore cleanup deferred"
            );
        }
        Ok(())
    }
}
impl Drop for RestoredFiles {
    fn drop(&mut self) {
        if !self.committed && recover_files(&self.root, &self.directory, &self.journal).is_err() {
            tracing::error!(
                operation = "checkpoint.restore.recover",
                message = "restore journal retained for recovery"
            );
        }
    }
}

impl Checkpoints {
    fn lock(&self, cwd: &Path) -> Arc<AsyncMutex<()>> {
        let mut locks = self
            .workspaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() != 0);
        let key = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.into());
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }
    async fn workspace_lock(&self, cwd: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let workspace = checkout_root(cwd).await.unwrap_or_else(|_| cwd.into());
        self.lock(&workspace).lock_owned().await
    }

    /// Whether `cwd` is inside a Git work tree; false when Git cannot tell.
    pub(crate) async fn is_git_repository(cwd: &Path) -> bool {
        run(cwd, &["rev-parse", "--is-inside-work-tree"], None, 4096)
            .await
            .is_ok_and(|output| {
                output.success && String::from_utf8_lossy(&output.stdout).trim() == "true"
            })
    }

    pub(crate) async fn has(&self, cwd: &Path, reference: &str) -> Result<bool> {
        Ok(commit_of(cwd, reference).await?.is_some())
    }

    /// Writes the checkout (tracked and untracked files) to `reference`.
    pub(crate) async fn capture(&self, cwd: &Path, reference: &str) -> Result<()> {
        let _guard = self.workspace_lock(cwd).await;
        if !Self::is_git_repository(cwd).await {
            return Err(anyhow!("the workspace is not a Git work tree"));
        }
        let temporary = tempfile::tempdir()?;
        let index = temporary.path().join("index");
        let sparse = matches!(
            text(cwd, &["config", "--bool", "core.sparseCheckout"], None)
                .await
                .as_deref(),
            Ok("true")
        );
        if sparse
            && !matches!(
                text(cwd, &["config", "--bool", "core.sparseCheckoutCone"], None)
                    .await
                    .as_deref(),
                Ok("true")
            )
        {
            return Err(anyhow!(
                "Cannot rebuild a checkpoint index for non-cone sparse checkout."
            ));
        }
        let source_index = text(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            None,
        )
        .await?;
        match tokio::fs::copy(source_index.trim(), &index).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if commit_of(cwd, "HEAD").await?.is_some() {
                    git(cwd, &["read-tree", "HEAD"], Some(&index)).await?;
                } else {
                    git(cwd, &["read-tree", "--empty"], Some(&index)).await?;
                }
            }
            Err(error) => return Err(error.into()),
        }
        let args: &[&str] = if sparse {
            &["add", "--all", "--sparse", "--", "."]
        } else {
            &["add", "--all", "--", "."]
        };
        git(cwd, args, Some(&index)).await?;
        let mut args = DURABLE.to_vec();
        args.push("write-tree");
        let tree = text(cwd, &args, Some(&index)).await?;
        let message = format!("checkpoint ref={reference}");
        let mut args = DURABLE.to_vec();
        args.extend(["commit-tree", &tree, "-m", &message]);
        let commit = text(cwd, &args, Some(&index)).await?;
        let mut args = DURABLE.to_vec();
        args.extend(["update-ref", reference, &commit]);
        git(cwd, &args, None).await?;
        Ok(())
    }

    /// Discards originals a recorded restore still keeps aside; the restored files stay.
    pub(crate) async fn finish_restore(&self, cwd: &Path) -> Result<()> {
        let cwd = dunce::canonicalize(cwd)?;
        let root = checkout_root(&cwd).await?;
        let _guard = self.lock(&root).lock_owned().await;
        let git_dir = PathBuf::from(
            text(
                &root,
                &["rev-parse", "--path-format=absolute", "--git-dir"],
                None,
            )
            .await?,
        );
        for name in ["checkpoint-restore", "checkpoint-restore-completed"] {
            let directory = git_dir.join(name);
            if directory.exists() {
                std::fs::remove_dir_all(directory)?;
            }
        }
        Ok(())
    }

    /// Stages the checkpoint's files under `cwd` in place, keeping the originals aside.
    pub(crate) async fn prepare_restore(
        &self,
        cwd: &Path,
        reference: &str,
    ) -> Result<RestoredFiles> {
        let cwd = dunce::canonicalize(cwd)?;
        let root = checkout_root(&cwd).await?;
        let guard = self.lock(&root).lock_owned().await;
        let relative = cwd.strip_prefix(&root)?;
        let prefix = if relative.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            relative
                .to_str()
                .context("checkpoint path is not UTF-8")?
                .to_owned()
        };
        let git_dir = PathBuf::from(
            text(
                &root,
                &["rev-parse", "--path-format=absolute", "--git-dir"],
                None,
            )
            .await?,
        );
        let directory = git_dir.join("checkpoint-restore");
        // A process may have died between native rollback and committing the
        // staged files. Recover the previous contents before retrying.
        if directory.join("journal.json").exists() {
            let journal: Vec<RestorePath> =
                serde_json::from_slice(&std::fs::read(directory.join("journal.json"))?)?;
            recover_files(&root, &directory, &journal)?;
        }
        if directory.exists() {
            std::fs::remove_dir_all(&directory)?;
        }
        let saved = git(
            &root,
            &["ls-tree", "-rz", "--full-tree", reference, "--", &prefix],
            None,
        )
        .await?;
        let mut saved_paths = std::collections::BTreeSet::new();
        for entry in saved.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let entry = std::str::from_utf8(entry)?;
            let (metadata, path) = entry.split_once('\t').context("invalid checkpoint tree")?;
            if metadata.starts_with("160000 ") {
                return Err(anyhow!("checkpoint restore does not replace submodules"));
            }
            validate_restore_path(&root, &cwd, path)?;
            saved_paths.insert(path.to_owned());
        }
        let current = git(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                &prefix,
            ],
            None,
        )
        .await?;
        let mut paths = saved_paths.clone();
        for path in current.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = std::str::from_utf8(path)?;
            validate_restore_path(&root, &cwd, path)?;
            paths.insert(path.to_owned());
        }
        let journal: Vec<_> = paths
            .into_iter()
            .map(|path| RestorePath {
                existed: root.join(&path).symlink_metadata().is_ok(),
                path,
            })
            .collect();
        std::fs::create_dir_all(directory.join("stage"))?;
        std::fs::create_dir_all(directory.join("backup"))?;
        let index = directory.join("index");
        git(&root, &["read-tree", reference], Some(&index)).await?;
        let stage = format!(
            "{}/",
            directory
                .join("stage")
                .to_str()
                .context("restore staging path is not UTF-8")?
        );
        git(
            &root,
            &["checkout-index", "--all", "--force", "--prefix", &stage],
            Some(&index),
        )
        .await?;
        let mut file = std::fs::File::create(directory.join("journal.json"))?;
        use std::io::Write as _;
        file.write_all(&serde_json::to_vec(&journal)?)?;
        file.sync_all()?;
        std::fs::File::open(&directory)?.sync_all()?;
        let restored = RestoredFiles {
            root,
            directory,
            journal,
            committed: false,
            _guard: guard,
        };
        let result = (|| -> Result<()> {
            for entry in &restored.journal {
                if entry.existed {
                    let backup = restored.directory.join("backup").join(&entry.path);
                    std::fs::create_dir_all(backup.parent().context("backup parent missing")?)?;
                    std::fs::rename(restored.root.join(&entry.path), backup)?;
                }
            }
            for path in &saved_paths {
                let target = restored.root.join(path);
                std::fs::create_dir_all(target.parent().context("restore parent missing")?)?;
                std::fs::rename(restored.directory.join("stage").join(path), target)?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            restored.undo()?;
            return Err(error);
        }
        Ok(restored)
    }

    /// Missing references are ignored.
    pub(crate) async fn delete(&self, cwd: &Path, references: &[String]) -> Result<()> {
        let _guard = self.workspace_lock(cwd).await;
        for reference in references {
            let mut args = DURABLE.to_vec();
            args.extend(["update-ref", "-d", reference]);
            run(cwd, &args, None, 4096).await?;
        }
        Ok(())
    }

    /// Changes between two checkpoints. With `fallback_from_head`, a missing
    /// `from` reference reads as HEAD.
    pub(crate) async fn diff(
        &self,
        cwd: &Path,
        from: &str,
        to: &str,
        ignore_whitespace: bool,
        format: DiffFormat,
        fallback_from_head: bool,
    ) -> Result<String> {
        let mut from = from.to_owned();
        if fallback_from_head {
            from = match commit_of(cwd, &from).await? {
                Some(commit) => commit,
                None => commit_of(cwd, "HEAD")
                    .await?
                    .context("Checkpoint ref is unavailable for diff operation.")?,
            };
        }
        let mut args = vec!["diff"];
        args.extend(match format {
            DiffFormat::Numstat => ["--numstat", "-z"].as_slice(),
            DiffFormat::Patch => ["--patch"].as_slice(),
        });
        args.extend([
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ]);
        if ignore_whitespace {
            args.push("--ignore-all-space");
        }
        let (from, to) = (format!("{from}^{{commit}}"), format!("{to}^{{commit}}"));
        args.extend([from.as_str(), to.as_str()]);
        let mut output = run(cwd, &args, None, DIFF_LIMIT).await?;
        if !output.success {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(anyhow!(if detail.is_empty() {
                "Checkpoint ref is unavailable for diff operation.".into()
            } else {
                detail
            }));
        }
        if output.stdout.len() > DIFF_LIMIT {
            if format == DiffFormat::Numstat {
                return Err(anyhow!("checkpoint summary exceeds its limit"));
            }
            output.stdout.truncate(DIFF_LIMIT);
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests;
