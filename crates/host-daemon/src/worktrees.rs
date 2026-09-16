use agent_core::models::{Worktree, WorktreeSettings};
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

    pub(crate) async fn remove(&self, target: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            let root = state
                .workspace_roots
                .get(&target)
                .context("Bexが作成したワークツリーではありません。")?;
            if !already_removed(&target, root)? {
                let (_, blocked) = inspect(&target, root)?;
                if let Some(reason) = blocked {
                    return Err(anyhow!(reason));
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
            state.workspace_roots.remove(&target);
            save(&path, &state)
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
                if !settings.extra.is_empty() { return Err(anyhow!("unknown worktree setting")); }
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

    pub(crate) async fn prepare(&self, cwd: Option<&str>) -> Result<Option<PathBuf>> {
        // Automatic worktrees need an explicitly selected checkout.
        let Some(cwd) = cwd else {
            return Ok(None);
        };
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        let cwd = PathBuf::from(cwd);
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            if !state.settings.create_on_new_session {
                return Ok(None);
            }
            let cwd = cwd.canonicalize()?;
            let root = PathBuf::from(
                crate::git::text(&cwd, &["rev-parse", "--show-toplevel"])?.trim_end(),
            )
            .canonicalize()?;
            let relative_cwd = cwd.strip_prefix(&root)?;
            let original = match state
                .workspace_roots
                .get(root.to_str().context("project path is not UTF-8")?)
            {
                Some(original) => PathBuf::from(original),
                None => PathBuf::from(
                    crate::git::text(&root, &["worktree", "list", "--porcelain", "-z"])?
                        .split('\0')
                        .next()
                        .and_then(|line| line.strip_prefix("worktree "))
                        .context("Git did not return the original repository")?,
                ),
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
            let parent = parent.canonicalize()?;
            let session = crate::platform::worktree_directory(&parent)?.keep();
            scopeguard::defer! { let _ = fs::remove_dir(&session); }
            let destination = session.join(
                original
                    .file_name()
                    .context("repository has no folder name")?,
            );
            crate::platform::create_state_directory(&destination)?;
            let branch = format!("bex/{}", session.file_name().unwrap().to_string_lossy());
            let destination_text = destination.to_str().context("worktree path is not UTF-8")?;
            if let Err(error) = crate::git::text(
                &root,
                &["worktree", "add", "-b", &branch, destination_text, "HEAD"],
            ) {
                let _ = fs::remove_dir(&destination);
                return Err(error);
            }
            let prepared = (|| {
                if state.settings.copy_on_create {
                    for entry in &state.settings.copy_paths {
                        let relative = relative_path(entry)?;
                        let source = root.join(relative);
                        match fs::symlink_metadata(&source) {
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(e) => return Err(e.into()),
                            Ok(_) => {}
                        }
                        // Check every ancestor as well as the leaf before following it.
                        no_symlinks(&root, relative)?;
                        copy(&source, &destination.join(relative))?;
                    }
                }
                let target = if relative_cwd.as_os_str().is_empty() {
                    destination.clone()
                } else {
                    destination.join(relative_cwd)
                };
                if !target.is_dir() {
                    return Err(anyhow!("working directory does not exist in HEAD"));
                }
                state.workspace_roots.insert(
                    destination_text.to_owned(),
                    original.to_string_lossy().into_owned(),
                );
                save(&path, &state)?;
                Ok(target)
            })();
            match prepared {
                Ok(target) => Ok(Some(target)),
                Err(error) => {
                    let cleanup = crate::git::text(
                        &root,
                        &["worktree", "remove", "--force", destination_text],
                    )
                    .and_then(|_| crate::git::text(&root, &["branch", "-D", &branch]));
                    match cleanup {
                        Ok(_) => Err(error),
                        Err(cleanup) => {
                            Err(error.context(format!("worktree cleanup failed: {cleanup:#}")))
                        }
                    }
                }
            }
        })
        .await?
    }
}

/// Inspect each visible execution directory once per list request. Git state must
/// not share the project settings cache: main can move without settings changing.
pub(crate) async fn merged_directories(directories: HashSet<String>) -> Result<HashSet<String>> {
    use futures_util::{StreamExt, TryStreamExt};
    // Bound process fan-out while avoiding a serial Git round trip for every
    // visible conversation. Recompute on every request so new commits stay fresh.
    let results: Vec<_> = futures_util::stream::iter(directories)
        .map(|cwd| async move {
            tokio::task::spawn_blocking(move || {
                merged_into_main(Path::new(&cwd))
                    .unwrap_or(false)
                    .then_some(cwd)
            })
            .await
        })
        .buffer_unordered(4)
        .try_collect()
        .await?;
    Ok(results.into_iter().flatten().collect())
}

fn merged_into_main(cwd: &Path) -> Result<bool> {
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
        return Ok(false);
    };
    if git_dir == common_dir || branch == "refs/heads/main" || !branch.starts_with("refs/heads/") {
        return Ok(false);
    }
    let history = crate::git::text(cwd, &["reflog", "show", "--format=%H", branch])?;
    let contained = crate::git::output(
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            head.trim(),
            "refs/heads/main",
        ],
    )
    .is_ok();
    Ok(agent_core::presentation::list::worktree_branch_merged(
        head.trim(),
        history.lines().last(),
        contained,
    ))
}

fn inspect(path: &str, project: &str) -> Result<(String, Option<String>)> {
    if already_removed(path, project)? {
        return Ok(("削除済み（登録を解除できます）".into(), None));
    }
    let target = Path::new(path)
        .canonicalize()
        .context("ワークツリーを確認できません")?;
    let project = Path::new(project)
        .canonicalize()
        .context("元のリポジトリを確認できません")?;
    if target != Path::new(path) || target == project {
        return Err(anyhow!("登録されたワークツリーの場所が変わっています。"));
    }
    let listing = crate::git::text(&project, &["worktree", "list", "--porcelain", "-z"])?;
    let expected = format!("worktree {path}");
    let entry = listing
        .split("\0\0")
        .find(|entry| entry.split('\0').next() == Some(expected.as_str()))
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

// A registry write can fail after Git has removed the directory. Allow retrying
// that write only when both the filesystem and Git agree removal is complete.
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
    let expected = format!("worktree {path}");
    Ok(!listing.split('\0').any(|field| field == expected))
}

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
        crate::git::text(root, &["init", "--quiet"]).unwrap();
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
    async fn invalid_saved_settings_keep_the_json_error() {
        let directory = tempfile::tempdir().unwrap();
        let worktrees = Worktrees::new(&directory.path().join("projects.json"));
        fs::write(&worktrees.path, "invalid json").unwrap();
        let error = worktrees.list().await.unwrap_err();
        assert!(error.downcast_ref::<serde_json::Error>().is_some());
    }

    #[tokio::test]
    async fn managed_removal_rechecks_changes_locks_and_ownership_and_preserves_commits() {
        let repository = repository();
        let root = repository.path().canonicalize().unwrap();
        let state = root.join("projects.json");
        let worktrees = Worktrees::new(&state);
        worktrees
            .settings(Some(WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let first = worktrees.prepare(root.to_str()).await.unwrap().unwrap();
        let other = worktrees.prepare(root.to_str()).await.unwrap().unwrap();
        let first_path = first.to_str().unwrap().to_owned();
        let branch = crate::git::text(&first, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        let head = crate::git::text(&first, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(Worktrees::new(&state).list().await.unwrap().len(), 2);
        assert!(
            worktrees
                .remove(root.to_str().unwrap().into())
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
            assert!(worktrees.remove(first_path.clone()).await.is_err());
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
        assert!(worktrees.remove(first_path.clone()).await.is_err());
        crate::git::text(&root, &["worktree", "unlock", &first_path]).unwrap();
        crate::git::text(&first, &["checkout", "--detach", "HEAD"]).unwrap();
        assert!(worktrees.remove(first_path.clone()).await.is_err());
        crate::git::text(&first, &["checkout", &branch]).unwrap();
        worktrees.remove(first_path.clone()).await.unwrap();
        assert!(!first.exists());
        assert!(!first.parent().unwrap().exists());
        assert!(other.join("tracked.txt").exists());
        assert_eq!(
            crate::git::text(&root, &["rev-parse", &branch]).unwrap(),
            head
        );
        let restarted = Worktrees::new(&state);
        let entries = restarted.list().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, other.to_str().unwrap());
        assert!(
            !workspace_roots(&state)
                .await
                .unwrap()
                .contains_key(&first_path)
        );
        // Simulate a registry write failure after successful Git removal.
        let other_path = other.to_str().unwrap();
        crate::git::text(&root, &["worktree", "remove", "--", other_path]).unwrap();
        assert!(restarted.list().await.unwrap()[0].blocked_reason.is_none());
        let unrelated = other.parent().unwrap().join("keep.txt");
        fs::write(&unrelated, "preserve").unwrap();
        restarted.remove(other_path.into()).await.unwrap();
        assert!(Worktrees::new(&state).list().await.unwrap().is_empty());
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "preserve");
    }

    #[tokio::test]
    async fn settings_persist_and_toggles_control_real_worktree_creation_and_copying() {
        let directory = repository();
        let root = directory.path().canonicalize().unwrap();
        let projects = root.join("projects.json");
        let store = Worktrees::new(&projects);
        assert!(store.prepare(None).await.unwrap().is_none());
        fs::write(root.join("tracked.txt"), "uncommitted\n").unwrap();
        fs::write(root.join("config.txt"), "local config\n").unwrap();
        fs::write(root.join(".env"), "FIXTURE_TOKEN=isolated\n").unwrap();
        #[cfg(unix)]
        fs::set_permissions(root.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(root.join("local")).unwrap();
        fs::write(root.join("local/value"), "local data").unwrap();
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env","local","missing","config.txt"],"worktreeDirectory":""});
        store.configure(Some(settings.clone())).await.unwrap();
        let loaded = Worktrees::new(&projects);
        assert_eq!(loaded.configure(None).await.unwrap(), settings);
        let first = loaded.prepare(root.to_str()).await.unwrap().unwrap();
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
                .starts_with("bex/session-")
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
        let second = store.prepare(root.to_str()).await.unwrap().unwrap();
        assert_ne!(first, second);
        assert!(!second.join(".env").exists());
        store
            .configure(Some(
                json!({"createOnNewSession":false,"copyOnCreate":true,"copyPaths":[".env"]}),
            ))
            .await
            .unwrap();
        assert!(store.prepare(root.to_str()).await.unwrap().is_none());
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
                store.prepare(root.to_str()).await.is_err(),
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
        let valid = json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"],"worktreeDirectory":""});
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
        let root = directory.path().canonicalize().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let real_parent = storage.path().canonicalize().unwrap();
        symlink(&real_parent, root.join("storage")).unwrap();
        fs::write(root.join(".env"), "FIXTURE_VALUE=isolated\n").unwrap();
        let projects = root.join("projects.json");
        let store = Worktrees::new(&projects);
        let mut settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env"],"worktreeDirectory":root.join("storage/new folder")});
        store.configure(Some(settings.clone())).await.unwrap();
        let restarted = Worktrees::new(&projects);
        let first = restarted.prepare(root.to_str()).await.unwrap().unwrap();
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
        let second = restarted.prepare(first.to_str()).await.unwrap().unwrap();
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
            let root = repository.path().canonicalize().unwrap();
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
                .prepare(legacy.join("packages/app").to_str())
                .await
                .unwrap()
                .unwrap();
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
                .prepare(first.to_str())
                .await
                .unwrap()
                .unwrap();
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
                store.remove(legacy.to_str().unwrap().into()).await.unwrap();
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
        assert!(store.prepare(directory.path().to_str()).await.is_err());
        crate::git::text(directory.path(), &["init", "--quiet"]).unwrap();
        assert!(store.prepare(directory.path().to_str()).await.is_err());
    }
}
