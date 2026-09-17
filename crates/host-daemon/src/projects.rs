use agent_core::models::Project;
use anyhow::Context;
use std::{
    env, io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) mod state;
pub(crate) mod titles;
/// Host workspace metadata plus the last native project catalog, retained while
/// Codex is unavailable so Claude sessions can still be grouped.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    path: PathBuf,
    projects: Arc<tokio::sync::Mutex<Vec<Project>>>,
}
impl ProjectStore {
    pub fn from_environment() -> anyhow::Result<Self> {
        let directory = env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")))
            .context("neither CODEX_HOME nor the user home directory is available")?;
        Ok(Self::new(directory.join("bex-worktrees.json")))
    }
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            projects: Arc::default(),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn chat_directory(&self) -> PathBuf {
        self.path.with_file_name("bex-chats")
    }

    pub(crate) async fn load(
        &self,
        native_projects: Option<Vec<Project>>,
    ) -> anyhow::Result<state::Snapshot> {
        let mut cached = self.projects.lock().await;
        if let Some(projects) = native_projects {
            *cached = projects;
        }
        let mut snapshot = state::Snapshot {
            projects: cached.clone(),
            ..Default::default()
        };
        drop(cached);
        for root in snapshot.projects.iter().flat_map(|project| &project.roots) {
            if !Path::new(&root.path).is_absolute() {
                continue;
            }
            match tokio::fs::canonicalize(&root.path).await {
                Ok(path) => {
                    snapshot.resolved_roots.insert(root.path.clone(), path);
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
        snapshot.chat_directory = match tokio::fs::canonicalize(self.chat_directory()).await {
            Ok(path) => Some(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_catalog_cache_keeps_claude_groups_and_refreshes_worktree_roots() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
        let projects = serde_json::from_value(serde_json::json!([
            {"id":"native","name":"Project","roots":[{"path":project}]}
        ]))
        .unwrap();
        assert_eq!(store.load(Some(projects)).await.unwrap().projects.len(), 1);
        assert_eq!(store.clone().load(None).await.unwrap().projects.len(), 1);
        #[cfg(unix)]
        {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&project, &alias).unwrap();
            let projects = serde_json::from_value(serde_json::json!([
                {"id":"native","name":"Project","roots":[{"path":alias}]}
            ]))
            .unwrap();
            assert!(store.load(Some(projects)).await.unwrap().has_root(&project));
        }
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
            store
                .load(None)
                .await
                .unwrap()
                .project_for_workspace(checkout.to_str().unwrap()),
            Some("native")
        );
        std::fs::remove_file(store.path()).unwrap();
        assert_eq!(
            store
                .load(None)
                .await
                .unwrap()
                .project_for_workspace(checkout.to_str().unwrap()),
            None
        );
        assert!(
            store
                .load(Some(Vec::new()))
                .await
                .unwrap()
                .projects
                .is_empty()
        );
    }
}
