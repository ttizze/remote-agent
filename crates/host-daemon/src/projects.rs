use agent_protocol::models::Project;
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
        match self.add(root, None).await? {
            Registration::Created(id) | Registration::Existing(id) => Ok(id),
        }
    }
    /// Registers a directory under `name`, or under its directory name.
    pub(crate) async fn add(
        &self,
        root: &Path,
        name: Option<&str>,
    ) -> anyhow::Result<Registration> {
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
            id: id.clone(),
            name: name.map(str::to_owned).unwrap_or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.into())
            }),
            roots: vec![agent_protocol::models::ProjectRoot { path: path.into() }],
        });
        let file = self.path.with_file_name("projects.json");
        let bytes = serde_json::to_vec(&projects)?;
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
        .await??;
        Ok(Registration::Created(id))
    }
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
            store.add(&project, Some("Renamed")).await.unwrap(),
            Registration::Existing(registered)
        );
        let titled = root.join("titled");
        std::fs::create_dir(&titled).unwrap();
        let Registration::Created(id) = store.add(&titled, Some("Pinball Stats")).await.unwrap()
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
}
