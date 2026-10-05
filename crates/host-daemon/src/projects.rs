use agent_protocol::models::Project;
use anyhow::Context;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) mod state;
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
        self.path.with_file_name("bex-chats")
    }

    async fn read_projects(&self) -> anyhow::Result<Vec<Project>> {
        match tokio::fs::read(self.path.with_file_name("bex-projects.json")).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }
    pub(crate) async fn register(&self, root: &Path) -> anyhow::Result<String> {
        anyhow::ensure!(root.is_absolute(), "project directory must be absolute");
        let root = tokio::fs::canonicalize(root).await?;
        let root = dunce::simplified(&root);
        anyhow::ensure!(
            tokio::fs::metadata(root).await?.is_dir(),
            "project path must be a directory"
        );
        let _registration = self.registration.lock().await;
        let mut projects = self.read_projects().await?;
        let path = root.to_str().context("project path is not UTF-8")?;
        for project in &projects {
            for registered in &project.roots {
                if registered.path == path
                    || tokio::fs::canonicalize(&registered.path)
                        .await
                        .ok()
                        .is_some_and(|path| dunce::simplified(&path) == root)
                {
                    return Ok(project.id.clone());
                }
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        projects.push(Project {
            id: id.clone(),
            name: root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.into()),
            roots: vec![agent_protocol::models::ProjectRoot { path: path.into() }],
        });
        let file = self.path.with_file_name("bex-projects.json");
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
        Ok(id)
    }
    pub(crate) async fn load(&self) -> anyhow::Result<state::Snapshot> {
        let mut snapshot = state::Snapshot {
            projects: self.read_projects().await?,
            ..Default::default()
        };
        for root in snapshot.projects.iter().flat_map(|project| &project.roots) {
            if !Path::new(&root.path).is_absolute() {
                continue;
            }
            match tokio::fs::canonicalize(&root.path).await {
                Ok(path) => {
                    snapshot
                        .resolved_roots
                        .insert(root.path.clone(), dunce::simplified(&path).to_owned());
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        snapshot.worktree_roots = crate::worktrees::workspace_roots(&self.path).await?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn registration_is_durable_idempotent_and_refreshes_worktree_roots() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
        assert!(store.load().await.unwrap().projects.is_empty());
        let registered_id = store.register(&project).await.unwrap();
        assert_eq!(store.register(&project).await.unwrap(), registered_id);
        let reopened = ProjectStore::new(store.path());
        let snapshot = reopened.load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(
            snapshot.project_for_workspace(project.to_str().unwrap()),
            Some(snapshot.projects[0].id.as_str())
        );
        let id = snapshot.projects[0].id.clone();
        assert_eq!(registered_id, id);
        assert_ne!(id, project.to_str().unwrap());
        let checkout = root.join("checkout");
        std::fs::write(
            store.path(),
            serde_json::to_vec(
                &serde_json::json!({"workspaceRoots":{checkout.to_str().unwrap():project}}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reopened
                .load()
                .await
                .unwrap()
                .project_for_workspace(checkout.to_str().unwrap()),
            Some(id.as_str())
        );
        std::fs::remove_file(store.path()).unwrap();
        assert_eq!(
            reopened
                .load()
                .await
                .unwrap()
                .project_for_workspace(checkout.to_str().unwrap()),
            None
        );
    }
}
