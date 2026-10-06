use agent_protocol::models::{Project, ProjectScript};
use anyhow::Context;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Whether a registration added the workspace or found it registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Registration {
    Created(String),
    Existing(String),
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

    pub(crate) async fn load(&self) -> anyhow::Result<Vec<Project>> {
        match tokio::fs::read(self.path.with_file_name("projects.json")).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
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
        projects.push(Project {
            repository_identity: None,
            id: id.clone(),
            name: name.map(str::to_owned).unwrap_or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.into())
            }),
            roots: vec![agent_protocol::models::ProjectRoot { path: path.into() }],
            scripts,
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
        rootless: bool,
    ) -> anyhow::Result<()> {
        let scripts = scripts.map(valid_scripts).transpose()?;
        let _registration = self.registration.lock().await;
        let mut projects = self.load().await?;
        let index = match projects.iter().position(|project| project.id == id) {
            Some(index) => index,
            None if rootless => {
                projects.push(Project {
                    repository_identity: None,
                    id: id.into(),
                    ..Project::default()
                });
                projects.len() - 1
            }
            None => anyhow::bail!("project {id} is not registered"),
        };
        if let Some(scripts) = scripts {
            projects[index].scripts = scripts;
        }
        self.save(&projects).await
    }
    async fn save(&self, projects: &[Project]) -> anyhow::Result<()> {
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
            .update(&id, Some(vec![script(" vp install ")]), false)
            .await
            .unwrap();
        store.update(&id, None, false).await.unwrap();
        let scripts = &store.load().await.unwrap()[0].scripts;
        assert_eq!(
            (
                scripts[0].id.as_str(),
                scripts[0].name.as_str(),
                scripts[0].command.as_str()
            ),
            ("setup", "Setup", "vp install")
        );
        assert!(
            store
                .update(&id, Some(vec![script(" ")]), false)
                .await
                .is_err()
        );
        assert!(store.update("missing", Some(vec![]), false).await.is_err());
        assert_eq!(store.load().await.unwrap()[0].scripts.len(), 1);

        // An unregistered project keeps its settings in an entry without roots.
        store
            .update("chats", Some(vec![script("vp install")]), true)
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
