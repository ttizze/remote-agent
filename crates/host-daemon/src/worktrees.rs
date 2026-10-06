use agent_protocol::models::{Worktree, WorktreeSettings};
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct State {
    settings: WorktreeSettings,
    workspace_roots: HashMap<String, String>,
}

pub(crate) struct Worktrees {
    path: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl Worktrees {
    pub(crate) fn new(project_state: &Path) -> Self {
        Self {
            path: project_state.with_file_name("bex-worktrees.json"),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub(crate) async fn list(&self) -> Result<Vec<Worktree>> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let state = read(&path)?;
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
        .await?
    }

    pub(crate) async fn remove(&self, target: String, require_merged: bool) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let state = read(&path)?;
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
            // Only remove the empty session container used by the named-checkout layout.
            // Old worktrees and any unrelated files in the container stay untouched.
            let target_path = Path::new(&target);
            if target_path.file_name() == Path::new(root).file_name()
                && let Some(parent) = target_path.parent()
                && parent
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("session-"))
            {
                let _ = fs::remove_dir(parent);
            }
            Ok(())
        })
        .await?
    }

    /// Recreate a deleted checkout at its persisted path so provider sessions
    /// and every client keep using the same working directory.
    pub(crate) async fn ensure_available(&self, cwd: &str) -> Result<Option<PathBuf>> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        let cwd = PathBuf::from(cwd);
        tokio::task::spawn_blocking(move || {
            match fs::metadata(&cwd) {
                Ok(metadata) if metadata.is_dir() => return Ok(None),
                Ok(_) => return Err(anyhow!("working directory is not a directory")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let state = read(&path)?;
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
            let relative_cwd = cwd.strip_prefix(destination)?;
            create_checkout(
                root,
                destination,
                &branch,
                "refs/heads/main",
                if state.settings.copy_on_create {
                    &state.settings.copy_paths
                } else {
                    &[]
                },
                relative_cwd,
            )?;
            Ok(Some(destination.to_path_buf()))
        })
        .await?
    }

    pub(crate) async fn settings(
        &self,
        update: Option<WorktreeSettings>,
    ) -> Result<WorktreeSettings> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            if let Some(settings) = update {

                for entry in &settings.copy_paths { relative_path(entry)?; }
                if !settings.worktree_directory.is_empty() && !Path::new(&settings.worktree_directory).is_absolute() {
                    return Err(anyhow!("worktree directory must be an absolute path on the Host, or empty for the default"));
                }
                state.settings = settings;
                save(&path, &state)?;
            }
            Ok(state.settings)
        })
        .await
        ?
    }

    /// A new checkout of `cwd`'s repository for one thread, from `base_ref` or,
    /// with `start_from_origin`, from its origin branch. Returns the working
    /// directory and the branch.
    pub(crate) async fn create(
        &self,
        cwd: &str,
        base_ref: &str,
        branch: Option<String>,
        start_from_origin: bool,
    ) -> Result<(PathBuf, String)> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        let cwd = PathBuf::from(cwd);
        let base_ref = base_ref.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            let base = if start_from_origin {
                crate::git::text(&cwd, &["fetch", "--quiet", "origin", &base_ref])
                    .with_context(|| format!("Could not fetch {base_ref} from origin"))?;
                format!("origin/{base_ref}")
            } else {
                base_ref
            };
            checkout(&path, &mut state, &cwd, &base, branch)
        })
        .await?
    }
}

/// Creates a managed checkout of the repository containing `cwd` and records it.
fn checkout(
    path: &Path,
    state: &mut State,
    cwd: &Path,
    base: &str,
    branch: Option<String>,
) -> Result<(PathBuf, String)> {
    let cwd = dunce::canonicalize(cwd)?;
    let root =
        dunce::canonicalize(crate::git::text(&cwd, &["rev-parse", "--show-toplevel"])?.trim_end())?;
    let relative_cwd = cwd.strip_prefix(&root)?;
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
    let parent = if state.settings.worktree_directory.is_empty() {
        let exclude = PathBuf::from(
            crate::git::text(
                &root,
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
    let parent = dunce::canonicalize(&parent)?;
    let session = crate::platform::worktree_directory(&parent)?.keep();
    scopeguard::defer! { let _ = fs::remove_dir(&session); }
    let destination = session.join(
        original
            .file_name()
            .context("repository has no folder name")?,
    );
    let branch = branch
        .unwrap_or_else(|| format!("agent/{}", session.file_name().unwrap().to_string_lossy()));
    let target = create_checkout(
        &root,
        &destination,
        &branch,
        base,
        if state.settings.copy_on_create {
            &state.settings.copy_paths
        } else {
            &[]
        },
        relative_cwd,
    )?;
    state.workspace_roots.insert(
        destination
            .to_str()
            .context("worktree path is not UTF-8")?
            .to_owned(),
        original.to_string_lossy().into_owned(),
    );
    save(path, state).map_err(|error| discard_checkout(&root, &destination, &branch, error))?;
    Ok((target, branch))
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

/// Fresh and recreated worktrees share checkout, copy and rollback rules.
fn create_checkout(
    source: &Path,
    destination: &Path,
    branch: &str,
    base: &str,
    copy_paths: &[String],
    relative_cwd: &Path,
) -> Result<PathBuf> {
    let destination_text = destination.to_str().context("worktree path is not UTF-8")?;
    if let Some(parent) = destination.parent() {
        crate::platform::create_state_directory(parent)?;
    }
    crate::platform::create_state_directory(destination)?;
    if let Err(error) = crate::git::text(source, &["branch", branch, base]) {
        let _ = fs::remove_dir(destination);
        return Err(error);
    }
    // Create the branch separately so a locked/missing checkout cannot leak it.
    // One --force replaces a missing registration but continues to respect locks.
    if let Err(error) = crate::git::text(
        source,
        &["worktree", "add", "--force", destination_text, branch],
    ) {
        let _ = fs::remove_dir(destination);
        return Err(match crate::git::text(source, &["branch", "-D", branch]) {
            Ok(_) => error,
            Err(cleanup) => error.context(format!("branch cleanup failed: {cleanup:#}")),
        });
    }
    let prepared = (|| {
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
        let target = if relative_cwd.as_os_str().is_empty() {
            destination.to_path_buf()
        } else {
            destination.join(relative_cwd)
        };
        if !target.is_dir() {
            return Err(anyhow!("working directory does not exist in the checkout"));
        }
        Ok(target)
    })();
    prepared.map_err(|error| discard_checkout(source, destination, branch, error))
}

fn discard_checkout(
    source: &Path,
    destination: &Path,
    branch: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let cleanup = (|| -> Result<()> {
        crate::git::text(
            source,
            &[
                "worktree",
                "remove",
                "--force",
                destination.to_str().context("worktree path is not UTF-8")?,
            ],
        )?;
        crate::git::text(source, &["branch", "-D", branch])?;
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
                .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(root.to_str().unwrap(), "HEAD", None, false)
            .await
            .unwrap()
            .0;
        let other = worktrees
            .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(root.to_str().unwrap(), "HEAD", None, false)
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
                    .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(root.to_str().unwrap(), "HEAD", None, false)
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
            .create(first.to_str().unwrap(), "HEAD", None, false)
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
    async fn existing_checkouts_keep_original_name_location_and_selected_subdirectory() {
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
                    legacy.join("packages/app").to_str().unwrap(),
                    "HEAD",
                    None,
                    false,
                )
                .await
                .unwrap()
                .0;
            let checkout = first.parent().unwrap().parent().unwrap();
            assert_eq!(
                first.strip_prefix(checkout).unwrap(),
                Path::new("packages/app")
            );
            assert_eq!(checkout.file_name(), root.file_name());
            assert_eq!(
                checkout.parent().unwrap().parent().unwrap(),
                root.join(".worktree")
            );
            assert_eq!(
                fs::read_to_string(first.join("source.txt")).unwrap(),
                "worktree-only commit"
            );
            assert_eq!(
                crate::git::text(checkout, &["rev-parse", "HEAD"]).unwrap(),
                crate::git::text(&legacy, &["rev-parse", "HEAD"]).unwrap()
            );
            assert!(!root.join("packages/app/source.txt").exists());
            let second = Worktrees::new(&projects)
                .create(first.to_str().unwrap(), "HEAD", None, false)
                .await
                .unwrap()
                .0;
            let second_checkout = second.parent().unwrap().parent().unwrap();
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
            assert!(first.join("source.txt").is_file());
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
                .create(directory.path().to_str().unwrap(), "HEAD", None, false)
                .await
                .is_err()
        );
        crate::git::text(directory.path(), &["init", "--quiet"]).unwrap();
        assert!(
            store
                .create(directory.path().to_str().unwrap(), "HEAD", None, false)
                .await
                .is_err()
        );
    }
}
