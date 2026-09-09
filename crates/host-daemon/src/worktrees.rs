use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
};

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    create_on_new_session: bool,
    copy_on_create: bool,
    copy_paths: Vec<String>,
    worktree_directory: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct State {
    settings: Settings,
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

    pub(crate) async fn settings(&self, update: Option<Value>) -> Result<Value, String> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            if let Some(update) = update {
                let settings: Settings = serde_json::from_value(update).map_err(|e| e.to_string())?;
                for entry in &settings.copy_paths { relative_path(entry)?; }
                if !settings.worktree_directory.is_empty() && !Path::new(&settings.worktree_directory).is_absolute() {
                    return Err("worktree directory must be an absolute path on the Host, or empty for the default".into());
                }
                state.settings = settings;
                save(&path, &state)?;
            }
            serde_json::to_value(state.settings).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    pub(crate) async fn prepare(&self, cwd: Option<&str>) -> Result<Option<PathBuf>, String> {
        let _guard = self.lock.lock().await;
        let path = self.path.clone();
        let cwd = cwd.map(PathBuf::from);
        tokio::task::spawn_blocking(move || {
            let mut state = read(&path)?;
            if !state.settings.create_on_new_session {
                return Ok(None);
            }
            let cwd = cwd
                .ok_or("worktree creation requires a working directory")?
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let root = PathBuf::from(git(&cwd, &["rev-parse", "--show-toplevel"])?)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let relative_cwd = cwd.strip_prefix(&root).map_err(|e| e.to_string())?;
            let parent = if state.settings.worktree_directory.is_empty() {
                PathBuf::from(git(
                    &root,
                    &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                )?)
                .join("bex-worktrees")
            } else {
                PathBuf::from(&state.settings.worktree_directory)
            };
            fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
            // Resolve aliases such as /tmp before checking copy destination ancestors.
            let parent = parent.canonicalize().map_err(|e| e.to_string())?;
            let destination = crate::platform::worktree_directory(&parent)
                .map_err(|e| e.to_string())?
                .keep();
            let branch = format!("bex/{}", destination.file_name().unwrap().to_string_lossy());
            let destination_text = destination.to_str().ok_or("worktree path is not UTF-8")?;
            if let Err(error) = git(
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
                            Err(e) => return Err(e.to_string()),
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
                    return Err("working directory does not exist in HEAD".into());
                }
                let original = state
                    .workspace_roots
                    .get(root.to_str().ok_or("project path is not UTF-8")?)
                    .cloned()
                    .unwrap_or_else(|| root.to_string_lossy().into_owned());
                state
                    .workspace_roots
                    .insert(destination_text.to_owned(), original);
                save(&path, &state)?;
                Ok(target)
            })();
            match prepared {
                Ok(target) => Ok(Some(target)),
                Err(error) => {
                    let cleanup = git(&root, &["worktree", "remove", "--force", destination_text])
                        .and_then(|_| git(&root, &["branch", "-D", &branch]));
                    match cleanup {
                        Ok(_) => Err(error),
                        Err(cleanup) => Err(format!("{error}; worktree cleanup failed: {cleanup}")),
                    }
                }
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

pub(crate) async fn workspace_roots(
    project_state: &Path,
) -> Result<HashMap<String, String>, String> {
    let path = project_state.with_file_name("bex-worktrees.json");
    tokio::task::spawn_blocking(move || read(&path).map(|state| state.workspace_roots))
        .await
        .map_err(|e| e.to_string())?
}

fn read(path: &Path) -> Result<State, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.to_string()),
    }
}

fn save(path: &Path, state: &State) -> Result<(), String> {
    let parent = path.parent().ok_or("settings have no parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write_with_options(
            |file| {
                serde_json::to_writer(&mut *file, state).map_err(std::io::Error::other)?;
                file.write_all(b"\n")
            },
            crate::platform::private_file_options(),
        )
        .map_err(|e| e.to_string())
}

fn relative_path(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if value.is_empty()
        || !path
            .components()
            .all(|part| matches!(part, Component::Normal(name) if name != ".git"))
    {
        return Err("copy paths must be relative paths without '..' or '.git'".into());
    }
    Ok(path)
}

fn no_symlinks(root: &Path, relative: &Path) -> Result<(), String> {
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("copy paths cannot contain symbolic links".into());
        }
    }
    Ok(())
}

fn copy(source: &Path, target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|e| e.to_string())?;
    if let Some(parent) = target.parent() {
        for ancestor in parent.ancestors() {
            if let Ok(meta) = fs::symlink_metadata(ancestor)
                && meta.file_type().is_symlink()
            {
                return Err("copy destination contains a symbolic link".into());
            }
        }
    }
    if metadata.is_dir() {
        match fs::symlink_metadata(target) {
            Ok(meta) if !meta.is_dir() => return Err("copy destination is not a directory".into()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(target).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        }
        for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() == ".git" {
                return Err("cannot copy Git metadata".into());
            }
            copy(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        let parent = target.parent().ok_or("copy destination has no parent")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        match fs::symlink_metadata(target) {
            Ok(meta) if !meta.is_file() => {
                return Err("copy destination is not a regular file".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        // Configured files may replace HEAD's version in this fresh checkout.
        // An atomic replacement also avoids exposing partially copied secrets.
        let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        std::io::copy(
            &mut fs::File::open(source).map_err(|e| e.to_string())?,
            &mut output,
        )
        .map_err(|e| e.to_string())?;
        output
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|e| e.to_string())?;
        output.persist(target).map_err(|e| e.error.to_string())?;
    } else {
        return Err("only regular files and directories can be copied".into());
    }
    Ok(())
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().into());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim_end().to_owned())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--quiet"]).unwrap();
        fs::write(root.join("tracked.txt"), "committed\n").unwrap();
        fs::write(root.join("config.txt"), "default\n").unwrap();
        fs::write(root.join(".gitignore"), ".env\nlocal/\n").unwrap();
        git(root, &["add", "."]).unwrap();
        git(
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
    async fn settings_persist_and_toggles_control_real_worktree_creation_and_copying() {
        let directory = repository();
        let root = directory.path().canonicalize().unwrap();
        let projects = root.join("projects.json");
        let store = Worktrees::new(&projects);
        assert!(store.prepare(None).await.unwrap().is_none());
        fs::write(root.join("tracked.txt"), "uncommitted\n").unwrap();
        fs::write(root.join("config.txt"), "local config\n").unwrap();
        fs::write(root.join(".env"), "FIXTURE_TOKEN=isolated\n").unwrap();
        fs::set_permissions(root.join(".env"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(root.join("local")).unwrap();
        fs::write(root.join("local/value"), "local data").unwrap();
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env","local","missing","config.txt"],"worktreeDirectory":""});
        store.settings(Some(settings.clone())).await.unwrap();
        let loaded = Worktrees::new(&projects);
        assert_eq!(loaded.settings(None).await.unwrap(), settings);
        let first = loaded.prepare(root.to_str()).await.unwrap().unwrap();
        assert_eq!(first.parent().unwrap(), root.join(".git/bex-worktrees"));
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
            git(&first, &["branch", "--show-current"])
                .unwrap()
                .starts_with("bex/session-")
        );
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            workspace_roots(&projects).await.unwrap()[first.to_str().unwrap()],
            root.to_str().unwrap()
        );
        store
            .settings(Some(
                json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"]}),
            ))
            .await
            .unwrap();
        let second = store.prepare(root.to_str()).await.unwrap().unwrap();
        assert_ne!(first, second);
        assert!(!second.join(".env").exists());
        store
            .settings(Some(
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
        let before = git(root, &["worktree", "list", "--porcelain"]).unwrap();
        let branches = git(root, &["branch", "--list"]).unwrap();
        for path in ["link", "nested/value"] {
            if path == "link" {
                symlink(root.join("tracked.txt"), root.join("link")).unwrap();
            }
            if path == "nested/value" {
                fs::create_dir(root.join("local")).unwrap();
                fs::write(root.join("local/value"), "fixture").unwrap();
                symlink(root.join("local"), root.join("nested")).unwrap();
            }
            store
                .settings(Some(
                    json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[path]}),
                ))
                .await
                .unwrap();
            assert!(
                store.prepare(root.to_str()).await.is_err(),
                "{path} must be rejected"
            );
            assert_eq!(
                git(root, &["worktree", "list", "--porcelain"]).unwrap(),
                before
            );
            assert_eq!(git(root, &["branch", "--list"]).unwrap(), branches);
        }
    }

    #[tokio::test]
    async fn invalid_settings_never_replace_saved_preferences() {
        let directory = repository();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        let valid = json!({"createOnNewSession":true,"copyOnCreate":false,"copyPaths":[".env"],"worktreeDirectory":""});
        store.settings(Some(valid.clone())).await.unwrap();
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
                    .settings(Some(json!({"copyPaths":[path]})))
                    .await
                    .is_err()
            );
            assert_eq!(store.settings(None).await.unwrap(), valid);
        }
        assert!(
            store
                .settings(Some(json!({"createOnNewSession":"yes"})))
                .await
                .is_err()
        );
        for path in ["relative/worktrees", "~/worktrees", "../worktrees"] {
            assert!(
                store
                    .settings(Some(json!({"worktreeDirectory":path})))
                    .await
                    .is_err()
            );
            assert_eq!(store.settings(None).await.unwrap(), valid);
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
        store.settings(Some(settings.clone())).await.unwrap();
        let restarted = Worktrees::new(&projects);
        let first = restarted.prepare(root.to_str()).await.unwrap().unwrap();
        assert_eq!(first.parent().unwrap(), real_parent.join("new folder"));
        assert_eq!(
            fs::read(first.join(".env")).unwrap(),
            fs::read(root.join(".env")).unwrap()
        );
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        settings["worktreeDirectory"] = json!(real_parent.join("second"));
        store.settings(Some(settings)).await.unwrap();
        let second = restarted.prepare(first.to_str()).await.unwrap().unwrap();
        assert_eq!(second.parent().unwrap(), real_parent.join("second"));
        assert!(first.join(".env").is_file());
        let roots = workspace_roots(&projects).await.unwrap();
        assert_eq!(roots[first.to_str().unwrap()], root.to_str().unwrap());
        assert_eq!(roots[second.to_str().unwrap()], root.to_str().unwrap());
    }

    #[tokio::test]
    async fn non_repository_and_unborn_head_fail_without_starting_a_session() {
        let directory = tempfile::tempdir().unwrap();
        let store = Worktrees::new(&directory.path().join("projects.json"));
        store
            .settings(Some(json!({"createOnNewSession":true})))
            .await
            .unwrap();
        assert!(store.prepare(directory.path().to_str()).await.is_err());
        git(directory.path(), &["init", "--quiet"]).unwrap();
        assert!(store.prepare(directory.path().to_str()).await.is_err());
    }
}
