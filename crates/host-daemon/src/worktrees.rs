use agent_protocol::models::{Worktree, WorktreeSettings};
use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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

pub(crate) enum Removal {
    Manual,
    /// Cleanup of a checkout whose HEAD `base` contains, for the project that owns it.
    Unchanged {
        project: Option<String>,
        base: String,
    },
}

const GIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);
const REMOVE_LIMIT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Resolve and refresh the ref an unchanged checkout must be contained in: the
/// primary remote's default branch, or the project's branch without a remote.
pub(crate) fn integration_base(project: &Path) -> Result<String> {
    let text = |output: std::process::Output| {
        String::from_utf8(output.stdout).context("git returned non-UTF-8 output")
    };
    let remotes = text(crate::git::bounded(project, &["remote"], GIT_LIMIT)?)?;
    let remotes: Vec<_> = remotes.lines().filter(|name| !name.is_empty()).collect();
    let Some(remote) = remotes
        .iter()
        .find(|name| **name == "origin")
        .or(remotes.first())
    else {
        let head = crate::git::bounded(project, &["symbolic-ref", "--quiet", "HEAD"], GIT_LIMIT)
            .context("project branch is unavailable")?;
        return Ok(text(head)?.trim().to_owned());
    };
    let tracking = format!("refs/remotes/{remote}/");
    let local = crate::git::bounded(
        project,
        &["symbolic-ref", &format!("{tracking}HEAD")],
        GIT_LIMIT,
    )
    .ok()
    .and_then(|output| text(output).ok())
    .and_then(|head| head.trim().strip_prefix(&tracking).map(str::to_owned));
    let branch = match local {
        Some(branch) => branch,
        // Without a local HEAD ref only the remote can name its default;
        // a conventional name may be a feature branch.
        None => text(crate::git::bounded(
            project,
            &["ls-remote", "--symref", "--", remote, "HEAD"],
            std::time::Duration::from_secs(20),
        )?)?
        .lines()
        .find_map(|line| {
            line.strip_prefix("ref: refs/heads/")?
                .strip_suffix("\tHEAD")
        })
        .map(str::to_owned)
        .context("default branch is unavailable")?,
    };
    crate::git::bounded(
        project,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--",
            remote,
            &format!("+refs/heads/{branch}:{tracking}{branch}"),
        ],
        GIT_LIMIT,
    )?;
    Ok(format!("{tracking}{branch}"))
}

/// Any failure, including a missing base, counts as not contained and keeps the checkout.
fn contained(checkout: &Path, base: &str) -> bool {
    crate::git::output(checkout, &["merge-base", "--is-ancestor", "HEAD", base]).is_ok()
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
            // Many checkouts share a repository. Read its registration once per
            // request, including deleted paths, rather than once per checkout.
            let mut listings = HashMap::new();
            let mut entries = state
                .workspace_roots
                .into_iter()
                .map(|(path, project_path)| {
                    let listing = listings.entry(project_path.clone()).or_insert_with(|| {
                        crate::git::text(
                            Path::new(&project_path),
                            &["worktree", "list", "--porcelain", "-z"],
                        )
                        .map_err(|error| format!("{error:#}"))
                    });
                    let (branch, blocked_reason) = listing
                        .as_ref()
                        .map_err(|error| anyhow!(error.clone()))
                        .and_then(|listing| inspect(&path, &project_path, listing))
                        .map(|entry| entry.unwrap_or_else(|| ("削除済み".into(), None)))
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

    pub(crate) async fn remove(&self, target: String, removal: Removal) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let state = read(&path)?;
            let root = state
                .workspace_roots
                .get(&target)
                .context("Bexが作成したワークツリーではありません。")?;
            let listing =
                crate::git::text(Path::new(root), &["worktree", "list", "--porcelain", "-z"])?;
            if let Some((_, blocked)) = inspect(&target, root, &listing)? {
                if let Some(reason) = blocked {
                    return Err(anyhow!(reason));
                }
                // Recheck both the preference and HEAD after the activity scan.
                // A new commit or a disabled preference cancels automatic removal.
                if let Removal::Unchanged { project, base } = &removal
                    && !(state.settings.deletes_unchanged(project.as_deref())
                        && contained(Path::new(&target), base))
                {
                    return Ok(());
                }
                // Keep the branch so commits remain reachable even if not merged.
                remove_linked_worktree(Path::new(root), Path::new(&target), false)?;
            }
            remove_session_container(Path::new(&target), Path::new(root));
            Ok(())
        })
        .await?
    }

    /// Roll back a checkout, including its branch, made for a session start
    /// that failed before any conversation recorded it.
    pub(crate) async fn discard(&self, cwd: String) -> Result<()> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            let (target, root) = state
                .workspace_roots
                .iter()
                .filter(|(target, _)| Path::new(&cwd).starts_with(target))
                .max_by_key(|(target, _)| target.len())
                .map(|(target, root)| (target.clone(), root.clone()))
                .context("Bexが作成したワークツリーではありません。")?;
            let branch = crate::git::text(Path::new(&target), &["branch", "--show-current"])?;
            remove_checkout(Path::new(&root), Path::new(&target), branch.trim())?;
            state.workspace_roots.remove(&target);
            save(&path, &state)?;
            remove_session_container(Path::new(&target), Path::new(&root));
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
                Ok(_) => {
                    return Err(anyhow!(
                        "作業フォルダに指定したパスはフォルダではありません。"
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let state = read(&path)?;
            let (target, project) = state
                .workspace_roots
                .iter()
                .filter(|(target, _)| cwd.starts_with(target))
                .max_by_key(|(target, _)| target.len())
                .context("作業フォルダが見つかりません。")?;
            let destination = Path::new(target);
            // A missing subdirectory in an existing checkout is not a deleted worktree.
            match fs::symlink_metadata(destination) {
                Ok(_) => return Err(anyhow!("作業フォルダが見つかりません。")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let root = Path::new(project);
            let branch = format!("bex/session-{}", uuid::Uuid::new_v4());
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
                    return Err(anyhow!("ワークツリーの保存先には、ホスト上の絶対パスを入力するか空欄にしてください。"));
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
        let state_path = path.clone();
        let mut state = tokio::task::spawn_blocking(move || read(&state_path)).await??;
        if !state.settings.create_on_new_session {
            return Ok(None);
        }
        let cwd = PathBuf::from(cwd);
        tokio::task::spawn_blocking(move || {
            let cwd = dunce::canonicalize(&cwd)?;
            let root = dunce::canonicalize(
                crate::git::text(&cwd, &["rev-parse", "--show-toplevel"])?.trim_end(),
            )?;
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
                    .context("リポジトリのフォルダ名を取得できません。")?,
            );
            let branch = format!("bex/{}", session.file_name().unwrap().to_string_lossy());
            let target = create_checkout(
                &root,
                &destination,
                &branch,
                "HEAD",
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
            save(&path, &state)
                .map_err(|error| discard_checkout(&root, &destination, &branch, error))?;
            Ok(Some(target))
        })
        .await?
    }
}

fn worktree_path(entry: &str) -> Option<&Path> {
    entry
        .split('\0')
        .next()?
        .strip_prefix("worktree ")
        .map(Path::new)
}

fn inspect(path: &str, project: &str, listing: &str) -> Result<Option<(String, Option<String>)>> {
    if already_removed(path, listing)? {
        return Ok(None);
    }
    let target = dunce::canonicalize(path).context("ワークツリーを確認できません")?;
    let project = dunce::canonicalize(project).context("元のリポジトリを確認できません")?;
    if target != Path::new(path) || target == project {
        return Err(anyhow!("登録されたワークツリーの場所が変わっています。"));
    }
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
    Ok(Some((branch.unwrap_or("detached HEAD").into(), reason)))
}

// Treat removal as complete only when both the filesystem and Git agree.
fn already_removed(path: &str, listing: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(!listing
        .split("\0\0")
        .any(|entry| worktree_path(entry) == Some(Path::new(path))))
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
            agent_local_host::private_file_options(),
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
    let base = crate::workspace_review::comparison::source_ref(source, base)?;
    let destination_text = destination.to_str().context("worktree path is not UTF-8")?;
    if let Some(parent) = destination.parent() {
        agent_local_host::create_state_directory(parent)?;
    }
    agent_local_host::create_state_directory(destination)?;
    if let Err(error) = crate::git::text(source, &["branch", branch, &base]) {
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
        crate::git::text(
            source,
            &[
                "config",
                &crate::workspace_review::comparison::base_key(branch),
                &base,
            ],
        )?;
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
            return Err(anyhow!("作業フォルダがワークツリー内に見つかりません。"));
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
    match remove_checkout(source, destination, branch) {
        Ok(_) => error,
        Err(cleanup) => error.context(format!("worktree cleanup failed: {cleanup:#}")),
    }
}

fn remove_checkout(source: &Path, destination: &Path, branch: &str) -> Result<()> {
    remove_linked_worktree(source, destination, true)?;
    crate::git::text(source, &["branch", "-D", branch])?;
    Ok(())
}

/// Remove a linked worktree of `root`. Git for Windows before 2.54 follows NTFS
/// junctions, such as pnpm's, into their targets, and newer Git leaves them
/// behind; there the checkout is deleted here without following links.
fn remove_linked_worktree(root: &Path, target: &Path, force: bool) -> Result<()> {
    let target_text = target.to_str().context("worktree path is not UTF-8")?;
    if cfg!(windows) && target.join(".git").is_file() {
        return remove_without_following_links(root, target, force);
    }
    // Git's untracked check honors status.showUntrackedFiles=no and would
    // delete those files; it rechecks changes and locks at removal time.
    let mut args = vec![
        "-c",
        "status.showUntrackedFiles=normal",
        "worktree",
        "remove",
    ];
    if force {
        args.push("--force");
    }
    args.extend(["--", target_text]);
    crate::git::bounded(root, &args, REMOVE_LIMIT)?;
    Ok(())
}

/// Apply Git's removal refusals, delete the checkout without following links,
/// then let Git drop only the registration. A failure partway restores `.git`
/// so a retry can remove the rest.
fn remove_without_following_links(root: &Path, target: &Path, force: bool) -> Result<()> {
    let target_text = target.to_str().context("worktree path is not UTF-8")?;
    let refuse = |message: String| anyhow!("fatal: {message}");
    let not_a_worktree = || refuse(format!("'{}' is not a working tree", target.display()));
    let git_file = target.join(".git");
    // A registered worktree of this repository: its `.git` file names an admin
    // directory under the common `worktrees/`, whose `gitdir` points back.
    let admin = PathBuf::from(
        crate::git::text(target, &["rev-parse", "--absolute-git-dir"])
            .map_err(|_| not_a_worktree())?
            .trim_end(),
    );
    let common = root.join(crate::git::text(root, &["rev-parse", "--git-common-dir"])?.trim_end());
    let same = |left: &Path, right: &Path| matches!((dunce::canonicalize(left), dunce::canonicalize(right)), (Ok(left), Ok(right)) if left == right);
    let points_back = fs::read_to_string(admin.join("gitdir"))
        .is_ok_and(|gitdir| same(&admin.join(gitdir.trim()), &git_file));
    if !points_back
        || !admin
            .parent()
            .is_some_and(|parent| same(parent, &common.join("worktrees")))
    {
        return Err(not_a_worktree());
    }
    // Git counts the lock file's existence; anything but a confirmed absence refuses.
    match fs::read_to_string(admin.join("locked")) {
        Ok(reason) if reason.trim().is_empty() => {
            return Err(refuse("cannot remove a locked working tree".into()));
        }
        Ok(reason) => {
            return Err(refuse(format!(
                "cannot remove a locked working tree, lock reason: {}",
                reason.trim()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(refuse(format!(
                "cannot inspect the lock of '{}': {error}",
                target.display()
            )));
        }
    }
    if !force {
        // Bind the checks like Git's own, so `core.worktree` cannot point them elsewhere.
        let git_dir = format!("--git-dir={}", git_file.display());
        let work_tree = format!("--work-tree={}", target.display());
        let submodules =
            || refuse("working trees containing submodules cannot be moved or removed".into());
        // An unreadable path counts as present, so a doubt refuses.
        let present = |path: &Path| !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
        if present(&admin.join("modules")) {
            return Err(submodules());
        }
        let index = crate::git::text(target, &[&git_dir, &work_tree, "ls-files", "--stage", "-z"])?;
        if index
            .split('\0')
            .filter(|entry| entry.starts_with("160000 "))
            .filter_map(|entry| entry.split_once('\t'))
            .any(|(_, submodule)| present(&target.join(submodule).join(".git")))
        {
            return Err(submodules());
        }
        let status = crate::git::text(
            target,
            &[
                "-c",
                "status.showUntrackedFiles=normal",
                &git_dir,
                &work_tree,
                "status",
                "--porcelain",
                "--ignore-submodules=none",
            ],
        )?;
        if !status.is_empty() {
            return Err(refuse(format!(
                "'{}' contains modified or untracked files, use --force to delete it",
                target.display()
            )));
        }
    }
    let contents = fs::read(&git_file).map_err(|_| not_a_worktree())?;
    // The standard library removes links, including junctions, without following them.
    fs::remove_dir_all(target).map_err(|error| {
        let _ = fs::write(&git_file, &contents);
        refuse(format!("failed to delete '{}': {error}", target.display()))
    })?;
    crate::git::bounded(
        root,
        &[
            "worktree",
            "remove",
            "--force",
            "--force",
            "--",
            target_text,
        ],
        REMOVE_LIMIT,
    )?;
    Ok(())
}

/// Only remove the empty session container used by the named-checkout layout.
/// Old worktrees and any unrelated files in the container stay untouched.
fn remove_session_container(target: &Path, root: &Path) {
    if target.file_name() == root.file_name()
        && let Some(parent) = target.parent()
        && parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("session-"))
    {
        let _ = fs::remove_dir(parent);
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
            return Err(anyhow!(
                "コピーするパスにシンボリックリンクは指定できません。"
            ));
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
                return Err(anyhow!("コピー先にシンボリックリンクが含まれています。"));
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
        return Err(anyhow!(
            "コピーできるのは通常のファイルとフォルダだけです。"
        ));
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
            let cwd = store.prepare(root.to_str()).await.unwrap().unwrap();
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
                    .remove(cwd.to_str().unwrap().into(), Removal::Manual)
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

    fn commit(cwd: &Path, message: &str) {
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
                "--allow-empty",
                "--quiet",
                "-m",
                message,
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn automatic_removal_rechecks_containment_and_opt_in_before_removing_checkout() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let store = Worktrees::new(&root.join("projects.json"));
        let mut settings = WorktreeSettings {
            create_on_new_session: true,
            delete_unchanged: true,
            ..Default::default()
        };
        store.settings(Some(settings.clone())).await.unwrap();
        let base = integration_base(&root).unwrap();
        assert_eq!(
            base, "refs/heads/main",
            "without a remote the project branch is the base"
        );
        let unchanged = |project: Option<&str>| Removal::Unchanged {
            project: project.map(str::to_owned),
            base: base.clone(),
        };
        let fresh = store.prepare(root.to_str()).await.unwrap().unwrap();
        store
            .remove(fresh.to_str().unwrap().into(), unchanged(None))
            .await
            .unwrap();
        assert!(!fresh.exists(), "a checkout without commits is unchanged");

        let cwd = store.prepare(root.to_str()).await.unwrap().unwrap();
        let target = cwd.to_str().unwrap().to_owned();
        let branch = crate::git::text(&cwd, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        let merge = || crate::git::text(&root, &["merge", "--ff-only", &branch]).unwrap();
        commit(&cwd, "work");
        store.remove(target.clone(), unchanged(None)).await.unwrap();
        assert!(cwd.is_dir(), "unmerged work must stay");
        merge();
        settings.delete_unchanged = false;
        store.settings(Some(settings.clone())).await.unwrap();
        store.remove(target.clone(), unchanged(None)).await.unwrap();
        assert!(
            cwd.is_dir(),
            "disabling cleanup cancels an earlier eligibility decision"
        );
        settings.project_delete_unchanged = [("project".to_owned(), true)].into();
        store.settings(Some(settings.clone())).await.unwrap();
        store
            .remove(target.clone(), unchanged(Some("other")))
            .await
            .unwrap();
        assert!(cwd.is_dir(), "another project's override does not apply");
        commit(&cwd, "after merge");
        store
            .remove(target.clone(), unchanged(Some("project")))
            .await
            .unwrap();
        assert!(cwd.is_dir(), "a commit after merge cancels removal");
        merge();
        let head = crate::git::text(&cwd, &["rev-parse", "HEAD"]).unwrap();
        store
            .remove(target, unchanged(Some("project")))
            .await
            .unwrap();
        assert!(!cwd.exists());
        assert_eq!(
            crate::git::text(&root, &["rev-parse", &branch]).unwrap(),
            head
        );
    }

    #[tokio::test]
    async fn work_pushed_to_the_remote_default_branch_is_unchanged() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let remote = tempfile::tempdir().unwrap();
        crate::git::text(
            remote.path(),
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        )
        .unwrap();
        let remote_path = remote.path().to_str().unwrap();
        crate::git::text(&root, &["remote", "add", "origin", remote_path]).unwrap();
        crate::git::text(&root, &["push", "--quiet", "origin", "main"]).unwrap();
        let store = Worktrees::new(&root.join("projects.json"));
        store
            .settings(Some(WorktreeSettings {
                create_on_new_session: true,
                delete_unchanged: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let cwd = store.prepare(root.to_str()).await.unwrap().unwrap();
        commit(&cwd, "work");
        let base = integration_base(&root).unwrap();
        assert_eq!(
            base, "refs/remotes/origin/main",
            "the remote names its default branch"
        );
        let unchanged = || Removal::Unchanged {
            project: None,
            base: base.clone(),
        };
        store
            .remove(cwd.to_str().unwrap().into(), unchanged())
            .await
            .unwrap();
        assert!(cwd.is_dir(), "work the remote lacks must stay");
        crate::git::text(&cwd, &["push", "--quiet", "origin", "HEAD:main"]).unwrap();
        crate::git::text(&root, &["update-ref", "-d", "refs/remotes/origin/main"]).unwrap();
        assert_eq!(integration_base(&root).unwrap(), base);
        store
            .remove(cwd.to_str().unwrap().into(), unchanged())
            .await
            .unwrap();
        assert!(
            !cwd.exists(),
            "work pushed directly to the default branch is integrated even though local main is behind"
        );
    }

    #[tokio::test]
    async fn discarding_a_failed_start_removes_its_checkout_branch_and_record() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let store = Worktrees::new(&root.join("projects.json"));
        store
            .settings(Some(WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let cwd = store.prepare(root.to_str()).await.unwrap().unwrap();
        let branch = crate::git::text(&cwd, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        fs::write(cwd.join("partial.txt"), "written before the start failed").unwrap();
        store.discard(cwd.to_str().unwrap().into()).await.unwrap();
        assert!(!cwd.exists());
        assert!(
            !cwd.parent().unwrap().exists(),
            "the session container goes too"
        );
        assert!(crate::git::text(&root, &["rev-parse", "--verify", "--quiet", &branch]).is_err());
        assert!(store.list().await.unwrap().is_empty());
        assert!(
            store.discard(cwd.to_str().unwrap().into()).await.is_err(),
            "only recorded checkouts can be discarded"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_removal_does_not_follow_junctions() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("shared.txt"), "outside the checkout").unwrap();
        let linked = root.join(".worktree").join("linked");
        crate::git::text(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "linked",
                linked.to_str().unwrap(),
            ],
        )
        .unwrap();
        let junction = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(linked.join("store"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(junction.status.success(), "{junction:?}");
        remove_linked_worktree(&root, &linked, true).unwrap();
        assert!(!linked.exists());
        assert_eq!(
            fs::read_to_string(outside.path().join("shared.txt")).unwrap(),
            "outside the checkout"
        );
        let listing = crate::git::text(&root, &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(
            listing.matches("worktree ").count(),
            1,
            "Git drops the registration"
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_safe_removal_keeps_link_targets_and_applies_git_refusals() {
        let repository = repository();
        let root = dunce::canonicalize(repository.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("shared.txt"), "outside the checkout").unwrap();
        let add = |name: &str| {
            let path = root.join(".worktree").join(name);
            crate::git::text(
                &root,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    name,
                    path.to_str().unwrap(),
                ],
            )
            .unwrap();
            path
        };
        let linked = add("linked");
        // A symbolic link stands in for an NTFS junction, which Windows Git can follow.
        std::os::unix::fs::symlink(outside.path(), linked.join("store")).unwrap();
        let error = remove_without_following_links(&root, &linked, false).unwrap_err();
        assert!(format!("{error:#}").contains("modified or untracked"));
        assert!(linked.join("store").exists(), "a refusal deletes nothing");
        remove_without_following_links(&root, &linked, true).unwrap();
        assert!(!linked.exists());
        assert_eq!(
            fs::read_to_string(outside.path().join("shared.txt")).unwrap(),
            "outside the checkout"
        );
        let listing = crate::git::text(&root, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !listing.contains(linked.to_str().unwrap()),
            "Git drops the registration"
        );
        assert!(crate::git::text(&root, &["rev-parse", "--verify", "linked"]).is_ok());

        let locked = add("locked");
        crate::git::text(
            &root,
            &[
                "worktree",
                "lock",
                "--reason",
                "in use",
                locked.to_str().unwrap(),
            ],
        )
        .unwrap();
        let error = remove_without_following_links(&root, &locked, true).unwrap_err();
        assert!(format!("{error:#}").contains("lock reason: in use"));
        assert!(locked.is_dir());
        assert!(
            remove_without_following_links(&root, &root, true).is_err(),
            "the main checkout is not a linked worktree"
        );
        assert!(root.join("tracked.txt").is_file());
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
                .remove(root.to_str().unwrap().into(), Removal::Manual)
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
            assert!(
                worktrees
                    .remove(first_path.clone(), Removal::Manual)
                    .await
                    .is_err()
            );
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
        let entries = worktrees.list().await.unwrap();
        assert!(
            entries
                .iter()
                .find(|entry| entry.path == first_path)
                .unwrap()
                .blocked_reason
                .as_ref()
                .unwrap()
                .contains("ロック")
        );
        assert!(
            entries
                .iter()
                .find(|entry| entry.path == other.to_str().unwrap())
                .unwrap()
                .blocked_reason
                .is_none()
        );
        assert!(
            worktrees
                .remove(first_path.clone(), Removal::Manual)
                .await
                .is_err()
        );
        crate::git::text(&root, &["worktree", "unlock", &first_path]).unwrap();
        crate::git::text(&first, &["checkout", "--detach", "HEAD"]).unwrap();
        let entries = worktrees.list().await.unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.path == first_path)
            .unwrap();
        assert_eq!(entry.branch, "detached HEAD");
        assert!(entry.blocked_reason.is_some());
        assert!(
            worktrees
                .remove(first_path.clone(), Removal::Manual)
                .await
                .is_err()
        );
        crate::git::text(&first, &["checkout", &branch]).unwrap();
        worktrees
            .remove(first_path.clone(), Removal::Manual)
            .await
            .unwrap();
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
        restarted
            .remove(other_path.into(), Removal::Manual)
            .await
            .unwrap();
        assert_eq!(Worktrees::new(&state).list().await.unwrap().len(), 2);
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "preserve");
        let registered = worktrees.prepare(root.to_str()).await.unwrap().unwrap();
        fs::remove_dir_all(&registered).unwrap();
        let entries = worktrees.list().await.unwrap();
        let entry = entries
            .iter()
            .find(|entry| Path::new(&entry.path) == registered)
            .unwrap();
        assert!(
            entry.blocked_reason.is_some(),
            "a missing checkout still registered with Git must not be treated as removed"
        );
        assert!(
            worktrees
                .remove(entry.path.clone(), Removal::Manual)
                .await
                .is_err()
        );
        #[cfg(unix)]
        assert!(already_removed(root.join("tracked.txt/checkout").to_str().unwrap(), "").is_err());
    }

    #[tokio::test]
    async fn settings_persist_and_toggles_control_real_worktree_creation_and_copying() {
        let directory = repository();
        let root = dunce::canonicalize(directory.path()).unwrap();
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
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env","local","missing","config.txt"],"worktreeDirectory":"","deleteUnchanged":false,"projectDeleteUnchanged":{}});
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
        let valid = json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"],"worktreeDirectory":"","deleteUnchanged":false,"projectDeleteUnchanged":{}});
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
            let root = dunce::canonicalize(repository.path()).unwrap();
            let state_directory = tempfile::tempdir().unwrap();
            let projects = state_directory.path().join("projects.json");
            let legacy = root.join(".git/agent-worktrees/session-legacy");
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
                store
                    .remove(legacy.to_str().unwrap().into(), Removal::Manual)
                    .await
                    .unwrap();
                assert!(!legacy.exists());
                assert!(root.join(".git/agent-worktrees").is_dir());
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
