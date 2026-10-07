use agent_domain::Timestamp;
use agent_protocol::models::{ProjectRoot, ProjectScript};
use anyhow::Context;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

mod named;
pub(crate) use named::{Git, NamedProjectError, create_named_project};

/// Whether a registration added the workspace or found it registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Registration {
    Created(String),
    Existing(String),
}

/// One entry of `projects.json`. A file in any other shape is refused.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StoredProject {
    pub(crate) id: String,
    pub(crate) name: String,
    /// Empty for a project that is not registered and keeps only its settings.
    pub(crate) roots: Vec<ProjectRoot>,
    pub(crate) scripts: Vec<ProjectScript>,
    /// The icon file the user chose.
    #[serde(deserialize_with = "serde::Deserialize::deserialize")]
    pub(crate) favicon_path: Option<String>,
    pub(crate) created_at: Timestamp,
    pub(crate) updated_at: Timestamp,
}

/// The Host owns project registration independently of native provider catalogs.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    path: PathBuf,
    registration: Arc<tokio::sync::Mutex<()>>,
}
impl ProjectStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            registration: Arc::default(),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn chat_directory(&self) -> PathBuf {
        self.path.with_file_name("chats")
    }
    /// The folder that holds projects started from just a name, beside the chats
    /// and away from folders the user organizes by hand.
    pub(crate) fn named_project_directory(&self) -> PathBuf {
        self.path.with_file_name("projects")
    }

    pub(crate) async fn load(&self) -> anyhow::Result<Vec<StoredProject>> {
        let file = self.path.with_file_name("projects.json");
        match tokio::fs::read(&file).await {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!("{} is not in the current project format", file.display())
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) async fn register(&self, root: &Path) -> anyhow::Result<String> {
        match self.add(root, None, vec![]).await? {
            Registration::Created(id) | Registration::Existing(id) => Ok(id),
        }
    }
    /// Registers a directory under `name`, or under its directory name.
    pub(crate) async fn add(
        &self,
        root: &Path,
        name: Option<&str>,
        scripts: Vec<ProjectScript>,
    ) -> anyhow::Result<Registration> {
        let scripts = valid_scripts(scripts)?;
        anyhow::ensure!(root.is_absolute(), "project directory must be absolute");
        let root = tokio::fs::canonicalize(root).await?;
        let root = dunce::simplified(&root);
        anyhow::ensure!(
            tokio::fs::metadata(root).await?.is_dir(),
            "project path must be a directory"
        );
        let _registration = self.registration.lock().await;
        let mut projects = self.load().await?;
        let path = root.to_str().context("project path is not UTF-8")?;
        for project in &projects {
            for registered in &project.roots {
                if registered.path == path
                    || tokio::fs::canonicalize(&registered.path)
                        .await
                        .ok()
                        .is_some_and(|path| dunce::simplified(&path) == root)
                {
                    return Ok(Registration::Existing(project.id.clone()));
                }
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = now();
        projects.push(StoredProject {
            created_at: now.clone(),
            updated_at: now,
            id: id.clone(),
            name: name.map(str::to_owned).unwrap_or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.into())
            }),
            roots: vec![ProjectRoot { path: path.into() }],
            scripts,
            favicon_path: None,
        });
        self.save(&projects).await?;
        Ok(Registration::Created(id))
    }
    /// Applies a project update. A `rootless` project, which is not registered,
    /// keeps its settings in an entry without roots.
    pub(crate) async fn update(
        &self,
        id: &str,
        scripts: Option<Vec<ProjectScript>>,
        favicon_path: Option<Option<String>>,
        rootless: bool,
    ) -> anyhow::Result<()> {
        let scripts = scripts.map(valid_scripts).transpose()?;
        let favicon_path = favicon_path
            .map(|path| {
                path.map(|path| agent_protocol::models::project_favicon_path(&path))
                    .transpose()
            })
            .transpose()
            .map_err(anyhow::Error::msg)?;
        let _registration = self.registration.lock().await;
        let mut projects = self.load().await?;
        let index = match projects.iter().position(|project| project.id == id) {
            Some(index) => index,
            None if rootless => {
                let now = now();
                projects.push(StoredProject {
                    id: id.into(),
                    name: String::new(),
                    roots: vec![],
                    scripts: vec![],
                    favicon_path: None,
                    created_at: now.clone(),
                    updated_at: now,
                });
                projects.len() - 1
            }
            None => anyhow::bail!("project {id} is not registered"),
        };
        let changed = scripts.is_some() || favicon_path.is_some();
        if let Some(scripts) = scripts {
            projects[index].scripts = scripts;
        }
        if let Some(favicon_path) = favicon_path {
            projects[index].favicon_path = favicon_path;
        }
        if changed {
            projects[index].updated_at = now();
        }
        self.save(&projects).await
    }
    async fn save(&self, projects: &[StoredProject]) -> anyhow::Result<()> {
        let file = self.path.with_file_name("projects.json");
        let bytes = serde_json::to_vec(projects)?;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomicwrites::AtomicFile::new(&file, atomicwrites::AllowOverwrite).write(|f| {
                use std::io::Write;
                f.write_all(&bytes)
            })?;
            Ok(())
        })
        .await?
    }
}

fn now() -> Timestamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64);
    Timestamp::from_millis(millis).expect("the clock is within range")
}

/// `~`, `~/…` and `~\…` name the home directory.
pub(crate) fn expand_home(path: &str) -> PathBuf {
    match directories::BaseDirs::new() {
        Some(dirs) => expand_home_in(path, dirs.home_dir()),
        None => PathBuf::from(path),
    }
}
fn expand_home_in(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_owned();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => normalize_lexically(Path::new(&format!(
            "{}{}{rest}",
            home.display(),
            std::path::MAIN_SEPARATOR
        ))),
        None => PathBuf::from(path),
    }
}

/// `path` with `.` and `..` folded away without touching the filesystem; `..`
/// stops at the root.
pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) =>
            {
                normalized.pop();
            }
            Component::ParentDir if normalized.has_root() => {}
            part => normalized.push(part.as_os_str()),
        }
    }
    normalized
}

/// Trims the scripts' text fields and rejects empty ones.
pub(crate) fn valid_scripts(scripts: Vec<ProjectScript>) -> anyhow::Result<Vec<ProjectScript>> {
    let required = |value: String, field: &str| {
        let value = value.trim();
        anyhow::ensure!(
            !value.is_empty(),
            "project script {field} must not be empty"
        );
        Ok(value.to_owned())
    };
    scripts
        .into_iter()
        .map(|script| {
            Ok(ProjectScript {
                id: required(script.id, "id")?,
                name: required(script.name, "name")?,
                command: required(script.command, "command")?,
                preview_url: script
                    .preview_url
                    .map(|url| required(url, "preview URL"))
                    .transpose()?,
                ..script
            })
        })
        .collect()
}

/// Words of a thread's first message for its folder name. Only [a-z0-9] reaches a
/// folder name, so it stays one path segment, and the words are capped so a pasted
/// blob cannot outgrow a file name.
fn folder_words(text: &str) -> String {
    let text = text.to_lowercase();
    let words = text
        .split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|word| !word.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    words[..words.len().min(48)]
        .trim_end_matches('-')
        .to_owned()
}

/// Claims a fresh folder of its own for a chat thread under `root`, named from the
/// UTC `date`, its first message and its id. Each folder is created without its
/// parents, so creating it is the claim.
pub(crate) fn claim_thread_folder(
    root: &Path,
    thread: &str,
    text: &str,
    date: &str,
) -> io::Result<PathBuf> {
    crate::platform::create_state_directory(root)?;
    let words = folder_words(text);
    let id: String = thread
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    let folder_for = |id_part: &str| {
        root.join(
            [date, words.as_str(), id_part]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-"),
        )
    };
    let full = folder_for(&id);
    // Thread ids often share a prefix, so the first name uses the id's tail. A taken
    // short name falls back to the full id, then to fresh random suffixes.
    for attempt in 1.. {
        let folder = match attempt {
            1 => folder_for(&id[id.len().saturating_sub(8)..]),
            2 => full.clone(),
            _ => {
                let mut name = full.clone().into_os_string();
                name.push(format!("-{}", &uuid::Uuid::new_v4().to_string()[..8]));
                PathBuf::from(name)
            }
        };
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    unreachable!("folder names never run out")
}

#[cfg(test)]
mod tests {
    use super::*;
    // pathExpansion.test.ts expandHomePath: `~` alone, `~/` and `~\` only, joined
    // like a path.
    #[test]
    fn a_leading_tilde_names_the_home_directory() {
        let home = directories::BaseDirs::new().unwrap().home_dir().to_owned();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/.codex-work"), home.join(".codex-work"));
        assert_eq!(expand_home("~\\.codex"), home.join(".codex"));
        for unchanged in [
            "",
            "/absolute/path",
            "relative/path",
            "some~weird~path",
            "~alice/foo",
            "/abs/~",
        ] {
            assert_eq!(expand_home(unchanged), PathBuf::from(unchanged));
        }
        let home = Path::new("/home/me");
        assert_eq!(expand_home_in("~/", home), home);
        assert_eq!(expand_home_in("~/a/./b/../c", home), home.join("a/c"));
        assert_eq!(expand_home_in("~//etc", home), home.join("etc"));
        assert_eq!(expand_home_in("~/../other", home), Path::new("/home/other"));
    }

    // WorkspacePaths.ts normalizeWorkspaceRoot: an added project's `~` names the
    // home directory.
    #[tokio::test]
    async fn a_project_added_at_the_tilde_is_the_home_directory() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("worktrees.json"));
        let home = dunce::canonicalize(directories::BaseDirs::new().unwrap().home_dir()).unwrap();
        let id = store.register(&expand_home(" ~ ".trim())).await.unwrap();
        assert_eq!(store.register(&expand_home("~/")).await.unwrap(), id);
        assert_eq!(
            store.load().await.unwrap()[0].roots[0].path,
            home.to_str().unwrap()
        );
    }

    #[test]
    fn paths_fold_dots_without_the_filesystem() {
        for (path, normalized) in [
            ("/a/./b/../c/", "/a/c"),
            ("/../a", "/a"),
            ("a/../../b", "../b"),
            ("./a", "a"),
        ] {
            assert_eq!(normalize_lexically(Path::new(path)), Path::new(normalized));
        }
    }

    #[tokio::test]
    async fn registration_is_durable_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::new(root.join("worktrees.json"));
        assert!(store.load().await.unwrap().is_empty());
        let registered = store.register(&project).await.unwrap();
        assert_eq!(store.register(&project).await.unwrap(), registered);
        let projects = ProjectStore::new(store.path()).load().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].id, registered);
        assert_eq!(projects[0].name, "project");
        assert_eq!(projects[0].roots[0].path, project.to_str().unwrap());
        assert_eq!(projects[0].created_at, projects[0].updated_at);
        assert!(store.register(Path::new("relative")).await.is_err());
        assert_eq!(
            store.add(&project, Some("Renamed"), vec![]).await.unwrap(),
            Registration::Existing(registered)
        );
        let titled = root.join("titled");
        std::fs::create_dir(&titled).unwrap();
        let Registration::Created(id) = store
            .add(&titled, Some("Pinball Stats"), vec![])
            .await
            .unwrap()
        else {
            panic!("a new workspace is created");
        };
        let projects = store.load().await.unwrap();
        assert_eq!(
            projects
                .iter()
                .find(|project| project.id == id)
                .unwrap()
                .name,
            "Pinball Stats"
        );
    }

    // Only the current format loads; an entry without times or with fields the
    // Host never stores is refused rather than read as another format.
    #[tokio::test]
    async fn a_projects_file_in_another_format_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("worktrees.json"));
        let file = directory.path().join("projects.json");
        let entry =
            r#"{"id":"p","name":"p","roots":[{"path":"/p"}],"scripts":[],"faviconPath":null"#;
        let times =
            r#""createdAt":"2026-10-07T00:00:00.000Z","updatedAt":"2026-10-07T00:00:00.000Z""#;
        for contents in [
            format!("[{entry}}}]"),
            format!(r#"[{entry},{times},"repositoryIdentity":null}}]"#),
            format!(r#"[{{"id":"p","name":"p","roots":[],"scripts":[],{times}}}]"#),
        ] {
            std::fs::write(&file, contents).unwrap();
            let error = store.load().await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .ends_with("is not in the current project format"),
                "{error:#}"
            );
        }
        std::fs::write(&file, format!(r#"[{entry},{times}}}]"#)).unwrap();
        assert_eq!(store.load().await.unwrap()[0].id, "p");
    }

    #[tokio::test]
    async fn updates_store_trimmed_scripts_and_keep_omitted_fields() {
        use agent_protocol::models::ProjectScriptIcon;
        let script = |command: &str| ProjectScript {
            id: " setup ".into(),
            name: " Setup ".into(),
            command: command.into(),
            icon: ProjectScriptIcon::Configure,
            run_on_worktree_create: true,
            run_async: Some(false),
            preview_url: None,
            auto_open_preview: None,
        };
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::new(root.join("worktrees.json"));
        let id = store.register(&project).await.unwrap();

        store
            .update(&id, Some(vec![script(" vp install ")]), None, false)
            .await
            .unwrap();
        store
            .update(&id, None, Some(Some(" brand/logo.svg ".into())), false)
            .await
            .unwrap();
        store.update(&id, None, None, false).await.unwrap();
        let stored = &store.load().await.unwrap()[0];
        let scripts = &stored.scripts;
        assert_eq!(
            (
                scripts[0].id.as_str(),
                scripts[0].name.as_str(),
                scripts[0].command.as_str()
            ),
            ("setup", "Setup", "vp install")
        );
        assert_eq!(stored.favicon_path.as_deref(), Some("brand/logo.svg"));
        assert!(
            store
                .update(&id, Some(vec![script(" ")]), None, false)
                .await
                .is_err()
        );
        for invalid in ["", "notes.txt"] {
            assert!(
                store
                    .update(&id, None, Some(Some(invalid.into())), false)
                    .await
                    .is_err()
            );
        }
        assert!(
            store
                .update("missing", Some(vec![]), None, false)
                .await
                .is_err()
        );
        let stored = &store.load().await.unwrap()[0];
        assert_eq!(stored.scripts.len(), 1);
        assert_eq!(stored.favicon_path.as_deref(), Some("brand/logo.svg"));
        store.update(&id, None, Some(None), false).await.unwrap();
        assert_eq!(store.load().await.unwrap()[0].favicon_path, None);

        // An unregistered project keeps its settings in an entry without roots.
        store
            .update("chats", Some(vec![script("vp install")]), None, true)
            .await
            .unwrap();
        let projects = store.load().await.unwrap();
        assert_eq!(projects.len(), 2);
        assert!(projects[1].roots.is_empty());
        assert_eq!(projects[1].scripts[0].command, "vp install");
    }

    const DATE: &str = "2026-10-06";

    // ManagedProjectFolders.test.ts "gives each Scratch thread its own folder, named
    // from its message".
    #[test]
    fn gives_each_chat_thread_its_own_folder_named_from_its_message() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("chats");
        let text = "Convert these PNGs to WebP, please!";
        // Two ids with the same tail would collide on the short name.
        let first = claim_thread_folder(&root, "thread:a:0123456789abcdef", text, DATE).unwrap();
        let second = claim_thread_folder(&root, "thread:b:0123456789abcdef", text, DATE).unwrap();
        // Ids that normalize to the same characters take both of its names.
        let third = claim_thread_folder(&root, "thread:b0123456789abcdef", text, DATE).unwrap();

        let unique: std::collections::HashSet<_> = [&first, &second, &third].into_iter().collect();
        assert_eq!(unique.len(), 3);
        for folder in [&first, &second, &third] {
            assert_eq!(folder.parent(), Some(root.as_path()));
            let name = folder.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with("2026-10-06-convert-these-pngs-to-webp-"),
                "{name}"
            );
            assert!(folder.is_dir());
        }

        let pasted_text = format!("{}../../etc", "word ".repeat(10_000));
        let pasted = claim_thread_folder(&root, "thread-pasted", &pasted_text, DATE).unwrap();
        assert_eq!(pasted.parent(), Some(root.as_path()));
        assert!(pasted.file_name().unwrap().len() <= 80);
    }

    // "recreates the Scratch folder after it is deleted" (the thread folder part).
    #[test]
    fn claims_a_folder_after_the_chats_folder_is_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("chats");
        std::fs::create_dir(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let folder =
            claim_thread_folder(&root, "thread-after-delete", "Still works", DATE).unwrap();
        assert!(folder.is_dir());
    }
}
