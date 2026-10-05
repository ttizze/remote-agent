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

impl Checkpoints {
    pub async fn restore(&self, scope: &CheckpointScope, checkpoint: &Checkpoint) -> Result<()> {
        let cwd = Path::new(&scope.cwd);
        let lock = self.lock(cwd);
        let _guard = lock.lock().await;
        let root = PathBuf::from(text(cwd, &["rev-parse", "--show-toplevel"], None).await?);
        let saved = git(
            cwd,
            &[
                "ls-tree",
                "-rz",
                "--full-tree",
                "--name-only",
                checkpoint.reference.as_str(),
            ],
            None,
        )
        .await?;
        let saved: std::collections::HashSet<_> =
            saved.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
        let temporary = tempfile::tempdir()?;
        let index = temporary.path().join("index");
        git(
            cwd,
            &["read-tree", checkpoint.reference.as_str()],
            Some(&index),
        )
        .await?;
        // Use checkout-relative names even when the thread's cwd is nested.
        let current = git(
            &root,
            &[
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ],
            None,
        )
        .await?;
        let removed = current
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty() && !saved.contains(p))
            .map(|p| String::from_utf8(p.to_vec()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for path in removed {
            let path = root.join(path);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let mut parent = path.parent();
            while let Some(directory) = parent.filter(|p| *p != root && p.starts_with(&root)) {
                match std::fs::remove_dir(directory) {
                    Ok(()) => parent = directory.parent(),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                        ) =>
                    {
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        git(&root, &["checkout-index", "--all", "--force"], Some(&index)).await?;
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
    pub async fn baseline(
        &self,
        scope: &CheckpointScope,
        ordinal: u64,
        now: &Timestamp,
    ) -> Vec<EventPayload> {
        let lock = self.lock(Path::new(&scope.cwd));
        let _guard = lock.lock().await;
        let mut events = vec![EventPayload::CheckpointScopeCreated(scope.clone())];
        let mut ordinals = vec![0];
        if ordinal > 0 {
            ordinals.push(ordinal);
        }
        for ordinal in ordinals {
            let reference = reference(&scope.id, ordinal);
            let status = Self::capture_ref(Path::new(&scope.cwd), &reference)
                .await
                .unwrap_or(CheckpointStatus::Error);
            events.push(EventPayload::CheckpointCaptured(record(
                scope,
                None,
                ordinal,
                reference,
                status,
                vec![],
                now,
            )));
        }
        events
    }
    pub async fn capture(
        &self,
        scope: &CheckpointScope,
        run_id: &RunId,
        node_id: &NodeId,
        ordinal: u64,
        now: &Timestamp,
    ) -> Checkpoint {
        let lock = self.lock(Path::new(&scope.cwd));
        let _guard = lock.lock().await;
        let reference = reference(&scope.id, ordinal);
        let status = Self::capture_ref(Path::new(&scope.cwd), &reference)
            .await
            .unwrap_or(CheckpointStatus::Error);
        let mut files = vec![];
        if status == CheckpointStatus::Ready {
            let previous = self::reference(&scope.id, ordinal.saturating_sub(1));
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
        let lock = self.lock(Path::new(cwd));
        let _guard = lock.lock().await;
        let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
        if ignore_whitespace {
            args.push("--ignore-all-space");
        }
        args.extend([from.reference.as_str(), to.reference.as_str(), "--"]);
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
        parent_checkpoint_id: (ordinal > 0).then(|| {
            CheckpointId::new(format!("checkpoint:{}:{}", scope.id, ordinal - 1))
                .expect("derived id")
        }),
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
        let baseline = owner.baseline(&scope, 0, &scope.created_at).await;
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
