use agent_protocol::models::Project;
use anyhow::Context;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) mod icons;
pub(crate) mod state;
pub(crate) mod titles;

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
        let root = resolve_root(root).await?;
        let root = dunce::simplified(&root);
        anyhow::ensure!(
            tokio::fs::metadata(root).await?.is_dir(),
            "project path must be a directory"
        );
        let _registration = self.registration.lock().await;
        let mut projects = self.read_projects().await?;
        let path = root.to_str().context("project path is not UTF-8")?;
        for registered in projects.iter().flat_map(|project| &project.roots) {
            if registered.path == path
                || resolve_root(Path::new(&registered.path))
                    .await
                    .ok()
                    .is_some_and(|path| dunce::simplified(&path) == root)
            {
                return Ok(path.into());
            }
        }
        projects.push(Project {
            list_revision: 0,
            id: uuid::Uuid::new_v4().to_string(),
            name: root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.into()),
            roots: vec![agent_protocol::models::ProjectRoot { path: path.into() }],
            icon: None,
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
        Ok(path.into())
    }
    pub(crate) async fn load(&self) -> anyhow::Result<state::Snapshot> {
        // Admission reserves the configured path before preparation creates it.
        // Keep it alongside the physical path when its parent is a symlink.
        let mut chat_directories = std::collections::BTreeSet::from([self.chat_directory()]);
        match tokio::fs::canonicalize(self.chat_directory()).await {
            Ok(path) => {
                chat_directories.insert(dunce::simplified(&path).to_owned());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut snapshot = state::Snapshot {
            chat_directories,
            ..Default::default()
        };
        for mut project in self.read_projects().await? {
            for root in std::mem::take(&mut project.roots) {
                if let Ok(path) = resolve_root(Path::new(&root.path)).await {
                    let path = dunce::simplified(&path);
                    if snapshot.chat_directories.contains(path) {
                        continue;
                    }
                    snapshot
                        .resolved_roots
                        .insert(root.path.clone(), path.to_owned());
                }
                project.roots.push(root);
            }
            if !project.roots.is_empty() {
                snapshot.projects.push(project);
            }
        }
        snapshot.worktree_roots = crate::worktrees::workspace_roots(&self.path).await?;
        Ok(snapshot)
    }
}

async fn resolve_root(root: &Path) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        root.is_absolute(),
        "プロジェクトのフォルダは絶対パスで指定してください。"
    );
    if let Some(folder) = agent_local_host::wsl::folder(&root.to_string_lossy())? {
        anyhow::bail!(
            "WSL ({}) のプロジェクトは、その WSL Host で開いてください。",
            folder.distro
        );
    }
    Ok(tokio::fs::canonicalize(root).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_root_keeps_saved_membership_and_other_projects_readable() {
        use agent_protocol::models::ProjectMembership;
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let unavailable = root.join("unavailable");
        std::os::unix::fs::symlink(&unavailable, &unavailable).unwrap();
        assert!(tokio::fs::canonicalize(&unavailable).await.is_err());
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
        let projects = serde_json::json!([
            {"id":"offline","name":"Offline","roots":[{"path":unavailable}]},
            {"id":"local","name":"Local","roots":[{"path":root}]}
        ]);
        std::fs::write(
            root.join("bex-projects.json"),
            serde_json::to_vec(&projects).unwrap(),
        )
        .unwrap();
        let snapshot = store.load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 2);
        assert!(
            !snapshot
                .resolved_roots
                .contains_key(unavailable.to_str().unwrap())
        );
        assert_eq!(
            snapshot.project_membership(unavailable.join("src").to_str()),
            ProjectMembership::Assigned("offline".into())
        );
        assert_eq!(
            snapshot.project_membership(root.to_str()),
            ProjectMembership::Assigned("local".into())
        );
        assert!(store.register(&unavailable).await.is_err());
        std::fs::remove_file(&unavailable).unwrap();
        std::fs::create_dir(&unavailable).unwrap();
        assert!(
            store
                .load()
                .await
                .unwrap()
                .resolved_roots
                .contains_key(unavailable.to_str().unwrap())
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn wsl_roots_remain_readable_without_accessing_windows_shares() {
        use agent_protocol::models::ProjectMembership;
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("bex-worktrees.json"));
        let path = r"\\?\UNC\wsl.localhost\Ubuntu\home\user\project";
        let projects = serde_json::json!([
            {"id":"wsl","name":"WSL project","roots":[{"path":path}]}
        ]);
        std::fs::write(
            directory.path().join("bex-projects.json"),
            serde_json::to_vec(&projects).unwrap(),
        )
        .unwrap();
        let snapshot = store.load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert!(snapshot.resolved_roots.is_empty());
        assert_eq!(
            snapshot.project_membership(Some(path)),
            ProjectMembership::Assigned("wsl".into())
        );
        let error = store.register(Path::new(path)).await.unwrap_err();
        assert!(error.to_string().contains("WSL (Ubuntu)"), "{error}");
    }

    #[tokio::test]
    async fn corrupt_project_state_is_still_reported() {
        let directory = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(directory.path().join("bex-worktrees.json"));
        std::fs::write(directory.path().join("bex-projects.json"), b"not json").unwrap();
        assert!(store.load().await.is_err());
    }

    #[tokio::test]
    async fn reserved_chat_directory_is_unassigned_before_and_after_creation() {
        use agent_protocol::models::ProjectMembership;
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        #[cfg(unix)]
        let configured = {
            let alias = root.join("alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let configured = root.clone();
        let store = ProjectStore::new(configured.join("bex-worktrees.json"));
        store.register(&root).await.unwrap();
        let chats = store.chat_directory();
        assert!(!chats.exists());
        for created in [false, true] {
            if created {
                std::fs::create_dir(&chats).unwrap();
            }
            let snapshot = store.load().await.unwrap();
            assert_eq!(
                snapshot.project_membership(chats.to_str()),
                ProjectMembership::Unassigned {}
            );
            assert_eq!(
                snapshot.project_membership(root.to_str()),
                ProjectMembership::Assigned(snapshot.projects[0].id.clone())
            );
            if created {
                assert_eq!(
                    snapshot.project_membership(dunce::canonicalize(&chats).unwrap().to_str()),
                    ProjectMembership::Unassigned {}
                );
            }
        }
    }

    #[tokio::test]
    async fn standalone_chat_directory_does_not_become_a_project() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
        let chats = store.chat_directory();
        std::fs::create_dir(&chats).unwrap();
        store.register(&chats).await.unwrap();
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let projects = serde_json::json!([
            {"id":"chats","name":"Chats","roots":[{"path":chats}]},
            {"id":"mixed","name":"Workspace","roots":[{"path":chats},{"path":workspace}]}
        ]);
        std::fs::write(
            root.join("bex-projects.json"),
            serde_json::to_vec(&projects).unwrap(),
        )
        .unwrap();
        let snapshot = store.load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.projects[0].id, "mixed");
        assert_eq!(snapshot.projects[0].roots.len(), 1);
        assert_eq!(Path::new(&snapshot.projects[0].roots[0].path), workspace);
        assert_eq!(
            snapshot.project_for_workspace(chats.to_str().unwrap()),
            None
        );
        assert!(
            !snapshot
                .resolved_roots
                .contains_key(chats.to_str().unwrap())
        );
    }
    #[tokio::test]
    async fn registration_is_durable_idempotent_and_refreshes_worktree_roots() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::new(root.join("bex-worktrees.json"));
        assert!(store.load().await.unwrap().projects.is_empty());
        store.register(&project).await.unwrap();
        store.register(&project).await.unwrap();
        let reopened = ProjectStore::new(store.path());
        let snapshot = reopened.load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(
            snapshot.project_for_workspace(project.to_str().unwrap()),
            Some(snapshot.projects[0].id.as_str())
        );
        let id = snapshot.projects[0].id.clone();
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
