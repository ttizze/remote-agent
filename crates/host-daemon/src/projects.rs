use agent_protocol::models::Project;
use anyhow::Context;
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) mod icons;
mod scratch;
pub(crate) mod state;

/// The Host owns project registration independently of native provider catalogs.
#[derive(Debug, Clone)]
pub struct ProjectStore {
    path: PathBuf,
    storage: Arc<crate::host_rpc::storage::Storage>,
}
impl ProjectStore {
    #[cfg(feature = "test-support")]
    pub fn fixture_projects(&self) -> Result<Vec<Project>, String> {
        self.storage.projects()
    }

    #[cfg(feature = "test-support")]
    pub fn replace_fixture_projects(&self, projects: &[Project]) -> Result<(), String> {
        self.storage.replace_projects(projects)
    }

    pub(crate) fn new(
        path: impl Into<PathBuf>,
        storage: Arc<crate::host_rpc::storage::Storage>,
    ) -> Self {
        let path = path.into();
        Self { path, storage }
    }
    #[cfg(test)]
    pub(crate) fn fixture(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let storage = Arc::new(crate::host_rpc::storage::Storage::open(
            &path.with_file_name(crate::host_rpc::storage::CONVERSATION_DATABASE_FILE),
        ));
        Self::new(path, storage)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    async fn read_projects(&self) -> anyhow::Result<Vec<Project>> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.projects())
            .await?
            .map_err(anyhow::Error::msg)
    }
    pub(crate) async fn register(&self, root: &Path) -> anyhow::Result<String> {
        let roots = self.scratch_roots().await?;
        if crate::projects::state::scratch_root(&roots, root).is_some() {
            let root = self.ensure_scratch().await?;
            self.synchronize_projectless().await?;
            return Ok(root.to_str().context("project path is not UTF-8")?.into());
        }
        let root = resolve_root(root).await?;
        let root = dunce::simplified(&root);
        let path = root.to_str().context("project path is not UTF-8")?;
        anyhow::ensure!(
            tokio::fs::metadata(root).await?.is_dir(),
            "project path must be a directory"
        );
        let projects = self.read_projects().await?;
        for registered in projects.iter().flat_map(|project| &project.roots) {
            if registered.path == path
                || resolve_root(Path::new(&registered.path))
                    .await
                    .ok()
                    .is_some_and(|path| dunce::simplified(&path) == root)
            {
                self.synchronize_projectless().await?;
                return Ok(path.into());
            }
        }
        let storage = self.storage.clone();
        let project = Project {
            id: uuid::Uuid::new_v4().to_string(),
            name: root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.into()),
            roots: vec![agent_protocol::models::ProjectRoot { path: path.into() }],
            icon: None,
        };
        tokio::task::spawn_blocking(move || storage.register_project(&project))
            .await?
            .map_err(anyhow::Error::msg)?;
        self.synchronize_projectless().await?;
        Ok(path.into())
    }
    async fn synchronize_projectless(&self) -> anyhow::Result<()> {
        let projects = self.load().await?;
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || storage.assign_projectless_threads(&projects))
            .await?
            .map_err(anyhow::Error::msg)
    }
    pub(crate) async fn load(&self) -> anyhow::Result<state::Snapshot> {
        let scratch_roots = self.scratch_roots().await?;
        let mut snapshot = state::Snapshot {
            scratch_roots,
            ..Default::default()
        };
        for mut project in self.read_projects().await? {
            for root in std::mem::take(&mut project.roots) {
                if let Ok(path) = resolve_root(Path::new(&root.path)).await {
                    let path = dunce::simplified(&path);
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
    async fn registered_projects_match_workspace_aliases_and_their_children() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        let alias = root.join("alias");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let store = ProjectStore::fixture(project.join("projects.json"));
        store.register(&project).await.unwrap();
        let snapshot = store.load().await.unwrap();
        let id = snapshot.projects[0].id.as_str();
        for path in [&project, &alias, &alias.join("src")] {
            assert_eq!(
                snapshot.project_for_workspace(path.to_str().unwrap()),
                Some(id)
            );
            assert_eq!(
                snapshot.project_membership(path.to_str()),
                agent_protocol::models::ProjectMembership::Assigned(id.into())
            );
        }
        assert_eq!(snapshot.project_for_workspace("alias/src"), None);

        let chats = store.create_chat_directory("").await.unwrap();
        let chat_alias = root.join("chat-alias");
        std::os::unix::fs::symlink(chats, &chat_alias).unwrap();
        let snapshot = store.load().await.unwrap();
        assert_eq!(
            snapshot.project_membership(chat_alias.to_str()),
            agent_protocol::models::ProjectMembership::Assigned(
                agent_protocol::models::SCRATCH_PROJECT_ID.into()
            )
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ambiguous_workspace_alias_does_not_select_a_physical_project() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        let alias = root.join("alias");
        std::fs::create_dir(&project).unwrap();
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let store = ProjectStore::fixture(root.join("projects.json"));
        store.register(&project).await.unwrap();
        let mut snapshot = store.load().await.unwrap();
        for id in ["first", "second"] {
            snapshot.projects.push(Project {
                id: id.into(),
                name: id.into(),
                roots: vec![agent_protocol::models::ProjectRoot {
                    path: alias.to_str().unwrap().into(),
                }],
                icon: None,
            });
        }
        assert_eq!(
            snapshot.project_for_workspace(alias.to_str().unwrap()),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_root_keeps_saved_membership_and_other_projects_readable() {
        use agent_protocol::models::ProjectMembership;
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let unavailable = root.join("unavailable");
        std::os::unix::fs::symlink(&unavailable, &unavailable).unwrap();
        assert!(tokio::fs::canonicalize(&unavailable).await.is_err());
        let store = ProjectStore::fixture(root.join("bex-worktrees.json"));
        let projects = serde_json::json!([
            {"id":"offline","name":"Offline","roots":[{"path":unavailable}]},
            {"id":"local","name":"Local","roots":[{"path":root}]}
        ]);
        let projects: Vec<Project> = serde_json::from_value(projects).unwrap();
        store.storage.replace_projects(&projects).unwrap();
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
        let store = ProjectStore::fixture(directory.path().join("bex-worktrees.json"));
        let path = r"\\?\UNC\wsl.localhost\Ubuntu\home\user\project";
        let projects = serde_json::json!([
            {"id":"wsl","name":"WSL project","roots":[{"path":path}]}
        ]);
        let projects: Vec<Project> = serde_json::from_value(projects).unwrap();
        store.storage.replace_projects(&projects).unwrap();
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
        let store = ProjectStore::fixture(directory.path().join("bex-worktrees.json"));
        let connection = rusqlite::Connection::open(
            directory
                .path()
                .join(crate::host_rpc::storage::CONVERSATION_DATABASE_FILE),
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at) VALUES ('broken','Broken','/broken',?1,'2026-11-14T00:00:00.000Z','2026-11-14T00:00:00.000Z')",
                [r#"["invalid record"]"#],
            )
            .unwrap();
        assert!(store.load().await.is_err());
    }

    #[tokio::test]
    async fn scratch_registration_reserves_children_and_survives_catalog_reopen() {
        use agent_protocol::models::{ProjectMembership, SCRATCH_PROJECT_ID};
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
        let store = ProjectStore::fixture(configured.join("bex-worktrees.json"));
        store.register(&root).await.unwrap();
        let scratch = store.scratch_directory();
        assert!(!scratch.exists());
        let child = scratch.join("missing-conversation");
        assert!(
            crate::projects::state::scratch_root(
                &store.load().await.unwrap().scratch_roots,
                &child
            )
            .is_some()
        );
        let registered = store.register(&child).await.unwrap();
        assert_eq!(
            Path::new(&registered),
            dunce::canonicalize(&scratch).unwrap()
        );
        store.register(&scratch).await.unwrap();
        let snapshot = ProjectStore::fixture(store.path()).load().await.unwrap();
        assert_eq!(snapshot.projects.len(), 2);
        for path in [&scratch, &child, &root.join("bex-chat/other")] {
            assert_eq!(
                snapshot.project_membership(path.to_str()),
                ProjectMembership::Assigned(SCRATCH_PROJECT_ID.into())
            );
        }
        assert_ne!(
            snapshot.project_for_workspace(root.join("bex-chat-other").to_str().unwrap()),
            Some(SCRATCH_PROJECT_ID)
        );
    }

    #[tokio::test]
    async fn registration_is_durable_idempotent_and_refreshes_worktree_roots() {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let store = ProjectStore::fixture(root.join("bex-worktrees.json"));
        assert!(store.load().await.unwrap().projects.is_empty());
        store.register(&project).await.unwrap();
        store.register(&project).await.unwrap();
        let reopened = ProjectStore::fixture(store.path());
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
