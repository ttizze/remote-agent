use agent_domain::{WorktreeSetupStageId, WorktreeSetupStageStatus};
use agent_protocol::models::{
    ConversationSettings, ConversationSettingsPatch, Worktree, WorktreeSettings,
};
use agent_runtime::{SetupEvent, SetupProgress};
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct State {
    settings: WorktreeSettings,
    conversation: ConversationSettings,
    workspace_roots: HashMap<String, String>,
    /// Each thread's checkout, recorded before it is created.
    threads: HashMap<String, ThreadCheckout>,
}

pub(crate) struct Worktrees {
    path: PathBuf,
    /// Held by the blocking work itself, so a caller that stops waiting never
    /// releases it while that work still reads or saves the state.
    lock: Arc<tokio::sync::Mutex<()>>,
    /// The saved conversation settings, for synchronous reads.
    conversation: std::sync::RwLock<ConversationSettings>,
}

impl Worktrees {
    pub(crate) fn new(project_state: &Path) -> Self {
        Self {
            path: project_state.with_file_name("bex-worktrees.json"),
            lock: Default::default(),
            conversation: Default::default(),
        }
    }

    /// Runs `work` on the state file under the lock, which the work keeps
    /// until it returns even when the caller stops waiting.
    async fn locked<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Path) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let guard = self.lock.clone().lock_owned().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            work(&path)
        })
        .await?
    }

    /// The conversation settings as last loaded or saved.
    pub(crate) fn conversation(&self) -> ConversationSettings {
        self.conversation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Reads, or replaces and saves, the conversation settings.
    /// The conversation settings, after merging `update` into them.
    pub(crate) async fn conversation_settings(
        &self,
        update: Option<ConversationSettingsPatch>,
    ) -> Result<ConversationSettings> {
        let settings = self
            .locked(move |path| {
                let mut state = read(path)?;
                if let Some(patch) = update {
                    let settings = state.conversation.patched(&patch);
                    settings.validate().map_err(|error| anyhow!(error))?;
                    state.conversation = settings;
                    save(path, &state)?;
                }
                Ok(state.conversation)
            })
            .await?;
        *self
            .conversation
            .write()
            .unwrap_or_else(|error| error.into_inner()) = settings.clone();
        Ok(settings)
    }

    pub(crate) async fn list(&self) -> Result<Vec<Worktree>> {
        self.locked(move |path| {
            let state = read(path)?;
            let mut entries = state
                .workspace_roots
                .into_iter()
                .map(|(path, project_path)| {
                    let (branch, blocked_reason) = inspect(&path, &project_path)
                        .unwrap_or_else(|error| (String::new(), Some(format!("{error:#}"))));
                    Worktree {
                        path,
                        project_path,
                        branch,
                        blocked_reason,
                        threads: Vec::new(),
                    }
                })
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| (&a.project_path, &a.path).cmp(&(&b.project_path, &b.path)));
            Ok(entries)
        })
        .await
    }

    pub(crate) async fn remove(&self, target: String, require_merged: bool) -> Result<()> {
        self.locked(move |path| {
            let state = read(path)?;
            let root = state
                .workspace_roots
                .get(&target)
                .context("Bexが作成したワークツリーではありません。")?;
            if !already_removed(&target, root)? {
                let (_, blocked) = inspect(&target, root)?;
                if let Some(reason) = blocked {
                    return Err(anyhow!(reason));
                }
                // Recheck both the preference and HEAD after the activity scan.
                // A new commit or a disabled preference cancels automatic removal.
                if require_merged
                    && (!state.settings.delete_merged
                        || directory_status(Path::new(&target), None, None, None)?
                            != Some(agent_protocol::models::WorktreeStatus::Merged))
                {
                    return Ok(());
                }
                // Git rechecks tracked/untracked changes and locks at removal time.
                // Keep the branch so commits remain reachable even if not merged.
                crate::git::text(Path::new(root), &["worktree", "remove", "--", &target])?;
            }
            remove_session_folder(&target, root);
            Ok(())
        })
        .await
    }

    /// Removes a checkout a launch gives up, with whatever its setup changed,
    /// and keeps its branch.
    pub(crate) async fn abandon(&self, target: String) -> Result<()> {
        self.locked(move |path| {
            let state = read(path)?;
            let root = state
                .workspace_roots
                .get(&target)
                .context("Bexが作成したワークツリーではありません。")?;
            if !already_removed(&target, root)? {
                crate::git::text(
                    Path::new(root),
                    &["worktree", "remove", "--force", "--", &target],
                )?;
            }
            remove_session_folder(&target, root);
            Ok(())
        })
        .await
    }

    /// Renames the branch checked out at `cwd` from `old` to `new`, or, unless
    /// `exact`, to the first of `new`, `new-1` … `new-100` that is free, as the
    /// reference's renameBranch does. Git validates the name. A thread whose
    /// checkout had `old` keeps it under the new name.
    pub(crate) async fn rename_branch(
        &self,
        cwd: String,
        old: String,
        new: String,
        exact: bool,
    ) -> Result<String> {
        self.locked(move |path| {
            if old == new {
                return Ok(new);
            }
            let cwd = Path::new(&cwd);
            let target = if exact {
                new
            } else {
                available_branch_name(cwd, &new)?
            };
            crate::git::text(cwd, &["branch", "-m", "--", &old, &target])?;
            let mut state = read(path)?;
            let root = dunce::canonicalize(
                crate::git::text(cwd, &["rev-parse", "--show-toplevel"])?.trim_end(),
            )?;
            let mut changed = false;
            for checkout in state.threads.values_mut() {
                if checkout.branch == old && Path::new(&checkout.path) == root {
                    checkout.branch = target.clone();
                    changed = true;
                }
            }
            if changed {
                save(path, &state)?;
            }
            Ok(target)
        })
        .await
    }

    /// Recreate a deleted checkout at its persisted path so provider sessions
    /// and every client keep using the same working directory.
    pub(crate) async fn ensure_available(&self, cwd: &str) -> Result<Option<PathBuf>> {
        let cwd = PathBuf::from(cwd);
        self.locked(move |path| {
            match fs::metadata(&cwd) {
                Ok(metadata) if metadata.is_dir() => return Ok(None),
                Ok(_) => return Err(anyhow!("working directory is not a directory")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let state = read(path)?;
            let (target, project) = state
                .workspace_roots
                .iter()
                .filter(|(target, _)| cwd.starts_with(target))
                .max_by_key(|(target, _)| target.len())
                .context("working directory is unavailable")?;
            let destination = Path::new(target);
            // A missing subdirectory in an existing checkout is not a deleted worktree.
            match fs::symlink_metadata(destination) {
                Ok(_) => return Err(anyhow!("working directory is unavailable")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let root = Path::new(project);
            let branch = format!("agent/session-{}", uuid::Uuid::new_v4());
            create_checkout(
                root,
                destination,
                &branch,
                Some("refs/heads/main"),
                if state.settings.copy_on_create {
                    &state.settings.copy_paths
                } else {
                    &[]
                },
                &CancellationToken::new(),
            )?;
            if !cwd.is_dir() {
                return Err(anyhow!("working directory does not exist in the checkout"));
            }
            Ok(Some(destination.to_path_buf()))
        })
        .await
    }

    pub(crate) async fn settings(
        &self,
        update: Option<WorktreeSettings>,
    ) -> Result<WorktreeSettings> {
        self.locked(move |path| {
            let mut state = read(path)?;
            if let Some(settings) = update {
                for entry in &settings.copy_paths {
                    relative_path(entry)?;
                }
                if !settings.worktree_directory.is_empty()
                    && !Path::new(&settings.worktree_directory).is_absolute()
                {
                    return Err(anyhow!(
                        "worktree directory must be an absolute path on the Host, or empty for the default"
                    ));
                }
                state.settings = settings;
                save(path, &state)?;
            }
            Ok(state.settings)
        })
        .await
    }

    /// The checkout of `cwd`'s repository for `thread`, from `base_ref` or, with
    /// `start_from_origin`, from its origin branch when the repository has one.
    /// Returns the checkout's root and its branch. The same thread always gets
    /// the same checkout, so a retry after a crash finds the one it created.
    ///
    /// Once `cancel` fires, or this call is dropped, the Git command running is
    /// stopped and what the call created is removed; the state stays locked
    /// until then.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create(
        &self,
        thread: &str,
        cwd: &str,
        base_ref: &str,
        branch: Option<String>,
        start_from_origin: bool,
        progress: SetupProgress,
        cancel: CancellationToken,
    ) -> Result<(PathBuf, String)> {
        let (thread, cwd, base_ref) = (thread.to_owned(), PathBuf::from(cwd), base_ref.to_owned());
        let stop = cancel.child_token();
        let dropped = stop.clone().drop_guard();
        let created = self
            .locked(move |path| {
                let cancel = stop;
                let mut state = read(path)?;
                let stage = |id, status| progress.report(SetupEvent::Stage(id, status));
                let base = || {
                    // "Start from origin" applies only when the repository has an origin.
                    let from_origin = start_from_origin
                        && crate::git::output(&cwd, &["remote", "get-url", "origin"]).is_ok();
                    stage(
                        WorktreeSetupStageId::Fetch,
                        if from_origin {
                            WorktreeSetupStageStatus::Running
                        } else {
                            WorktreeSetupStageStatus::Skipped
                        },
                    );
                    let start = if from_origin {
                        origin_start(&cwd, &base_ref, &cancel)?
                    } else {
                        base_ref.clone()
                    };
                    if from_origin {
                        stage(WorktreeSetupStageId::Fetch, WorktreeSetupStageStatus::Done);
                    }
                    stage(
                        WorktreeSetupStageId::Checkout,
                        WorktreeSetupStageStatus::Running,
                    );
                    Ok(start)
                };
                checkout(path, &mut state, &thread, &cwd, base, branch, &cancel)
            })
            .await;
        dropped.disarm();
        created
    }
}

/// Removes the empty session folder of the named-checkout layout. Old worktrees
/// and any unrelated files in the folder stay untouched.
fn remove_session_folder(target: &str, root: &str) {
    let target = Path::new(target);
    if target.file_name() == Path::new(root).file_name()
        && let Some(parent) = target.parent()
        && parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("session-"))
    {
        let _ = fs::remove_dir(parent);
    }
}

/// The commit a worktree "started from origin" begins at: the fetched origin
/// branch, or the local `base_ref` when origin has no such branch.
fn origin_start(cwd: &Path, base_ref: &str, cancel: &CancellationToken) -> Result<String> {
    fetch_origin(cwd, base_ref, cancel)?;
    let remote = format!("refs/remotes/origin/{base_ref}");
    if crate::git::output(cwd, &["show-ref", "--verify", "--quiet", &remote]).is_err() {
        return Ok(base_ref.to_owned());
    }
    Ok(crate::git::text(
        cwd,
        &["rev-parse", "--verify", &format!("{remote}^{{commit}}")],
    )?
    .trim()
    .to_owned())
}

/// Fetches `origin`: the branch, or every branch when origin has no such
/// branch. Failures report a fixed diagnosis, never Git's output, which can
/// contain remote credentials.
fn fetch_origin(cwd: &Path, base_ref: &str, cancel: &CancellationToken) -> Result<()> {
    let fetch = |refspec: Option<&str>| {
        let mut command = std::process::Command::new("git");
        command
            .args(["fetch", "--quiet", "--end-of-options", "origin"])
            .args(refspec)
            .current_dir(cwd)
            .env("LC_ALL", "C")
            .env("GCM_INTERACTIVE", "never")
            .env("GIT_ASKPASS", "")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("SSH_ASKPASS", "")
            .env("SSH_ASKPASS_REQUIRE", "never");
        let output = crate::git::run_cancellable(command, cancel)?;
        Ok::<_, anyhow::Error>((
            output.status.success(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    };
    let failed = |stderr: &str| {
        anyhow!(
            "Git command failed in GitVcsDriver.fetchRemote ({}): {}",
            cwd.display(),
            fetch_failure_detail(stderr).unwrap_or("git fetch origin failed")
        )
    };
    let branch = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    let (fetched, stderr) = fetch(Some(&format!(
        "+refs/heads/{branch}:refs/remotes/origin/{branch}"
    )))?;
    if fetched {
        return Ok(());
    }
    let missing = format!("fatal: couldn't find remote ref refs/heads/{branch}");
    if !stderr.lines().any(|line| line == missing) {
        return Err(failed(&stderr));
    }
    match fetch(None)? {
        (true, _) => Ok(()),
        (false, stderr) => Err(failed(&stderr)),
    }
}

/// A fixed diagnosis for recognized fetch failures.
fn fetch_failure_detail(stderr: &str) -> Option<&'static str> {
    // `prefix` followed by a word boundary (a regex `\b`).
    fn word(line: &str, prefix: &str) -> bool {
        line.strip_prefix(prefix)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
    }
    let authentication = |line: &str| {
        word(line, "fatal: authentication failed")
            || word(line, "fatal: could not read username")
            || word(line, "fatal: could not read password")
            || line
                .split_once(": permission denied (publickey")
                .is_some_and(|(host, _)| !host.is_empty() && !host.contains(char::is_whitespace))
    };
    let unreachable = |line: &str| {
        ["fatal: ", "ssh: ", ""].iter().any(|lead| {
            word(line, &format!("{lead}could not resolve host"))
                || word(line, &format!("{lead}could not resolve hostname"))
        }) || (line.starts_with("fatal: unable to access ")
            && [": could not resolve host", ": failed to connect"]
                .iter()
                .any(|reason| line.contains(reason)))
            || (line.starts_with("ssh: connect to host ")
                && line.contains(" port ")
                && [
                    ": connection timed out",
                    ": connection refused",
                    ": network is unreachable",
                ]
                .iter()
                .any(|reason| line.contains(reason)))
    };
    let inaccessible = |line: &str| {
        line == "remote: repository not found."
            || line == "remote: repository not found"
            || (line.starts_with("fatal: repository ") && line.ends_with(" not found"))
            || (line.starts_with("fatal: ")
                && line.ends_with(" does not appear to be a git repository"))
    };
    let locked = |line: &str| {
        word(line, "error: cannot lock ref")
            || word(line, "fatal: cannot lock ref")
            || ((line.starts_with("fatal: unable to create '")
                || line.starts_with("fatal: unable to create \""))
                && (line.contains(".lock':") || line.contains(".lock\":")))
    };
    let lines: Vec<String> = stderr
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .collect();
    type Check<'a> = (&'a dyn Fn(&str) -> bool, &'static str);
    let checks: [Check; 4] = [
        (
            &authentication,
            "Git could not authenticate with the remote. Check Git credentials or SSH access on the server, then retry.",
        ),
        (
            &unreachable,
            "Git could not reach the remote. Check the server's network connection and remote host, then retry.",
        ),
        (
            &inaccessible,
            "Git could not access the remote repository. Check the remote URL and repository permissions on the server.",
        ),
        (
            &locked,
            "Git could not update a local reference. Another Git operation or a stale lock may be blocking the fetch; check the repository on the server, then retry.",
        ),
    ];
    checks
        .into_iter()
        .find(|(matches, _)| lines.iter().any(|line| matches(line)))
        .map(|(_, detail)| detail)
}

/// A checkout's directory and default branch derive from its thread, so a retried
/// creation finds what an earlier attempt left.
#[derive(Clone, Serialize, Deserialize)]
struct ThreadCheckout {
    path: String,
    branch: String,
}

fn thread_key(thread: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, thread.as_bytes())
        .simple()
        .to_string()[..12]
        .to_owned()
}

fn branch_exists(root: &Path, branch: &str) -> bool {
    crate::git::output(
        root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// `desired`, or the first of `desired-1` … `desired-100` no branch has.
fn available_branch_name(cwd: &Path, desired: &str) -> Result<String> {
    std::iter::once(desired.to_owned())
        .chain((1..=100).map(|suffix| format!("{desired}-{suffix}")))
        .find(|candidate| !branch_exists(cwd, candidate))
        .ok_or_else(|| anyhow!("Could not find an available branch name for '{desired}'."))
}

fn registered(root: &Path, destination: &Path) -> Result<bool> {
    Ok(
        crate::git::text(root, &["worktree", "list", "--porcelain", "-z"])?
            .split("\0\0")
            .any(|entry| worktree_path(entry) == Some(destination)),
    )
}

/// Creates the managed checkout of the repository containing `cwd` for `thread`
/// and records it; returns the one recorded or left by an earlier attempt.
fn checkout(
    path: &Path,
    state: &mut State,
    thread: &str,
    cwd: &Path,
    base: impl FnOnce() -> Result<String>,
    branch: Option<String>,
    cancel: &CancellationToken,
) -> Result<(PathBuf, String)> {
    // Branch names reach Git as arguments; reject what Git would not accept as
    // a branch before any Git command runs.
    if let Some(branch) = branch
        .as_deref()
        .filter(|branch| !crate::git::valid_branch_name(branch))
    {
        return Err(anyhow!("fatal: '{branch}' is not a valid branch name"));
    }
    let cwd = dunce::canonicalize(cwd)?;
    let root =
        dunce::canonicalize(crate::git::text(&cwd, &["rev-parse", "--show-toplevel"])?.trim_end())?;
    let original = match state
        .workspace_roots
        .get(root.to_str().context("project path is not UTF-8")?)
    {
        Some(project) => PathBuf::from(project),
        None => dunce::canonicalize(
            worktree_path(&crate::git::text(
                &root,
                &["worktree", "list", "--porcelain", "-z"],
            )?)
            .context("Git did not return the original repository")?,
        )?,
    };
    let (destination, branch) = match state.threads.get(thread) {
        Some(existing) => (PathBuf::from(&existing.path), existing.branch.clone()),
        None => {
            let parent = checkout_parent(state, &root, &original)?;
            let key = thread_key(thread);
            let destination = parent.join(format!("session-{key}")).join(
                original
                    .file_name()
                    .context("repository has no folder name")?,
            );
            let branch = branch.unwrap_or_else(|| format!("agent/session-{key}"));
            if branch_exists(&root, &branch) {
                return Err(anyhow!("fatal: a branch named '{branch}' already exists"));
            }
            state.threads.insert(
                thread.to_owned(),
                ThreadCheckout {
                    path: destination
                        .to_str()
                        .context("worktree path is not UTF-8")?
                        .to_owned(),
                    branch: branch.clone(),
                },
            );
            save(path, state)?;
            (destination, branch)
        }
    };
    let key = destination
        .to_str()
        .context("worktree path is not UTF-8")?
        .to_owned();
    if registered(&root, &destination)? {
        if state.workspace_roots.contains_key(&key) {
            return Ok((destination, branch));
        }
        // An attempt that stopped before recording it may have left it incomplete.
        crate::git::text(&root, &["worktree", "remove", "--force", "--", &key])?;
    }
    if fs::symlink_metadata(&destination).is_ok() {
        fs::remove_dir_all(&destination)?;
    }
    let session = destination
        .parent()
        .context("worktree has no session folder")?
        .to_path_buf();
    scopeguard::defer! { let _ = fs::remove_dir(&session); }
    // A branch left by an earlier attempt for this thread is reused.
    let created = (|| {
        let base = match branch_exists(&root, &branch) {
            true => None,
            false => Some(base()?),
        };
        create_checkout(
            &root,
            &destination,
            &branch,
            base.as_deref(),
            if state.settings.copy_on_create {
                &state.settings.copy_paths
            } else {
                &[]
            },
            cancel,
        )?;
        Ok::<_, anyhow::Error>(base.is_some())
    })();
    let created_branch = match created {
        Ok(created) => created,
        Err(error) => {
            if !branch_exists(&root, &branch) {
                state.threads.remove(thread);
                let _ = save(path, state);
            }
            return Err(error);
        }
    };
    state
        .workspace_roots
        .insert(key, original.to_string_lossy().into_owned());
    save(path, state).map_err(|error| {
        discard_checkout(
            &root,
            &destination,
            created_branch.then_some(branch.as_str()),
            error,
        )
    })?;
    Ok((destination, branch))
}

/// Where new checkouts of the repository at `root` go.
fn checkout_parent(state: &State, root: &Path, original: &Path) -> Result<PathBuf> {
    let parent = if state.settings.worktree_directory.is_empty() {
        let exclude = PathBuf::from(
            crate::git::text(
                root,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "info/exclude",
                ],
            )?
            .trim_end(),
        );
        let existing = match fs::read_to_string(&exclude) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        if !existing.lines().any(|line| line == "/.worktree/") {
            fs::create_dir_all(exclude.parent().context("Git exclude has no parent")?)?;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exclude)
                .and_then(|mut file| file.write_all(b"\n/.worktree/\n"))?;
        }
        original.join(".worktree")
    } else {
        PathBuf::from(&state.settings.worktree_directory)
    };
    fs::create_dir_all(&parent)?;
    // Resolve aliases such as /tmp before checking copy destination ancestors.
    Ok(dunce::canonicalize(&parent)?)
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct StatusSource {
    pub cwd: String,
    pub checkout: Option<String>,
    pub repository: Option<String>,
    pub branch: Option<String>,
}

/// Inspect visible workspaces without caching Git state. Saved native branch
/// metadata resolves history only after the managed checkout has been deleted.
pub(crate) async fn directory_statuses(
    sources: HashSet<StatusSource>,
) -> Result<HashMap<StatusSource, agent_protocol::models::WorktreeStatus>> {
    use futures_util::{StreamExt, TryStreamExt};
    // Bound process fan-out while avoiding a serial Git round trip for every
    // visible conversation. Recompute on every request so new commits stay fresh.
    let results: Vec<_> = futures_util::stream::iter(sources)
        .map(|source| async move {
            tokio::task::spawn_blocking(move || {
                directory_status(
                    Path::new(&source.cwd),
                    source.checkout.as_deref(),
                    source.repository.as_deref(),
                    source.branch.as_deref(),
                )
                .ok()
                .flatten()
                .map(|status| (source, status))
            })
            .await
        })
        .buffer_unordered(4)
        .try_collect()
        .await?;
    Ok(results.into_iter().flatten().collect())
}

fn directory_status(
    cwd: &Path,
    checkout: Option<&str>,
    repository: Option<&str>,
    saved_branch: Option<&str>,
) -> Result<Option<agent_protocol::models::WorktreeStatus>> {
    match fs::metadata(cwd) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let (Some(checkout), Some(repository), Some(saved_branch)) =
                (checkout, repository, saved_branch)
            else {
                return Ok(None);
            };
            // A missing subdirectory of an existing checkout has no inspectable
            // working tree; do not turn its old metadata into a merge marker.
            match fs::symlink_metadata(checkout) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Ok(None),
            }
            if saved_branch.is_empty() || saved_branch == "main" || saved_branch == "HEAD" {
                return Ok(None);
            }
            let branch = format!("refs/heads/{saved_branch}");
            let repository = Path::new(repository);
            let head = crate::git::text(
                repository,
                &["rev-parse", "--verify", "--end-of-options", &branch],
            )?;
            return branch_status(repository, head.trim(), &branch, false);
        }
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let identity = crate::git::text(
        cwd,
        &[
            "rev-parse",
            "--absolute-git-dir",
            "--path-format=absolute",
            "--git-common-dir",
            "HEAD",
            "--symbolic-full-name",
            "HEAD",
        ],
    )?;
    let mut fields = identity.lines();
    let (Some(git_dir), Some(common_dir), Some(head), Some(branch)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Ok(None);
    };
    if git_dir == common_dir || branch == "refs/heads/main" || !branch.starts_with("refs/heads/") {
        return Ok(None);
    }
    let dirty = !crate::git::output(
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?
    .stdout
    .is_empty();
    branch_status(cwd, head, branch, dirty)
}

fn branch_status(
    repository: &Path,
    head: &str,
    branch: &str,
    dirty: bool,
) -> Result<Option<agent_protocol::models::WorktreeStatus>> {
    let contained = crate::git::text(
        repository,
        &[
            "rev-list",
            "--max-count=1",
            head,
            "--not",
            "refs/heads/main",
        ],
    )?
    .is_empty();
    // Commit ancestry alone also counts empty commits and reverted work. Inspect
    // this branch's net file changes without counting changes made only on main.
    let unmerged_changes = !contained
        && !dirty
        && !crate::git::output(
            repository,
            &[
                "diff",
                "--no-ext-diff",
                "--no-relative",
                "--name-only",
                "-z",
                &format!("refs/heads/main...{branch}"),
                "--",
            ],
        )?
        .stdout
        .is_empty();
    let history = (contained && !dirty)
        .then(|| crate::git::text(repository, &["reflog", "show", "--format=%H", branch]))
        .transpose()?;
    Ok(agent_protocol::models::worktree_branch_status(
        dirty,
        unmerged_changes,
        history
            .as_deref()
            .and_then(|history| history.lines().last())
            .is_some_and(|initial| initial != head),
    ))
}

fn worktree_path(entry: &str) -> Option<&Path> {
    entry
        .split('\0')
        .next()?
        .strip_prefix("worktree ")
        .map(Path::new)
}

fn inspect(path: &str, project: &str) -> Result<(String, Option<String>)> {
    if already_removed(path, project)? {
        return Ok(("削除済み".into(), None));
    }
    let target = dunce::canonicalize(path).context("ワークツリーを確認できません")?;
    let project = dunce::canonicalize(project).context("元のリポジトリを確認できません")?;
    if target != Path::new(path) || target == project {
        return Err(anyhow!("登録されたワークツリーの場所が変わっています。"));
    }
    let listing = crate::git::text(&project, &["worktree", "list", "--porcelain", "-z"])?;
    let entry = listing
        .split("\0\0")
        .find(|entry| worktree_path(entry) == Some(target.as_path()))
        .context("元のリポジトリに登録されたワークツリーではありません。")?;
    let branch = entry
        .split('\0')
        .find_map(|field| field.strip_prefix("branch refs/heads/"));
    let locked = entry
        .split('\0')
        .any(|field| field == "locked" || field.starts_with("locked "));
    let reason = if locked {
        Some("ロックされているため削除できません。".into())
    } else if branch.is_none() {
        Some(
            "ブランチに属さないコミットがあります。ブランチに保存してから削除してください。".into(),
        )
    } else if !crate::git::output(
        &target,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?
    .stdout
    .is_empty()
    {
        Some("未保存の変更、未追跡ファイル、または無視対象のファイルがあります。保存・移動してから削除してください。".into())
    } else {
        None
    };
    Ok((branch.unwrap_or("detached HEAD").into(), reason))
}

// Treat removal as complete only when both the filesystem and Git agree.
fn already_removed(path: &str, project: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listing = crate::git::text(
        Path::new(project),
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    Ok(!listing
        .split("\0\0")
        .any(|entry| worktree_path(entry) == Some(Path::new(path))))
}

#[cfg(test)]
pub(crate) async fn workspace_roots(project_state: &Path) -> Result<HashMap<String, String>> {
    let path = project_state.with_file_name("bex-worktrees.json");
    tokio::task::spawn_blocking(move || read(&path).map(|state| state.workspace_roots)).await?
}

fn read(path: &Path) -> Result<State> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(Into::into),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.into()),
    }
}

fn save(path: &Path, state: &State) -> Result<()> {
    let parent = path.parent().context("settings have no parent directory")?;
    fs::create_dir_all(parent)?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write_with_options(
            |file| {
                serde_json::to_writer(&mut *file, state).map_err(std::io::Error::other)?;
                file.write_all(b"\n")
            },
            crate::platform::private_file_options(),
        )
        .map_err(Into::into)
}

/// Fresh and recreated worktrees share checkout, submodule, copy and rollback
/// rules. Without a `base` the existing `branch` is checked out and kept on
/// failure. A cancelled checkout is removed like a failed one.
fn create_checkout(
    source: &Path,
    destination: &Path,
    branch: &str,
    base: Option<&str>,
    copy_paths: &[String],
    cancel: &CancellationToken,
) -> Result<()> {
    let destination_text = destination.to_str().context("worktree path is not UTF-8")?;
    if let Some(parent) = destination.parent() {
        crate::platform::create_state_directory(parent)?;
    }
    crate::platform::create_state_directory(destination)?;
    if let Some(base) = base
        && let Err(error) = crate::git::text(source, &["branch", "--end-of-options", branch, base])
    {
        let _ = fs::remove_dir(destination);
        return Err(error);
    }
    let created = base.is_some().then_some(branch);
    // Create the branch separately so a locked/missing checkout cannot leak it.
    // One --force replaces a missing registration but continues to respect locks.
    if let Err(error) = crate::git::cancellable(
        source,
        &["worktree", "add", "--force", "--", destination_text, branch],
        cancel,
    ) {
        // A stopped checkout may have registered or written a partial worktree.
        if error.is::<crate::git::Cancelled>() {
            if registered(source, destination)? {
                return Err(discard_checkout(source, destination, created, error));
            }
            let _ = fs::remove_dir_all(destination);
        }
        let _ = fs::remove_dir(destination);
        let Some(branch) = created else {
            return Err(error);
        };
        return Err(
            match crate::git::text(source, &["branch", "-D", "--", branch]) {
                Ok(_) => error,
                Err(cleanup) => error.context(format!("branch cleanup failed: {cleanup:#}")),
            },
        );
    }
    let prepared = (|| {
        update_submodules(destination, cancel)?;
        for entry in copy_paths {
            let relative = relative_path(entry)?;
            let path = source.join(relative);
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            no_symlinks(source, relative)?;
            copy(&path, &destination.join(relative))?;
        }
        if cancel.is_cancelled() {
            return Err(crate::git::Cancelled.into());
        }
        Ok(())
    })();
    prepared.map_err(|error| discard_checkout(source, destination, created, error))
}

/// `git worktree add` leaves submodules empty. Like the reference, a checkout
/// with `.gitmodules` initializes them recursively, best effort: a failure
/// leaves them empty without failing the checkout. Only cancellation fails it.
fn update_submodules(destination: &Path, cancel: &CancellationToken) -> Result<()> {
    if !destination.join(".gitmodules").exists() {
        return Ok(());
    }
    match crate::git::cancellable(
        destination,
        &["submodule", "update", "--init", "--recursive"],
        cancel,
    ) {
        Err(error) if error.is::<crate::git::Cancelled>() => Err(error),
        Err(error) => {
            tracing::warn!(
                path = %destination.display(),
                error = %format!("{error:#}"),
                "worktree submodule checkout failed; submodule paths are empty"
            );
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

/// Removes the checkout and the branch it created, if any.
fn discard_checkout(
    source: &Path,
    destination: &Path,
    branch: Option<&str>,
    error: anyhow::Error,
) -> anyhow::Error {
    let cleanup = (|| -> Result<()> {
        crate::git::text(
            source,
            &[
                "worktree",
                "remove",
                // A checkout stopped mid-way is still locked as initializing.
                "--force",
                "--force",
                "--",
                destination.to_str().context("worktree path is not UTF-8")?,
            ],
        )?;
        if let Some(branch) = branch {
            crate::git::text(source, &["branch", "-D", "--", branch])?;
        }
        Ok(())
    })();
    match cleanup {
        Ok(_) => error,
        Err(cleanup) => error.context(format!("worktree cleanup failed: {cleanup:#}")),
    }
}

fn relative_path(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    if value.is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(name) if name != ".git"))
    {
        return Err(anyhow!(
            "copy paths must be relative paths without '..' or '.git'"
        ));
    }
    Ok(path)
}

fn no_symlinks(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(anyhow!("copy paths cannot contain symbolic links"));
        }
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<()> {
    if target.starts_with(source) {
        return Err(anyhow!("cannot copy a directory into itself"));
    }
    let metadata = fs::symlink_metadata(source)?;
    if let Some(parent) = target.parent() {
        for ancestor in parent.ancestors() {
            if let Ok(meta) = fs::symlink_metadata(ancestor)
                && meta.file_type().is_symlink()
            {
                return Err(anyhow!("copy destination contains a symbolic link"));
            }
        }
    }
    if metadata.is_dir() {
        match fs::symlink_metadata(target) {
            Ok(meta) if !meta.is_dir() => {
                return Err(anyhow!("copy destination is not a directory"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(target)?,
            Err(e) => return Err(e.into()),
        }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            if entry.file_name() == ".git" {
                return Err(anyhow!("cannot copy Git metadata"));
            }
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        let parent = target.parent().context("copy destination has no parent")?;
        fs::create_dir_all(parent)?;
        match fs::symlink_metadata(target) {
            Ok(meta) if !meta.is_file() => {
                return Err(anyhow!("copy destination is not a regular file"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Configured files may replace HEAD's version in this fresh checkout.
        // An atomic replacement also avoids exposing partially copied secrets.
        let mut output = tempfile::NamedTempFile::new_in(parent)?;
        std::io::copy(&mut fs::File::open(source)?, &mut output)?;
        output.as_file().set_permissions(metadata.permissions())?;
        output.persist(target)?;
    } else {
        return Err(anyhow!("only regular files and directories can be copied"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    impl Worktrees {
        async fn configure(&self, update: Option<Value>) -> Result<Value> {
            let update = update.map(serde_json::from_value).transpose()?;
            serde_json::to_value(self.settings(update).await?).map_err(Into::into)
        }
    }

    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[cfg(windows)]
    fn symlink(original: impl AsRef<Path>, link: impl AsRef<Path>) -> std::io::Result<()> {
        if original.as_ref().is_dir() {
            std::os::windows::fs::symlink_dir(original, link)
        } else {
            std::os::windows::fs::symlink_file(original, link)
        }
    }

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        crate::git::text(root, &["init", "--quiet", "--initial-branch=main"]).unwrap();
        crate::git::text(root, &["config", "core.autocrlf", "false"]).unwrap();
        fs::write(root.join("tracked.txt"), "committed\n").unwrap();
        fs::write(root.join("config.txt"), "default\n").unwrap();
        fs::write(root.join(".gitignore"), ".env\nlocal/\n").unwrap();
        crate::git::text(root, &["add", "."]).unwrap();
        crate::git::text(
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
        directory
    }

    fn new_thread() -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        format!(
            "thread:{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    fn commit(cwd: &Path, file: &str, contents: &str) -> String {
        fs::write(cwd.join(file), contents).unwrap();
        crate::git::text(cwd, &["add", file]).unwrap();
        crate::git::text(
            cwd,
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
                contents,
            ],
        )
        .unwrap();
        crate::git::text(cwd, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }

    fn head(cwd: &Path) -> String {
        crate::git::text(cwd, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_owned()
    }

    #[tokio::test]
    async fn a_thread_keeps_its_checkout_when_its_creation_is_retried() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Worktrees::new(&projects);
        let create = |thread: &'static str| {
            store.create(
                thread,
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
        };
        let first = create("thread:retried").await.unwrap();
        assert_eq!(create("thread:retried").await.unwrap(), first);
        let other = create("thread:other").await.unwrap();
        assert_ne!(other.0, first.0);
        assert_ne!(other.1, first.1);
        let listing = || {
            (
                crate::git::text(&root, &["worktree", "list", "--porcelain"]).unwrap(),
                crate::git::text(&root, &["branch", "--list"]).unwrap(),
            )
        };
        let listed = listing();
        let state = projects.with_file_name("bex-worktrees.json");
        let forget = || {
            let mut saved = read(&state).unwrap();
            saved.workspace_roots.remove(first.0.to_str().unwrap());
            save(&state, &saved).unwrap();
        };

        // Stopped after the checkout, before recording it.
        forget();
        assert_eq!(create("thread:retried").await.unwrap(), first);
        assert_eq!(listing(), listed);

        // Stopped after creating the branch, before the checkout.
        crate::git::text(
            &root,
            &["worktree", "remove", "--force", first.0.to_str().unwrap()],
        )
        .unwrap();
        forget();
        assert_eq!(create("thread:retried").await.unwrap(), first);
        assert_eq!(listing(), listed);
        assert!(
            workspace_roots(&projects)
                .await
                .unwrap()
                .contains_key(first.0.to_str().unwrap())
        );
    }

    fn commit_file(root: &Path, file: &str, contents: &str) {
        if let Some(parent) = Path::new(file).parent() {
            fs::create_dir_all(root.join(parent)).unwrap();
        }
        commit(root, file, contents);
    }

    fn registered_paths(root: &Path) -> Vec<String> {
        crate::git::text(root, &["worktree", "list", "--porcelain"])
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix("worktree ").map(str::to_owned))
            .collect()
    }

    // A project below the repository root records the checkout's root, which the
    // reference binds as the thread's worktree, so giving the launch up removes
    // it even after its setup changed tracked and untracked files. The branch
    // stays, as the reference's forced removal keeps it.
    #[tokio::test]
    async fn a_subproject_checkout_is_recorded_and_abandoned_by_its_root() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        commit_file(&root, "packages/app/source.txt", "app");
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Worktrees::new(&projects);
        let (checkout, branch) = store
            .create(
                &new_thread(),
                root.join("packages/app").to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(checkout.join("packages/app/source.txt").is_file());
        assert!(
            workspace_roots(&projects)
                .await
                .unwrap()
                .contains_key(checkout.to_str().unwrap())
        );
        fs::write(checkout.join("tracked.txt"), "changed by setup\n").unwrap();
        fs::create_dir_all(checkout.join("packages/app/node_modules/dependency")).unwrap();
        fs::write(
            checkout.join("packages/app/node_modules/dependency/index.js"),
            "",
        )
        .unwrap();

        store
            .abandon(checkout.to_str().unwrap().to_owned())
            .await
            .unwrap();

        assert!(!checkout.exists());
        assert!(!checkout.parent().unwrap().exists());
        assert_eq!(registered_paths(&root), [root.to_str().unwrap()]);
        assert!(branch_exists(&root, &branch));
    }

    // GitVcsDriverCore renameBranch: a taken name gets the first free numeric
    // suffix unless the exact name is required; the thread's retry finds its
    // checkout under the new name.
    #[tokio::test]
    async fn renaming_a_launch_branch_picks_a_free_name_and_keeps_the_checkout() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        crate::git::text(&root, &["branch", "feature/search"]).unwrap();
        crate::git::text(&root, &["branch", "feature/search-1"]).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        let thread = new_thread();
        let create = || {
            store.create(
                &thread,
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
        };
        let (checkout, temporary) = create().await.unwrap();
        let cwd = checkout.to_str().unwrap().to_owned();
        let rename = |old: &str, new: &str, exact: bool| {
            store.rename_branch(cwd.clone(), old.into(), new.into(), exact)
        };

        let exact = rename(&temporary, "feature/search", true)
            .await
            .unwrap_err();
        assert!(format!("{exact:#}").contains("already exists"), "{exact:#}");
        let option = rename(&temporary, "-D", true).await.unwrap_err();
        assert!(
            format!("{option:#}").contains("not a valid branch name"),
            "{option:#}"
        );
        let renamed = rename(&temporary, "feature/search", false).await.unwrap();
        assert_eq!(renamed, "feature/search-2");
        assert_eq!(
            crate::git::text(&checkout, &["branch", "--show-current"])
                .unwrap()
                .trim(),
            renamed
        );
        assert!(branch_exists(&root, "feature/search"));
        assert!(!branch_exists(&root, &temporary));
        assert_eq!(rename(&renamed, &renamed, false).await.unwrap(), renamed);
        assert_eq!(create().await.unwrap(), (checkout, renamed));
    }

    /// Makes `git worktree add` in `root` wait in its post-checkout hook, which
    /// writes its process ID to the returned file first.
    #[cfg(unix)]
    fn slow_checkout_hook(root: &Path, hooks: &Path) -> PathBuf {
        let marker = hooks.join("pid");
        let hook = hooks.join("post-checkout");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho $$ > '{}.tmp'\nmv '{0}.tmp' '{0}'\nexec sleep 30\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        crate::git::text(root, &["config", "core.hooksPath", hooks.to_str().unwrap()]).unwrap();
        marker
    }

    #[cfg(unix)]
    async fn hook_pid(marker: &Path) -> String {
        for _ in 0..1000 {
            if let Ok(pid) = fs::read_to_string(marker) {
                return pid.trim().to_owned();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the checkout never reached its hook");
    }

    #[cfg(unix)]
    async fn exited(pid: &str) -> bool {
        for _ in 0..200 {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            if !alive {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        false
    }

    // Cancelling a checkout stops Git and every process it started, and removes
    // what the checkout created before the call returns.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_checkout_stops_git_and_leaves_nothing_behind() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let hooks = tempfile::tempdir().unwrap();
        let marker = slow_checkout_hook(&root, hooks.path());
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Arc::new(Worktrees::new(&projects));
        let cancel = CancellationToken::new();
        let creating = tokio::spawn({
            let (store, root, cancel) = (store.clone(), root.clone(), cancel.clone());
            async move {
                store
                    .create(
                        &new_thread(),
                        root.to_str().unwrap(),
                        "HEAD",
                        None,
                        false,
                        Default::default(),
                        cancel,
                    )
                    .await
            }
        });
        let pid = hook_pid(&marker).await;
        let started = std::time::Instant::now();
        cancel.cancel();
        let error = creating.await.unwrap().unwrap_err();
        assert!(error.is::<crate::git::Cancelled>(), "{error:#}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert!(exited(&pid).await, "the hook Git started still runs");
        assert_eq!(registered_paths(&root), [root.to_str().unwrap()]);
        assert_eq!(
            crate::git::text(&root, &["branch", "--format=%(refname:short)"]).unwrap(),
            "main\n"
        );
        assert!(workspace_roots(&projects).await.unwrap().is_empty());
        assert!(
            fs::read_dir(root.join(".worktree"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    // A caller that stops waiting stops the checkout too, and the state stays
    // locked until it has: a later save is never overwritten by the stale state
    // the stopped checkout read.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_abandoned_checkout_stops_before_the_state_is_released() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let hooks = tempfile::tempdir().unwrap();
        let marker = slow_checkout_hook(&root, hooks.path());
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Arc::new(Worktrees::new(&projects));
        let creating = tokio::spawn({
            let (store, root) = (store.clone(), root.clone());
            async move {
                store
                    .create(
                        &new_thread(),
                        root.to_str().unwrap(),
                        "HEAD",
                        None,
                        false,
                        Default::default(),
                        Default::default(),
                    )
                    .await
            }
        });
        let pid = hook_pid(&marker).await;
        creating.abort();
        let settings = WorktreeSettings {
            copy_on_create: true,
            copy_paths: vec![".env".into()],
            ..Default::default()
        };
        store.settings(Some(settings.clone())).await.unwrap();
        assert!(exited(&pid).await, "the hook Git started still runs");
        assert_eq!(registered_paths(&root), [root.to_str().unwrap()]);
        let saved = read(&projects.with_file_name("bex-worktrees.json")).unwrap();
        assert_eq!(saved.settings, settings);
        assert!(saved.workspace_roots.is_empty());
        assert!(saved.threads.is_empty());
    }

    /// Serves the files under `base` over HTTP, enough for Git's dumb protocol;
    /// returns the base URL.
    fn serve_dumb_http(base: PathBuf) -> String {
        use std::io::{BufRead as _, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let base = base.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    let _ = reader.read_line(&mut request);
                    let mut header = String::new();
                    while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                        header.clear();
                    }
                    let path = request
                        .split_whitespace()
                        .nth(1)
                        .and_then(|target| target.split('?').next())
                        .unwrap_or("/");
                    let body = (!path.contains(".."))
                        .then(|| fs::read(base.join(path.trim_start_matches('/'))).ok())
                        .flatten();
                    let status = if body.is_some() {
                        "200 OK"
                    } else {
                        "404 Not Found"
                    };
                    let body = body.unwrap_or_default();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(&body);
                });
            }
        });
        url
    }

    // GitVcsDriverCore createWorktree: a new checkout initializes its submodules
    // recursively; a submodule that cannot be fetched stays empty without
    // failing the checkout.
    #[tokio::test]
    async fn new_checkouts_initialize_submodules_recursively_best_effort() {
        // Submodule clones refuse the file transport by default, so the
        // submodules are served over Git's dumb HTTP protocol.
        let served = tempfile::tempdir().unwrap();
        let url = serve_dumb_http(served.path().to_owned());
        let publish = |repository: &Path, name: &str| {
            let bare = served.path().join(name);
            crate::git::text(
                repository,
                &[
                    "clone",
                    "--quiet",
                    "--bare",
                    "--",
                    repository.to_str().unwrap(),
                    bare.to_str().unwrap(),
                ],
            )
            .unwrap();
            crate::git::text(&bare, &["update-server-info"]).unwrap();
            format!("{url}/{name}")
        };
        let add = |repository: &Path, url: &str, name: &str| {
            crate::git::text(
                repository,
                &["submodule", "add", "--quiet", "--", url, name],
            )
            .unwrap();
            commit(
                repository,
                ".gitmodules",
                &fs::read_to_string(repository.join(".gitmodules")).unwrap(),
            );
        };
        let inner = repository();
        let middle = repository();
        let outer = repository();
        add(middle.path(), &publish(inner.path(), "inner.git"), "inner");
        add(outer.path(), &publish(middle.path(), "middle.git"), "lib");
        let root = dunce::canonicalize(outer.path()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        let (checkout, _) = store
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(checkout.join("lib/tracked.txt").is_file());
        assert!(checkout.join("lib/inner/tracked.txt").is_file());

        // A submodule whose source does not exist stays empty; the checkout stays.
        let broken = repository();
        let root = dunce::canonicalize(broken.path()).unwrap();
        let missing = directory.path().join("missing");
        fs::write(
            root.join(".gitmodules"),
            format!(
                "[submodule \"gone\"]\n\tpath = gone\n\turl = {}\n",
                missing.display()
            ),
        )
        .unwrap();
        crate::git::text(
            &root,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},gone", head(inner.path())),
            ],
        )
        .unwrap();
        commit(
            &root,
            ".gitmodules",
            &fs::read_to_string(root.join(".gitmodules")).unwrap(),
        );
        let (checkout, _) = store
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap();
        assert!(checkout.join("tracked.txt").is_file());
        assert!(!checkout.join("gone/tracked.txt").exists());
    }

    // A branch or base named like an option is never read as one: neither
    // deletes, forces or otherwise changes an existing branch.
    #[tokio::test]
    async fn option_shaped_branch_and_base_names_change_no_branch() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        crate::git::text(&root, &["switch", "--quiet", "-c", "valuable-topic"]).unwrap();
        let unmerged = commit(&root, "topic.txt", "unmerged work");
        crate::git::text(&root, &["switch", "--quiet", "main"]).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Worktrees::new(&projects);
        let branches = || crate::git::text(&root, &["branch", "--list"]).unwrap();
        let before = branches();
        for (branch, base) in [
            (Some("-D"), "valuable-topic"),
            (Some("--force"), "valuable-topic"),
            (Some("-m"), "valuable-topic"),
            (Some("HEAD"), "main"),
            (Some("a..b"), "main"),
            (Some("ok-name"), "-D"),
            (None, "--orphan"),
        ] {
            let error = store
                .create(
                    &new_thread(),
                    root.to_str().unwrap(),
                    base,
                    branch.map(str::to_owned),
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .unwrap_err();
            if let Some(branch) = branch.filter(|branch| !crate::git::valid_branch_name(branch)) {
                assert_eq!(
                    error.to_string(),
                    format!("fatal: '{branch}' is not a valid branch name")
                );
            }
            assert_eq!(branches(), before, "{branch:?} from {base}");
            assert_eq!(
                crate::git::text(&root, &["rev-parse", "valuable-topic"])
                    .unwrap()
                    .trim(),
                unmerged
            );
        }
        assert!(workspace_roots(&projects).await.unwrap().is_empty());
    }

    // "Start from origin" fetches only when the repository has an origin, and
    // starts from the local base when origin lacks the branch.
    #[tokio::test]
    async fn starting_from_origin_falls_back_to_the_local_base() {
        let upstream = repository();
        let directory = tempfile::tempdir().unwrap();
        let clone = directory.path().join("clone");
        crate::git::text(
            directory.path(),
            &[
                "clone",
                "--quiet",
                upstream.path().to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        )
        .unwrap();
        let clone = dunce::canonicalize(clone).unwrap();
        let upstream_head = commit(upstream.path(), "tracked.txt", "upstream");
        let local_head = commit(&clone, "local.txt", "local");
        crate::git::text(&clone, &["branch", "feature"]).unwrap();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        let create = |thread: &'static str, base: &'static str| {
            store.create(
                thread,
                clone.to_str().unwrap(),
                base,
                None,
                true,
                Default::default(),
                Default::default(),
            )
        };

        let (from_origin, _) = create("thread:origin", "main").await.unwrap();
        assert_eq!(head(&from_origin), upstream_head);
        let (missing_remote_branch, _) = create("thread:local", "feature").await.unwrap();
        assert_eq!(head(&missing_remote_branch), local_head);
        crate::git::text(&clone, &["remote", "remove", "origin"]).unwrap();
        let (without_origin, _) = create("thread:no-origin", "main").await.unwrap();
        assert_eq!(head(&without_origin), local_head);
    }

    // ThreadLaunchService.test.ts "shows the fetch diagnosis when preparing a worktree
    // from origin fails": nothing is checked out and Git's output stays out.
    #[tokio::test]
    async fn a_failed_origin_fetch_reports_its_diagnosis_and_checks_out_nothing() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        crate::git::text(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "/missing/secret-token/repository",
            ],
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects.json");
        let store = Worktrees::new(&projects);
        let branches = crate::git::text(&root, &["branch", "--list"]).unwrap();
        let error = store
            .create(
                "thread:fetch",
                root.to_str().unwrap(),
                "main",
                None,
                true,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            format!(
                "Git command failed in GitVcsDriver.fetchRemote ({}): Git could not access the remote repository. Check the remote URL and repository permissions on the server.",
                root.display()
            )
        );
        assert_eq!(
            crate::git::text(&root, &["branch", "--list"]).unwrap(),
            branches
        );
        assert_eq!(fs::read_dir(root.join(".worktree")).unwrap().count(), 0);
        assert!(workspace_roots(&projects).await.unwrap().is_empty());
    }

    // GitVcsDriverCore.test.ts "reports $name during fetch without retaining remote output"
    #[test]
    fn fetch_failures_report_a_fixed_diagnosis() {
        let secret = "secret-fetch-token";
        for (stderr, expected) in [
            ("fatal: Authentication failed for", "could not authenticate"),
            (
                "git@example.com: Permission denied (publickey).",
                "could not authenticate",
            ),
            (
                "fatal: could not read Username: terminal prompts disabled",
                "could not authenticate",
            ),
            (
                "fatal: Could not resolve host: example.com",
                "could not reach the remote",
            ),
            (
                "ssh: connect to host example.com port 22: Connection refused",
                "could not reach the remote",
            ),
            (
                "remote: Repository not found.",
                "could not access the remote repository",
            ),
            (
                "fatal: remote does not appear to be a git repository",
                "could not access the remote repository",
            ),
            (
                "error: cannot lock ref 'refs/remotes/origin/main': is at abc but expected def",
                "could not update a local reference",
            ),
            (
                "fatal: Unable to create '/repo/.git/FETCH_HEAD.lock': File exists.",
                "could not update a local reference",
            ),
            (
                "remote: Help: authentication failed, connection refused, cannot lock ref\nremote: unrelated service error",
                "git fetch origin failed",
            ),
            (
                "fatal: unable to access 'https://example.com/repo.git/': Could not resolve host: example.com",
                "could not reach the remote",
            ),
            (
                "fatal: unexpected remote failure",
                "git fetch origin failed",
            ),
        ] {
            let stderr =
                format!("{stderr}\nhttps://user:{secret}@example.com/private?token={secret}");
            let detail = fetch_failure_detail(&stderr).unwrap_or("git fetch origin failed");
            assert!(detail.contains(expected), "{stderr}: {detail}");
            assert!(!detail.contains(secret));
        }
    }

    #[tokio::test]
    async fn deleted_worktrees_are_recreated_on_send_even_when_creation_is_disabled() {
        for removal in ["managed", "git", "filesystem"] {
            let repository = repository();
            let root = dunce::canonicalize(repository.path()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let projects = directory.path().join("projects.json");
            let store = Worktrees::new(&projects);
            store
                .configure(Some(json!({"createOnNewSession":true})))
                .await
                .unwrap();
            let cwd = store
                .create(
                    &new_thread(),
                    root.to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .unwrap()
                .0;
            fs::write(cwd.join("tracked.txt"), "worktree commit").unwrap();
            crate::git::text(&cwd, &["add", "tracked.txt"]).unwrap();
            crate::git::text(
                &cwd,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-m",
                    "worktree change",
                ],
            )
            .unwrap();
            let branch = crate::git::text(&cwd, &["branch", "--show-current"]).unwrap();
            let head = crate::git::text(&cwd, &["rev-parse", "HEAD"]).unwrap();
            match removal {
                "managed" => store
                    .remove(cwd.to_str().unwrap().into(), false)
                    .await
                    .unwrap(),
                "git" => {
                    crate::git::text(&root, &["worktree", "remove", cwd.to_str().unwrap()])
                        .unwrap();
                }
                _ => fs::remove_dir_all(&cwd).unwrap(),
            }
            crate::git::text(
                &root,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "advance main",
                ],
            )
            .unwrap();
            let branches = crate::git::text(
                &root,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads/",
                ],
            )
            .unwrap();
            if removal == "filesystem" {
                crate::git::text(&root, &["worktree", "lock", cwd.to_str().unwrap()]).unwrap();
                assert!(store.ensure_available(cwd.to_str().unwrap()).await.is_err());
                assert!(!cwd.exists());
                crate::git::text(&root, &["worktree", "unlock", cwd.to_str().unwrap()]).unwrap();
            }
            // Failed copying must roll back the new checkout and branch before retrying.
            symlink(root.join("tracked.txt"), root.join("unsafe-copy")).unwrap();
            store
                .configure(Some(
                    json!({"copyOnCreate":true,"copyPaths":["unsafe-copy"]}),
                ))
                .await
                .unwrap();
            assert!(store.ensure_available(cwd.to_str().unwrap()).await.is_err());
            assert!(!cwd.exists());
            assert_eq!(
                crate::git::text(
                    &root,
                    &[
                        "for-each-ref",
                        "--format=%(refname) %(objectname)",
                        "refs/heads/"
                    ]
                )
                .unwrap(),
                branches
            );
            fs::write(root.join(".env"), "local configuration").unwrap();
            store
                .configure(Some(
                    json!({"createOnNewSession":false,"copyOnCreate":true,"copyPaths":[".env"]}),
                ))
                .await
                .unwrap();
            let restarted = Worktrees::new(&projects);
            let (first, second) = tokio::join!(
                restarted.ensure_available(cwd.to_str().unwrap()),
                restarted.ensure_available(cwd.to_str().unwrap()),
            );
            let recreated: Vec<_> = [first.unwrap(), second.unwrap()]
                .into_iter()
                .flatten()
                .collect();
            assert_eq!(recreated, std::slice::from_ref(&cwd));
            assert_ne!(
                crate::git::text(&cwd, &["branch", "--show-current"]).unwrap(),
                branch
            );
            assert_eq!(
                crate::git::text(&cwd, &["rev-parse", "HEAD"]).unwrap(),
                crate::git::text(&root, &["rev-parse", "main"]).unwrap()
            );
            assert_eq!(
                crate::git::text(&root, &["rev-parse", branch.trim()]).unwrap(),
                head
            );
            assert_eq!(
                fs::read_to_string(cwd.join(".env")).unwrap(),
                "local configuration"
            );
            assert_eq!(restarted.list().await.unwrap().len(), 1);
            for missing in [cwd.join("missing"), root.join("unknown")] {
                assert!(
                    restarted
                        .ensure_available(missing.to_str().unwrap())
                        .await
                        .is_err()
                );
            }
            fs::write(cwd.join("keep.txt"), "uncommitted").unwrap();
            restarted
                .ensure_available(cwd.to_str().unwrap())
                .await
                .unwrap();
            assert_eq!(
                fs::read_to_string(cwd.join("keep.txt")).unwrap(),
                "uncommitted"
            );
        }
    }

    #[tokio::test]
    async fn invalid_saved_settings_keep_the_json_error() {
        let directory = tempfile::tempdir().unwrap();
        let worktrees = Worktrees::new(&directory.path().join("projects.json"));
        fs::write(&worktrees.path, "invalid json").unwrap();
        let error = worktrees.list().await.unwrap_err();
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
    }

    #[tokio::test]
    async fn automatic_removal_rechecks_merge_and_opt_in_before_removing_checkout() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let store = Worktrees::new(&root.join("projects.json"));
        let mut settings = WorktreeSettings {
            create_on_new_session: true,
            delete_merged: true,
            ..Default::default()
        };
        store.settings(Some(settings.clone())).await.unwrap();
        let cwd = store
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        let target = cwd.to_str().unwrap().to_owned();
        let branch = crate::git::text(&cwd, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        let commit = |message: &str| {
            crate::git::text(
                &cwd,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    message,
                ],
            )
            .unwrap()
        };
        let merge = || crate::git::text(&root, &["merge", "--ff-only", &branch]).unwrap();
        store.remove(target.clone(), true).await.unwrap();
        assert!(cwd.is_dir(), "a fresh branch is not completed work");
        commit("work");
        store.remove(target.clone(), true).await.unwrap();
        assert!(cwd.is_dir(), "unmerged work must stay");
        merge();
        settings.delete_merged = false;
        store.settings(Some(settings.clone())).await.unwrap();
        store.remove(target.clone(), true).await.unwrap();
        assert!(
            cwd.is_dir(),
            "disabling cleanup cancels an earlier eligibility decision"
        );
        settings.delete_merged = true;
        store.settings(Some(settings)).await.unwrap();
        commit("after merge");
        store.remove(target.clone(), true).await.unwrap();
        assert!(cwd.is_dir(), "a commit after merge cancels removal");
        merge();
        let head = crate::git::text(&cwd, &["rev-parse", "HEAD"]).unwrap();
        store.remove(target, true).await.unwrap();
        assert!(!cwd.exists());
        assert_eq!(
            crate::git::text(&root, &["rev-parse", &branch]).unwrap(),
            head
        );
    }

    #[tokio::test]
    async fn managed_removal_rechecks_changes_locks_and_ownership_and_preserves_commits() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let state = root.join("projects.json");
        let worktrees = Worktrees::new(&state);
        worktrees
            .settings(Some(WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let first = worktrees
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        let other = worktrees
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        let first_path = first.to_str().unwrap().to_owned();
        let branch = crate::git::text(&first, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        let head = crate::git::text(&first, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(Worktrees::new(&state).list().await.unwrap().len(), 2);
        assert!(
            worktrees
                .remove(root.to_str().unwrap().into(), false)
                .await
                .is_err()
        );
        for file in ["tracked.txt", "new.txt", ".env"] {
            let previous = fs::read(first.join(file)).ok();
            fs::write(first.join(file), "preserve this local data\n").unwrap();
            assert!(
                worktrees
                    .list()
                    .await
                    .unwrap()
                    .iter()
                    .find(|tree| tree.path == first_path)
                    .unwrap()
                    .blocked_reason
                    .is_some()
            );
            assert!(worktrees.remove(first_path.clone(), false).await.is_err());
            assert_eq!(
                fs::read_to_string(first.join(file)).unwrap(),
                "preserve this local data\n"
            );
            if let Some(previous) = previous {
                fs::write(first.join(file), previous).unwrap();
            } else {
                fs::remove_file(first.join(file)).unwrap();
            }
        }
        crate::git::text(&root, &["worktree", "lock", &first_path]).unwrap();
        assert!(worktrees.remove(first_path.clone(), false).await.is_err());
        crate::git::text(&root, &["worktree", "unlock", &first_path]).unwrap();
        crate::git::text(&first, &["checkout", "--detach", "HEAD"]).unwrap();
        assert!(worktrees.remove(first_path.clone(), false).await.is_err());
        crate::git::text(&first, &["checkout", &branch]).unwrap();
        worktrees.remove(first_path.clone(), false).await.unwrap();
        assert!(!first.exists());
        assert!(!first.parent().unwrap().exists());
        assert!(other.join("tracked.txt").exists());
        assert_eq!(
            crate::git::text(&root, &["rev-parse", &branch]).unwrap(),
            head
        );
        let restarted = Worktrees::new(&state);
        let entries = restarted.list().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .find(|entry| entry.path == first_path)
                .unwrap()
                .branch,
            "削除済み"
        );
        assert!(
            workspace_roots(&state)
                .await
                .unwrap()
                .contains_key(&first_path)
        );
        // External removal also keeps the conversation-to-project mapping.
        let other_path = other.to_str().unwrap();
        crate::git::text(&root, &["worktree", "remove", "--", other_path]).unwrap();
        assert!(restarted.list().await.unwrap()[0].blocked_reason.is_none());
        let unrelated = other.parent().unwrap().join("keep.txt");
        fs::write(&unrelated, "preserve").unwrap();
        restarted.remove(other_path.into(), false).await.unwrap();
        assert_eq!(Worktrees::new(&state).list().await.unwrap().len(), 2);
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "preserve");
    }

    #[tokio::test]
    async fn settings_persist_and_toggles_control_real_worktree_creation_and_copying() {
        let directory = repository();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let projects = root.join("projects.json");
        let store = Worktrees::new(&projects);
        fs::write(root.join("tracked.txt"), "uncommitted\n").unwrap();
        fs::write(root.join("config.txt"), "local config\n").unwrap();
        fs::write(root.join(".env"), "FIXTURE_TOKEN=isolated\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(root.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(root.join("local")).unwrap();
        fs::write(root.join("local/value"), "local data").unwrap();
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env","local","missing","config.txt"],"worktreeDirectory":"","deleteMerged":false});
        store.configure(Some(settings.clone())).await.unwrap();
        let loaded = Worktrees::new(&projects);
        assert_eq!(loaded.configure(None).await.unwrap(), settings);
        let first = loaded
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        assert_eq!(first.file_name(), root.file_name());
        assert_eq!(
            first.parent().unwrap().parent().unwrap(),
            root.join(".worktree")
        );
        assert_eq!(
            fs::read_to_string(first.join("tracked.txt")).unwrap(),
            "committed\n"
        );
        assert_eq!(
            fs::read_to_string(first.join("config.txt")).unwrap(),
            "local config\n"
        );
        assert_eq!(
            fs::read(first.join(".env")).unwrap(),
            fs::read(root.join(".env")).unwrap()
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(first.join(".env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::read_to_string(first.join("local/value")).unwrap(),
            "local data"
        );
        assert!(!first.join("missing").exists());
        assert!(
            crate::git::text(&first, &["branch", "--show-current"])
                .unwrap()
                .starts_with("agent/session-")
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            workspace_roots(&projects).await.unwrap()[first.to_str().unwrap()],
            root.to_str().unwrap()
        );
        store
            .configure(Some(
                json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"]}),
            ))
            .await
            .unwrap();
        let second = store
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        assert_ne!(first, second);
        assert!(!second.join(".env").exists());
        store
            .configure(Some(
                json!({"createOnNewSession":false,"copyOnCreate":true,"copyPaths":[".env"]}),
            ))
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("tracked.txt")).unwrap(),
            "uncommitted\n"
        );
    }

    #[tokio::test]
    async fn failed_copy_removes_only_the_new_worktree_and_branch() {
        let directory = repository();
        let root = directory.path();
        let store = Worktrees::new(&root.join("projects.json"));
        let before = crate::git::text(root, &["worktree", "list", "--porcelain"]).unwrap();
        let branches = crate::git::text(root, &["branch", "--list"]).unwrap();
        for path in ["link", "nested/value", ".worktree"] {
            if path == "link" {
                symlink(root.join("tracked.txt"), root.join("link")).unwrap();
            }
            if path == "nested/value" {
                fs::create_dir(root.join("local")).unwrap();
                fs::write(root.join("local/value"), "fixture").unwrap();
                symlink(root.join("local"), root.join("nested")).unwrap();
            }
            store
                .configure(Some(
                    json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[path]}),
                ))
                .await
                .unwrap();
            assert!(
                store
                    .create(
                        &new_thread(),
                        root.to_str().unwrap(),
                        "HEAD",
                        None,
                        false,
                        Default::default(),
                        Default::default(),
                    )
                    .await
                    .is_err(),
                "{path} must be rejected"
            );
            assert_eq!(
                crate::git::text(root, &["worktree", "list", "--porcelain"]).unwrap(),
                before
            );
            assert_eq!(
                crate::git::text(root, &["branch", "--list"]).unwrap(),
                branches
            );
            assert_eq!(fs::read_dir(root.join(".worktree")).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn invalid_settings_never_replace_saved_preferences() {
        let directory = repository();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        let valid = json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"],"worktreeDirectory":"","deleteMerged":false});
        store.configure(Some(valid.clone())).await.unwrap();
        for path in [
            "",
            "/tmp/private",
            "../secret",
            ".git",
            "a/../../secret",
            "a/.git/config",
        ] {
            assert!(
                store
                    .configure(Some(json!({"copyPaths":[path]})))
                    .await
                    .is_err()
            );
            assert_eq!(store.configure(None).await.unwrap(), valid);
        }
        assert!(
            store
                .configure(Some(json!({"createOnNewSession":"yes"})))
                .await
                .is_err()
        );
        for path in ["relative/worktrees", "~/worktrees", "../worktrees"] {
            assert!(
                store
                    .configure(Some(json!({"worktreeDirectory":path})))
                    .await
                    .is_err()
            );
            assert_eq!(store.configure(None).await.unwrap(), valid);
        }
    }

    #[tokio::test]
    async fn configured_directory_supports_symlinked_parents_and_preserves_existing_worktrees() {
        let directory = repository();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let storage = tempfile::tempdir().unwrap();
        let real_parent = dunce::canonicalize(storage.path()).unwrap();
        symlink(&real_parent, root.join("storage")).unwrap();
        fs::write(root.join(".env"), "FIXTURE_VALUE=isolated\n").unwrap();
        let projects = root.join("projects.json");
        let store = Worktrees::new(&projects);
        let mut settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env"],"worktreeDirectory":root.join("storage/new folder")});
        store.configure(Some(settings.clone())).await.unwrap();
        let restarted = Worktrees::new(&projects);
        let first = restarted
            .create(
                &new_thread(),
                root.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        assert_eq!(first.file_name(), root.file_name());
        assert_eq!(
            first.parent().unwrap().parent().unwrap(),
            real_parent.join("new folder")
        );
        assert_eq!(
            fs::read(first.join(".env")).unwrap(),
            fs::read(root.join(".env")).unwrap()
        );
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        settings["worktreeDirectory"] = json!(real_parent.join("second"));
        store.configure(Some(settings)).await.unwrap();
        let second = restarted
            .create(
                &new_thread(),
                first.to_str().unwrap(),
                "HEAD",
                None,
                false,
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap()
            .0;
        assert_eq!(second.file_name(), root.file_name());
        assert_eq!(
            second.parent().unwrap().parent().unwrap(),
            real_parent.join("second")
        );
        assert!(first.join(".env").is_file());
        let roots = workspace_roots(&projects).await.unwrap();
        assert_eq!(roots[first.to_str().unwrap()], root.to_str().unwrap());
        assert_eq!(roots[second.to_str().unwrap()], root.to_str().unwrap());
    }

    #[tokio::test]
    async fn existing_checkouts_keep_original_name_and_location_and_return_the_checkout_root() {
        for registered in [false, true] {
            let repository = repository();
            let root = dunce::canonicalize(repository.path()).unwrap();
            let state_directory = tempfile::tempdir().unwrap();
            let projects = state_directory.path().join("projects.json");
            let legacy = root.join(".git/bex-worktrees/session-legacy");
            crate::git::text(
                &root,
                &["worktree", "add", "-b", "legacy", legacy.to_str().unwrap()],
            )
            .unwrap();
            fs::create_dir_all(legacy.join("packages/app")).unwrap();
            fs::write(
                legacy.join("packages/app/source.txt"),
                "worktree-only commit",
            )
            .unwrap();
            crate::git::text(&legacy, &["add", "packages"]).unwrap();
            crate::git::text(
                &legacy,
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
                    "worktree-only",
                ],
            )
            .unwrap();
            let exclude = root.join(".git/info/exclude");
            fs::write(
                &exclude,
                "# preserve existing settings without a final newline",
            )
            .unwrap();
            let mut state = State::default();
            state.settings.create_on_new_session = true;
            if registered {
                state.workspace_roots.insert(
                    legacy.to_str().unwrap().into(),
                    root.to_str().unwrap().into(),
                );
            }
            save(&projects.with_file_name("bex-worktrees.json"), &state).unwrap();
            let store = Worktrees::new(&projects);
            let first = store
                .create(
                    &new_thread(),
                    legacy.join("packages/app").to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .unwrap()
                .0;
            // Like the reference, a project below the repository root works in
            // the root of its checkout, which is also what the thread records.
            let checkout = first.as_path();
            assert!(checkout.join("packages/app").is_dir());
            assert_eq!(checkout.file_name(), root.file_name());
            assert_eq!(
                checkout.parent().unwrap().parent().unwrap(),
                root.join(".worktree")
            );
            assert_eq!(
                fs::read_to_string(first.join("packages/app/source.txt")).unwrap(),
                "worktree-only commit"
            );
            assert_eq!(
                crate::git::text(checkout, &["rev-parse", "HEAD"]).unwrap(),
                crate::git::text(&legacy, &["rev-parse", "HEAD"]).unwrap()
            );
            assert!(!root.join("packages/app/source.txt").exists());
            let second = Worktrees::new(&projects)
                .create(
                    &new_thread(),
                    first.join("packages/app").to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .unwrap()
                .0;
            let second_checkout = second.as_path();
            assert_eq!(second_checkout.file_name(), root.file_name());
            assert_eq!(
                second_checkout.parent().unwrap().parent().unwrap(),
                root.join(".worktree")
            );
            assert!(!checkout.join(".worktree").exists());
            let roots = workspace_roots(&projects).await.unwrap();
            assert_eq!(roots[checkout.to_str().unwrap()], root.to_str().unwrap());
            assert_eq!(
                roots[second_checkout.to_str().unwrap()],
                root.to_str().unwrap()
            );
            assert_eq!(
                fs::read_to_string(&exclude).unwrap(),
                "# preserve existing settings without a final newline\n/.worktree/\n"
            );
            assert!(
                crate::git::text(&root, &["status", "--porcelain", "--untracked-files=all"])
                    .unwrap()
                    .is_empty()
            );
            if registered {
                store
                    .remove(legacy.to_str().unwrap().into(), false)
                    .await
                    .unwrap();
                assert!(!legacy.exists());
                assert!(root.join(".git/bex-worktrees").is_dir());
            }
            assert!(first.join("packages/app/source.txt").is_file());
        }
    }

    #[tokio::test]
    async fn non_repository_and_unborn_head_fail_without_starting_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        store
            .configure(Some(json!({"createOnNewSession":true})))
            .await
            .unwrap();
        assert!(
            store
                .create(
                    &new_thread(),
                    directory.path().to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .is_err()
        );
        crate::git::text(directory.path(), &["init", "--quiet"]).unwrap();
        assert!(
            store
                .create(
                    &new_thread(),
                    directory.path().to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                    Default::default(),
                    Default::default(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn conversation_settings_persist_with_defaults_and_bounds() {
        use agent_protocol::models::AutoSettle;
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("projects.json");
        let store = Worktrees::new(&state);
        let defaults = store.conversation_settings(None).await.unwrap();
        assert_eq!(defaults, ConversationSettings::default());
        assert_eq!(defaults.auto_settle, AutoSettle::AfterDays(3));
        let mut changed = defaults.clone();
        changed.auto_settle = AutoSettle::Never;
        changed.continue_after_restart = true;
        // Two devices change different settings from the same snapshot.
        for patch in [
            ConversationSettingsPatch {
                auto_settle: Some(AutoSettle::Never),
                ..Default::default()
            },
            ConversationSettingsPatch {
                continue_after_restart: Some(true),
                ..Default::default()
            },
        ] {
            store.conversation_settings(Some(patch)).await.unwrap();
        }
        assert_eq!(store.conversation(), changed);
        let reopened = Worktrees::new(&state);
        assert_eq!(reopened.conversation_settings(None).await.unwrap(), changed);
        let invalid = ConversationSettingsPatch {
            auto_settle: Some(AutoSettle::AfterDays(91)),
            ..Default::default()
        };
        assert!(store.conversation_settings(Some(invalid)).await.is_err());
        assert_eq!(store.conversation(), changed);
    }
}
