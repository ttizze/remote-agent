//! Git-backed T3 checkpoints. Private indexes leave the user's index and HEAD untouched.
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use orchestration::*;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, Weak},
};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

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

pub(crate) fn reference(scope: &CheckpointScopeId, ordinal: u64) -> CheckpointRef {
    let hash = ring::digest::digest(&ring::digest::SHA256, scope.as_str().as_bytes());
    let hex: String = hash.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    CheckpointRef::new(format!(
        "refs/t3/orchestration-v2/checkpoints/{}/ordinal/{ordinal}",
        URL_SAFE_NO_PAD.encode(hex)
    ))
    .expect("derived ref")
}

fn before_reference(scope: &CheckpointScopeId, run: &RunId) -> CheckpointRef {
    let baseline = reference(scope, 0);
    let (namespace, _) = baseline
        .as_str()
        .rsplit_once("/ordinal/")
        .expect("checkpoint namespace");
    CheckpointRef::new(format!(
        "{namespace}/before/{}",
        URL_SAFE_NO_PAD.encode(run.as_str())
    ))
    .expect("derived ref")
}

async fn git(cwd: &Path, args: &[&str], index: Option<&Path>) -> Result<Vec<u8>> {
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
        .env("GIT_AUTHOR_NAME", "Bex")
        .env("GIT_AUTHOR_EMAIL", "bex@users.noreply.github.com")
        .env("GIT_COMMITTER_NAME", "Bex")
        .env("GIT_COMMITTER_EMAIL", "bex@users.noreply.github.com")
        .kill_on_drop(true);
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let mut child = command.spawn().context("start checkpoint Git operation")?;
    async fn read(pipe: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![];
        pipe.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > limit {
            return Err(anyhow!("checkpoint Git output exceeds its limit"));
        }
        Ok(bytes)
    }
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (status, stdout, stderr) =
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::try_join!(
                async { child.wait().await.map_err(anyhow::Error::from) },
                read(stdout, 8 * 1024 * 1024),
                read(stderr, 64 * 1024)
            )
        })
        .await
        .context("checkpoint Git operation timed out")??;
    if !status.success() {
        return Err(anyhow!(String::from_utf8_lossy(&stderr).trim().to_owned()));
    }
    Ok(stdout)
}
async fn text(cwd: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    Ok(String::from_utf8(git(cwd, args, index).await?)?
        .trim()
        .into())
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
/// Retains the original files until provider rollback and its durable events
/// succeed. Drop compensates any error; the journal also survives a crash.
pub(crate) struct RestoredFiles {
    root: PathBuf,
    directory: PathBuf,
    journal: Vec<RestorePath>,
    committed: bool,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}
impl RestoredFiles {
    fn undo(&mut self) -> Result<()> {
        recover_files(&self.root, &self.directory, &self.journal)?;
        self.committed = true;
        Ok(())
    }
    pub fn commit(mut self) -> Result<()> {
        let completed = self
            .directory
            .with_file_name("bex-checkpoint-restore-completed");
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
        if !self.committed && self.undo().is_err() {
            tracing::error!(
                operation = "checkpoint.restore.recover",
                message = "restore journal retained for recovery"
            );
        }
    }
}

impl Checkpoints {
    #[cfg(test)]
    pub async fn restore(&self, scope: &CheckpointScope, checkpoint: &Checkpoint) -> Result<()> {
        self.prepare_restore(scope, checkpoint).await?.commit()
    }
    pub async fn prepare_restore(
        &self,
        scope: &CheckpointScope,
        checkpoint: &Checkpoint,
    ) -> Result<RestoredFiles> {
        let cwd = dunce::canonicalize(&scope.cwd)?;
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
        let directory = git_dir.join("bex-checkpoint-restore");
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
            &[
                "ls-tree",
                "-rz",
                "--full-tree",
                checkpoint.reference.as_str(),
                "--",
                &prefix,
            ],
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
        git(
            &root,
            &["read-tree", checkpoint.reference.as_str()],
            Some(&index),
        )
        .await?;
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
        let mut restored = RestoredFiles {
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
    pub async fn delete_stale_refs(
        &self,
        scope: &CheckpointScope,
        checkpoints: &[Checkpoint],
    ) -> Result<()> {
        let root = checkout_root(Path::new(&scope.cwd)).await?;
        let _guard = self.lock(&root).lock_owned().await;
        for checkpoint in checkpoints.iter().filter(|c| c.scope_id == scope.id) {
            let mut args = DURABLE.to_vec();
            args.extend(["update-ref", "-d", checkpoint.reference.as_str()]);
            git(&root, &args, None).await?;
        }
        Ok(())
    }
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
    async fn capture_ref(cwd: &Path, reference: &CheckpointRef) -> Result<CheckpointStatus> {
        if !matches!(
            text(cwd, &["rev-parse", "--is-inside-work-tree"], None)
                .await
                .as_deref(),
            Ok("true")
        ) {
            return Ok(CheckpointStatus::Missing);
        }
        if text(
            cwd,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", reference),
            ],
            None,
        )
        .await
        .is_ok()
        {
            return Ok(CheckpointStatus::Ready);
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
                if text(cwd, &["rev-parse", "--verify", "HEAD"], None)
                    .await
                    .is_ok()
                {
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
        let mut args = DURABLE.to_vec();
        args.extend(["commit-tree", &tree, "-m", "Bex orchestration checkpoint"]);
        let commit = text(cwd, &args, Some(&index)).await?;
        let mut args = DURABLE.to_vec();
        args.extend(["update-ref", reference.as_str(), &commit]);
        git(cwd, &args, None).await?;
        Ok(CheckpointStatus::Ready)
    }
    pub async fn baseline(&self, scope: &CheckpointScope, now: &Timestamp) -> Vec<EventPayload> {
        let workspace = checkout_root(Path::new(&scope.cwd))
            .await
            .unwrap_or_else(|_| PathBuf::from(&scope.cwd));
        let lock = self.lock(&workspace);
        let _guard = lock.lock().await;
        let reference = reference(&scope.id, 0);
        let status = Self::capture_ref(Path::new(&scope.cwd), &reference)
            .await
            .unwrap_or(CheckpointStatus::Error);
        vec![
            EventPayload::CheckpointScopeCreated(scope.clone()),
            EventPayload::CheckpointCaptured(record(
                scope,
                None,
                0,
                reference,
                status,
                vec![],
                now,
            )),
        ]
    }
    pub async fn prepare_run(
        &self,
        scope: &CheckpointScope,
        run_id: &RunId,
        ordinal: u64,
        previous: Option<&Checkpoint>,
        now: &Timestamp,
    ) -> Vec<EventPayload> {
        let mut payloads = self.baseline(scope, now).await;
        let reference = before_reference(&scope.id, run_id);
        let workspace = checkout_root(Path::new(&scope.cwd))
            .await
            .unwrap_or_else(|_| PathBuf::from(&scope.cwd));
        let lock = self.lock(&workspace);
        let _guard = lock.lock().await;
        let status = Self::capture_ref(Path::new(&scope.cwd), &reference)
            .await
            .unwrap_or(CheckpointStatus::Error);
        let mut before = record(
            scope,
            Some(run_id.clone()),
            previous.and_then(|c| c.app_run_ordinal).unwrap_or(0),
            reference,
            status,
            vec![],
            now,
        );
        before.id = orchestration::checkpoint::before_run_id(&scope.id, run_id);
        before.ordinal_within_scope = ordinal;
        before.parent_checkpoint_id = previous.map(|c| c.id.clone());
        payloads.push(EventPayload::CheckpointCaptured(before));
        payloads
    }
    pub async fn capture(
        &self,
        scope: &CheckpointScope,
        run_id: &RunId,
        node_id: &NodeId,
        ordinal: u64,
        now: &Timestamp,
    ) -> Checkpoint {
        let workspace = checkout_root(Path::new(&scope.cwd))
            .await
            .unwrap_or_else(|_| PathBuf::from(&scope.cwd));
        let lock = self.lock(&workspace);
        let _guard = lock.lock().await;
        let reference = reference(&scope.id, ordinal);
        let status = Self::capture_ref(Path::new(&scope.cwd), &reference)
            .await
            .unwrap_or(CheckpointStatus::Error);
        let before = before_reference(&scope.id, run_id);
        let has_before = text(
            Path::new(&scope.cwd),
            &["rev-parse", "--verify", before.as_str()],
            None,
        )
        .await
        .is_ok();
        let mut files = vec![];
        if status == CheckpointStatus::Ready {
            let previous = if has_before {
                before
            } else {
                self::reference(&scope.id, 0)
            };
            if let Ok(bytes) = git(
                Path::new(&scope.cwd),
                &[
                    "diff",
                    "--numstat",
                    "-z",
                    "--no-ext-diff",
                    "--no-textconv",
                    previous.as_str(),
                    reference.as_str(),
                    "--",
                    ".",
                ],
                None,
            )
            .await
            {
                crate::workspace_review::parse_numstat(&bytes, |path, additions, deletions| {
                    files.push(CheckpointFileSummary {
                        path: String::from_utf8_lossy(path).into(),
                        kind: "modified".into(),
                        additions: additions.unwrap_or(0),
                        deletions: deletions.unwrap_or(0),
                    });
                });
            }
        }
        let mut checkpoint = record(
            scope,
            Some(run_id.clone()),
            ordinal,
            reference,
            status,
            files,
            now,
        );
        checkpoint.node_id = node_id.clone();
        if has_before {
            checkpoint.parent_checkpoint_id =
                Some(orchestration::checkpoint::before_run_id(&scope.id, run_id));
        }
        checkpoint
    }
    pub async fn diff(
        &self,
        cwd: &str,
        from: &Checkpoint,
        to: &Checkpoint,
        ignore_whitespace: bool,
    ) -> Result<String> {
        if from.scope_id != to.scope_id
            || from.status != CheckpointStatus::Ready
            || to.status != CheckpointStatus::Ready
        {
            return Err(anyhow!("checkpoint unavailable"));
        }
        let workspace = checkout_root(Path::new(cwd))
            .await
            .unwrap_or_else(|_| PathBuf::from(cwd));
        let lock = self.lock(&workspace);
        let _guard = lock.lock().await;
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if ignore_whitespace {
            args.push("--ignore-all-space");
        }
        args.extend([from.reference.as_str(), to.reference.as_str(), "--", "."]);
        Ok(String::from_utf8(git(Path::new(cwd), &args, None).await?)?)
    }
}
fn record(
    scope: &CheckpointScope,
    run_id: Option<RunId>,
    ordinal: u64,
    reference: CheckpointRef,
    status: CheckpointStatus,
    files: Vec<CheckpointFileSummary>,
    now: &Timestamp,
) -> Checkpoint {
    Checkpoint {
        id: CheckpointId::new(format!("checkpoint:{}:{ordinal}", scope.id)).expect("derived id"),
        thread_id: scope.thread_id.clone(),
        scope_id: scope.id.clone(),
        run_id,
        node_id: scope.node_id.clone(),
        parent_checkpoint_id: None,
        ordinal_within_scope: ordinal,
        app_run_ordinal: Some(ordinal),
        reference,
        status,
        files,
        captured_at: now.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(cwd: &Path) -> CheckpointScope {
        CheckpointScope {
            id: CheckpointScopeId::new("scope:thread:root").unwrap(),
            thread_id: ThreadId::new("thread").unwrap(),
            run_id: Some(RunId::new("run").unwrap()),
            node_id: NodeId::new("root").unwrap(),
            parent_scope_id: None,
            provider_thread_id: Some(ProviderThreadId::new("provider-thread").unwrap()),
            kind: ScopeKind::RootRun,
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: cwd.to_string_lossy().into(),
            created_at: Timestamp::from_millis(0).unwrap(),
        }
    }
    async fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"], None).await.unwrap();
        dir
    }
    #[tokio::test]
    async fn restore_recovers_files_and_removes_later_untracked_files_without_changing_head_or_index()
     {
        let dir = repo().await;
        let cwd = dir.path();
        std::fs::write(cwd.join("tracked"), "before\n").unwrap();
        std::fs::write(cwd.join(".gitignore"), "ignored\n").unwrap();
        git(cwd, &["add", "."], None).await.unwrap();
        git(cwd, &["commit", "-m", "initial"], None).await.unwrap();
        std::fs::write(cwd.join("tracked"), "staged\n").unwrap();
        git(cwd, &["add", "tracked"], None).await.unwrap();
        std::fs::write(cwd.join("tracked"), "checkpoint\n").unwrap();
        std::fs::write(cwd.join("untracked"), "saved\n").unwrap();
        let checkpoints = Checkpoints::default();
        let scope = scope(cwd);
        let checkpoint = checkpoints
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &Timestamp::from_millis(0).unwrap(),
            )
            .await;
        let index = std::fs::read(cwd.join(".git/index")).unwrap();
        let head = text(cwd, &["rev-parse", "HEAD"], None).await.unwrap();
        std::fs::write(cwd.join("tracked"), "later\n").unwrap();
        std::fs::remove_file(cwd.join("untracked")).unwrap();
        std::fs::write(cwd.join("new"), "later\n").unwrap();
        std::fs::write(cwd.join("ignored"), "keep\n").unwrap();
        checkpoints.restore(&scope, &checkpoint).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("tracked")).unwrap(),
            "checkpoint\n"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("untracked")).unwrap(),
            "saved\n"
        );
        assert!(!cwd.join("new").exists());
        assert_eq!(
            std::fs::read_to_string(cwd.join("ignored")).unwrap(),
            "keep\n"
        );
        assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
        assert_eq!(text(cwd, &["rev-parse", "HEAD"], None).await.unwrap(), head);
    }
    #[tokio::test]
    async fn captures_worktree_and_untracked_files_without_mutating_index_or_head() {
        let dir = repo().await;
        let cwd = dir.path();
        std::fs::write(cwd.join("tracked"), "original\n").unwrap();
        std::fs::write(cwd.join(".gitignore"), "ignored\n").unwrap();
        git(cwd, &["add", "."], None).await.unwrap();
        git(cwd, &["commit", "-m", "initial"], None).await.unwrap();
        std::fs::write(cwd.join("tracked"), "staged\n").unwrap();
        git(cwd, &["add", "tracked"], None).await.unwrap();
        std::fs::write(cwd.join("tracked"), "baseline\n").unwrap();
        let index = std::fs::read(cwd.join(".git/index")).unwrap();
        let head = text(cwd, &["rev-parse", "HEAD"], None).await.unwrap();
        let owner = Checkpoints::default();
        let scope = scope(cwd);
        let baseline = owner.baseline(&scope, &scope.created_at).await;
        let EventPayload::CheckpointCaptured(from) = &baseline[1] else {
            panic!("baseline")
        };
        std::fs::write(cwd.join("tracked"), "changed\nmore\n").unwrap();
        std::fs::write(cwd.join("untracked"), "new\n").unwrap();
        std::fs::write(cwd.join("ignored"), "ignored\n").unwrap();
        let to = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(to.status, CheckpointStatus::Ready);
        assert_eq!(
            text(cwd, &["show", &format!("{}:tracked", to.reference)], None)
                .await
                .unwrap(),
            "changed\nmore"
        );
        assert!(
            text(cwd, &["show", &format!("{}:ignored", to.reference)], None)
                .await
                .is_err()
        );
        assert_eq!(to.files.len(), 2);
        let diff = owner.diff(&scope.cwd, from, &to, false).await.unwrap();
        assert!(diff.contains("-baseline\n"));
        assert!(diff.contains("+changed\n"));
        assert!(diff.contains("untracked"));
        std::fs::write(cwd.join("tracked"), "later\n").unwrap();
        let replay = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(replay, to);
        assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
        assert_eq!(text(cwd, &["rev-parse", "HEAD"], None).await.unwrap(), head);
        assert_eq!(
            text(cwd, &["show", ":tracked"], None).await.unwrap(),
            "staged"
        );
    }
    #[tokio::test]
    async fn nested_workspace_capture_does_not_stage_sibling_changes() {
        let dir = repo().await;
        let cwd = dir.path();
        std::fs::create_dir(cwd.join("scope")).unwrap();
        std::fs::write(cwd.join("scope/file"), "before\n").unwrap();
        std::fs::write(cwd.join("sibling"), "original\n").unwrap();
        git(cwd, &["add", "."], None).await.unwrap();
        git(cwd, &["commit", "-m", "initial"], None).await.unwrap();
        std::fs::write(cwd.join("scope/file"), "inside\n").unwrap();
        std::fs::write(cwd.join("sibling"), "outside\n").unwrap();
        let scope = scope(&cwd.join("scope"));
        let owner = Checkpoints::default();
        let captured = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(captured.status, CheckpointStatus::Ready);
        assert_eq!(
            text(
                cwd,
                &["show", &format!("{}:scope/file", captured.reference)],
                None
            )
            .await
            .unwrap(),
            "inside"
        );
        assert_eq!(
            text(
                cwd,
                &["show", &format!("{}:sibling", captured.reference)],
                None
            )
            .await
            .unwrap(),
            "original"
        );
        std::fs::write(cwd.join("scope/file"), "later\n").unwrap();
        std::fs::write(cwd.join("scope/new"), "remove\n").unwrap();
        owner.restore(&scope, &captured).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(cwd.join("scope/file")).unwrap(),
            "inside\n"
        );
        assert_eq!(
            std::fs::read_to_string(cwd.join("sibling")).unwrap(),
            "outside\n"
        );
        assert!(!cwd.join("scope/new").exists());
    }
    #[tokio::test]
    async fn restore_compensates_provider_failure_and_recovers_an_interrupted_transaction() {
        let dir = repo().await;
        std::fs::write(dir.path().join("file"), "checkpoint").unwrap();
        let scope = scope(dir.path());
        let owner = Checkpoints::default();
        let checkpoint = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        std::fs::write(dir.path().join("file"), "later").unwrap();
        std::fs::write(dir.path().join("new"), "keep on failure").unwrap();
        let restored = owner.prepare_restore(&scope, &checkpoint).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file")).unwrap(),
            "checkpoint"
        );
        drop(restored);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file")).unwrap(),
            "later"
        );
        assert!(dir.path().join("new").exists());
        let mut restored = owner.prepare_restore(&scope, &checkpoint).await.unwrap();
        // Preserve the journal as if the process exited without destructors.
        restored.committed = true;
        drop(restored);
        let restored = owner.prepare_restore(&scope, &checkpoint).await.unwrap();
        drop(restored);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("file")).unwrap(),
            "later"
        );
        assert!(dir.path().join("new").exists());
    }
    #[tokio::test]
    async fn restore_rejects_path_collisions_before_mutating_any_files() {
        let dir = repo().await;
        std::fs::write(dir.path().join("a"), "checkpoint").unwrap();
        std::fs::write(dir.path().join("z"), "checkpoint").unwrap();
        let scope = scope(dir.path());
        let owner = Checkpoints::default();
        let checkpoint = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        std::fs::write(dir.path().join("a"), "later").unwrap();
        std::fs::remove_file(dir.path().join("z")).unwrap();
        std::fs::create_dir(dir.path().join("z")).unwrap();
        std::fs::write(dir.path().join("z/keep"), "keep").unwrap();
        assert!(owner.prepare_restore(&scope, &checkpoint).await.is_err());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a")).unwrap(),
            "later"
        );
        assert!(dir.path().join("z/keep").exists());
    }
    #[tokio::test]
    async fn before_run_links_survive_gaps_and_stale_refs_are_deleted() {
        let dir = repo().await;
        let scope = scope(dir.path());
        let owner = Checkpoints::default();
        std::fs::write(dir.path().join("file"), "baseline\n").unwrap();
        let events = owner.baseline(&scope, &scope.created_at).await;
        let EventPayload::CheckpointCaptured(baseline) = &events[1] else {
            panic!("baseline");
        };
        let run = RunId::new("run-after-gap").unwrap();
        std::fs::write(dir.path().join("file"), "before\n").unwrap();
        let events = owner
            .prepare_run(&scope, &run, 4, Some(baseline), &scope.created_at)
            .await;
        let EventPayload::CheckpointCaptured(before) = events.last().unwrap() else {
            panic!("before");
        };
        assert_eq!(before.app_run_ordinal, Some(0));
        assert_eq!(before.status, CheckpointStatus::Ready);
        assert_eq!(before.parent_checkpoint_id.as_ref(), Some(&baseline.id));
        std::fs::write(dir.path().join("file"), "after\n").unwrap();
        let captured = owner
            .capture(&scope, &run, &scope.node_id, 4, &scope.created_at)
            .await;
        assert_eq!(captured.parent_checkpoint_id.as_ref(), Some(&before.id));
        assert_eq!(captured.files.len(), 1);
        assert!(
            owner
                .diff(&scope.cwd, before, &captured, false)
                .await
                .unwrap()
                .contains("-before\n")
        );
        owner
            .delete_stale_refs(&scope, &[captured.clone(), before.clone()])
            .await
            .unwrap();
        assert!(
            text(
                dir.path(),
                &["rev-parse", "--verify", captured.reference.as_str()],
                None
            )
            .await
            .is_err()
        );
        assert!(
            text(
                dir.path(),
                &["rev-parse", "--verify", before.reference.as_str()],
                None
            )
            .await
            .is_err()
        );
        std::fs::write(dir.path().join("file"), "new checkpoint\n").unwrap();
        let recaptured = owner
            .capture(&scope, &run, &scope.node_id, 4, &scope.created_at)
            .await;
        assert_eq!(
            text(
                dir.path(),
                &["show", &format!("{}:file", recaptured.reference)],
                None
            )
            .await
            .unwrap(),
            "new checkpoint"
        );
    }
    #[tokio::test]
    async fn unborn_and_non_git_workspaces_have_explicit_checkpoint_status() {
        let dir = repo().await;
        let scope = scope(dir.path());
        let owner = Checkpoints::default();
        std::fs::write(dir.path().join("new"), "new\n").unwrap();
        let captured = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(captured.status, CheckpointStatus::Ready);
        assert!(!dir.path().join(".git/index").exists());
        assert!(
            text(dir.path(), &["rev-parse", "--verify", "HEAD"], None)
                .await
                .is_err()
        );
        let non_git = tempfile::tempdir().unwrap();
        let scope = self::scope(non_git.path());
        let missing = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(missing.status, CheckpointStatus::Missing);
        assert!(
            owner
                .diff(&scope.cwd, &missing, &missing, false)
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn sparse_capture_preserves_excluded_files_and_reports_unsupported_non_cone() {
        let dir = repo().await;
        let cwd = dir.path();
        std::fs::create_dir(cwd.join("included")).unwrap();
        std::fs::create_dir(cwd.join("excluded")).unwrap();
        std::fs::write(cwd.join("included/file"), "in\n").unwrap();
        std::fs::write(cwd.join("excluded/file"), "out\n").unwrap();
        git(cwd, &["add", "."], None).await.unwrap();
        git(cwd, &["commit", "-m", "initial"], None).await.unwrap();
        git(
            cwd,
            &[
                "sparse-checkout",
                "set",
                "--cone",
                "--sparse-index",
                "included",
            ],
            None,
        )
        .await
        .unwrap();
        assert!(!cwd.join("excluded/file").exists());
        let index = std::fs::read(cwd.join(".git/index")).unwrap();
        let scope = scope(cwd);
        let owner = Checkpoints::default();
        std::fs::write(cwd.join("included/file"), "changed\n").unwrap();
        let captured = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert_eq!(captured.status, CheckpointStatus::Ready);
        assert_eq!(
            text(
                cwd,
                &["show", &format!("{}:excluded/file", captured.reference)],
                None
            )
            .await
            .unwrap(),
            "out"
        );
        assert_eq!(std::fs::read(cwd.join(".git/index")).unwrap(), index);
        git(
            cwd,
            &["sparse-checkout", "set", "--no-cone", "/included/"],
            None,
        )
        .await
        .unwrap();
        let failed = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                2,
                &scope.created_at,
            )
            .await;
        assert_eq!(failed.status, CheckpointStatus::Error);
    }
    #[tokio::test]
    async fn turn_diff_can_ignore_whitespace_and_rejects_cross_scope_queries() {
        let dir = repo().await;
        let cwd = dir.path();
        std::fs::write(cwd.join("file"), "one two\n").unwrap();
        let scope = scope(cwd);
        let owner = Checkpoints::default();
        let from = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                0,
                &scope.created_at,
            )
            .await;
        std::fs::write(cwd.join("file"), "one  two\n").unwrap();
        let mut to = owner
            .capture(
                &scope,
                scope.run_id.as_ref().unwrap(),
                &scope.node_id,
                1,
                &scope.created_at,
            )
            .await;
        assert!(
            !owner
                .diff(&scope.cwd, &from, &to, false)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            owner
                .diff(&scope.cwd, &from, &to, true)
                .await
                .unwrap()
                .is_empty()
        );
        to.scope_id = CheckpointScopeId::new("different-scope").unwrap();
        assert!(owner.diff(&scope.cwd, &from, &to, false).await.is_err());
    }
}
