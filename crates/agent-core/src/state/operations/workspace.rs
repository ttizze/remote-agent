use super::*;
use crate::client::ClientExt;

pub use agent_protocol::operations::ListFiles;
pub use agent_protocol::operations::ResolveFile;

impl Operation for ResolveFile {
    rpc_operation!();
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::ResolvedFile {
            entry: output.clone(),
        }
    }
}

impl Operation for ListFiles {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Directory {
            pane: String::new(),
        })
    }
    rpc_operation!();
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Directory {
            listing: output.clone(),
        }
    }
    fn prepare(&mut self, _: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if !agent_protocol::paths::is_absolute_host_path(&self.path)
            && self.path != "~"
            && !self.path.starts_with("~/")
        {
            return Err("絶対パスを入力してください。".into());
        }
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFile {
    pub path: String,
    pub discard_draft: bool,
}
impl rpc::RpcMethod for ReadFile {
    agent_protocol::operations::rpc_contract!(ReadFile);
    fn params(
        &self,
    ) -> Result<<Self::Contract as crate::protocol::contracts::Contract>::Params, PeerError> {
        Ok(ListFiles {
            path: self.path.clone(),
        })
    }
}

impl Operation for ReadFile {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::File {
            pane: String::new(),
        })
    }
    no_input!();
    type Output = <Self as rpc::RpcMethod>::Output;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.call(self).await
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, file: Self::Output) -> Vec<Effect> {
        if let Some(pane) = snapshot.panes.get_mut(pane_id) {
            Arc::make_mut(&mut Arc::make_mut(pane).workspace).file = Some(Arc::new(file));
        }
        Vec::new()
    }
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::FileRead {
            file: output.clone(),
        }
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if self.discard_draft {
            Arc::make_mut(&mut snapshot.files.file_drafts).remove(&self.path);
        }
        Ok(())
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub path: String,
}
impl Operation for SaveFile {
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::SaveFile {
            path: self.path.clone(),
        })
    }
    type Input = Arc<FileDraft>;
    fn capture(&self, snapshot: &Snapshot, pane_id: &str) -> Result<Self::Input, PeerError> {
        if snapshot
            .panes
            .get(pane_id)
            .and_then(|pane| pane.workspace.file.as_ref())
            .is_some_and(|file| file.path == self.path && file.truncated)
        {
            return Err(PeerError::InvalidMessage(
                "ファイルが大きすぎるため編集できません。".into(),
            ));
        }
        snapshot
            .files
            .file_drafts
            .get(&self.path)
            .cloned()
            .ok_or_else(|| PeerError::InvalidMessage("file has no draft to save".into()))
    }
    type Output = (Arc<FileDraft>, FileContent);
    async fn run(
        &self,
        submitted: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let file = context
            .call(&rpc::WriteFile {
                path: self.path.clone(),
                revision: submitted.revision.clone(),
                text: submitted.text.clone(),
            })
            .await?;
        Ok((submitted, file))
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        pane_id: &str,
        (submitted, file): Self::Output,
    ) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        let effects = super::file_tree::refresh_file_trees(snapshot, &file.path);
        if snapshot
            .panes
            .get(pane_id)
            .and_then(|pane| pane.workspace.file.as_ref())
            .is_some_and(|current| current.path == file.path)
            && let Some(pane) = snapshot.panes.get_mut(pane_id)
        {
            Arc::make_mut(&mut Arc::make_mut(pane).workspace).file = Some(Arc::new(file));
        }
        effects
    }
    fn stale(
        self,
        snapshot: &mut Snapshot,
        _pane_id: &str,
        (submitted, file): Self::Output,
    ) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        Vec::new()
    }
}
impl SaveFile {
    fn rebase_draft(snapshot: &mut Snapshot, submitted: &FileDraft, file: &FileContent) {
        Arc::make_mut(&mut snapshot.files.workspace)
            .file_metadata
            .invalidate(&file.path);
        let drafts = Arc::make_mut(&mut snapshot.files.file_drafts);
        if let Some(current) = drafts.get_mut(&file.path) {
            if current.as_ref() == submitted {
                drafts.remove(&file.path);
            } else if current.revision == submitted.revision {
                Arc::make_mut(current).revision = file.revision.clone();
            }
        }
    }
}

pub use agent_protocol::operations::WriteWorkspaceFile;

/// A file written into a workspace, as T3 Code saves a plan there: its
/// dialog shows the write running and why it failed, so no notice does.
impl Operation for WriteWorkspaceFile {
    rpc_operation!();
    const REPORT_ERROR: bool = false;
    fn navigation_bound(&self) -> bool {
        false
    }
    fn prepare(&mut self, _snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        if self.cwd.trim().is_empty() {
            return Err("ワークスペースのパスがありません".into());
        }
        if self.relative_path.trim().is_empty() {
            return Err("ワークスペース内のパスを入力してください".into());
        }
        Ok(())
    }
}

pub use agent_protocol::operations::ChangeWorkspace;

impl Operation for ChangeWorkspace {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorkspaceAction {
            pane: String::new(),
        })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::Control
    }
    rpc_operation!();
    const REPORT_ERROR: bool = false;
    fn prepare(&mut self, snapshot: &mut Snapshot, pane_id: &str) -> Result<(), String> {
        let pane = snapshot.panes.get(pane_id).ok_or("pane is closed")?;
        if self.cwd.is_empty() || snapshot.selected_directory(pane) != self.cwd {
            return Err("The workspace changed. Refresh and try again.".into());
        }
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, pane_id: &str, _: Self::Output) -> Vec<Effect> {
        review_workspace(snapshot, pane_id).into_iter().collect()
    }
}
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
    pub base_ref: Option<String>,
}
rpc::rpc_method!(ReviewWorkspace, ReviewWorkspace, |self| {
    rpc::ReviewWorkspace {
        cwd: self.cwd.clone(),
        include_options: false,
        base_ref: self.base_ref.clone(),
    }
});

pub(in crate::state) fn preparing_workspace(
    preparations: &[crate::session::SessionPreparation],
) -> bool {
    preparations.iter().any(|preparation| {
        preparation.status == crate::session::PreparationStatus::Preparing
            || matches!(
                preparation.stage,
                crate::session::PreparationStage::Workspace
                    | crate::session::PreparationStage::Worktree
            )
    })
}

/// Navigation and notifications already belong to an epoch. Their review read
/// shares it instead of dispatching a second intent that invalidates siblings.
pub(in crate::state) fn review_workspace(snapshot: &mut Snapshot, pane_id: &str) -> Option<Effect> {
    let pane = snapshot.panes.get(pane_id)?.clone();
    // Preparing tasks still sit in the source checkout until the Host moves them.
    let preparing = pane
        .navigation
        .thread_id
        .as_ref()
        .and_then(|id| snapshot.sessions.conversations.get(id))
        .is_some_and(|thread| preparing_workspace(&thread.preparations));
    if preparing || snapshot.selected_directory(&pane).is_empty() {
        if pane.workspace.review.is_some() || pane.workspace.review_cwd.is_some() {
            let pane = Arc::make_mut(snapshot.panes.get_mut(pane_id)?);
            let workspace = Arc::make_mut(&mut pane.workspace);
            workspace.review = None;
            workspace.review_cwd = None;
        }
        return None;
    }
    if !snapshot.connection.connected {
        return None;
    }
    let mut operation = ReviewWorkspace {
        base_ref: None,
        cwd: pane.navigation.cwd.clone(),
    };
    operation
        .prepare(snapshot, pane_id)
        .expect("selecting a live review directory is infallible");
    Some(Effect::execute(operation))
}

impl Operation for ReviewWorkspace {
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestReview
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorkspaceReview {
            pane: String::new(),
        })
    }
    type Input = bool;
    fn capture(&self, snapshot: &Snapshot, pane_id: &str) -> Result<bool, PeerError> {
        // New-chat options share the existing workspace read. Refreshes of a
        // running conversation never resolve remote defaults or reload refs.
        Ok(snapshot.panes.get(pane_id).is_some_and(|pane| {
            pane.navigation.thread_id.is_none() && pane.navigation.cwd == self.cwd
        }) && !snapshot.files.workspace.options.contains_key(&self.cwd))
    }
    type Output = crate::models::WorkspaceReview;
    async fn run(
        &self,
        include_options: bool,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&rpc::ReviewWorkspace {
                cwd: self.cwd.clone(),
                base_ref: self.base_ref.clone(),
                include_options,
            })
            .await
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, pane_id: &str) -> Result<(), String> {
        let pane = snapshot.panes.get_mut(pane_id).ok_or("pane is closed")?;
        let current_directory = pane.navigation.cwd == self.cwd;
        let workspace = Arc::make_mut(&mut Arc::make_mut(pane).workspace);
        if workspace.review_cwd.as_ref() != Some(&self.cwd) {
            workspace.review = None;
        }
        if current_directory {
            self.base_ref = workspace.comparison.clone();
        }
        workspace.review_cwd = Some(self.cwd.clone());
        Ok(())
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        pane_id: &str,
        mut review: Self::Output,
    ) -> Vec<Effect> {
        if let Some(options) = review.options.take() {
            if let Some(pane) = snapshot.panes.get(pane_id)
                && pane.navigation.thread_id.is_none()
                && pane.navigation.cwd == self.cwd
            {
                let draft = Arc::make_mut(
                    Arc::make_mut(&mut snapshot.input.drafts)
                        .entry(pane.navigation.draft_key.clone())
                        .or_default(),
                );
                draft.workspace.get_or_insert_with(|| {
                    if options.is_repository && options.default_worktree {
                        agent_protocol::workspace::WorkspaceStrategy::Worktree {
                            base_ref: options.default_base.clone(),
                            start_from_origin: true,
                        }
                    } else {
                        agent_protocol::workspace::WorkspaceStrategy::Root
                    }
                });
            }
            Arc::make_mut(&mut snapshot.files.workspace)
                .options
                .insert(self.cwd, Arc::new(options));
        }
        if let Some(pane) = snapshot.panes.get_mut(pane_id) {
            Arc::make_mut(&mut Arc::make_mut(pane).workspace).review = Some(Arc::new(review));
        }
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadWorktreeSettings {}
rpc::rpc_method!(ReadWorktreeSettings, ReadWorktreeSettings, |self| {
    crate::models::Empty {}
});

impl Operation for ReadWorktreeSettings {
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestHostState(OperationKey::WorktreeSettings)
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorktreeSettings)
    }
    rpc_operation!(files.workspace.settings);
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UpdateWorktreeSettings {
    pub settings: WorktreeSettings,
}
rpc::rpc_method!(UpdateWorktreeSettings, UpdateWorktreeSettings, |self| self
    .settings
    .clone());

impl Operation for UpdateWorktreeSettings {
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorktreeSettings)
    }
    rpc_operation!(files.workspace.settings);
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFile {
    pub source: String,
    pub destination: String,
}
impl Operation for DownloadFile {
    no_input!();
    type Output = ();
    const REPORT_ERROR: bool = false;

    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let session = context.session.ok_or_else(|| {
            PeerError::InvalidMessage("binary transfers require an iroh session".into())
        })?;
        crate::transfers::download_file(
            context.client,
            || async { session.open_stream().await.map_err(std::io::Error::other) },
            std::path::Path::new(&self.source),
            std::path::Path::new(&self.destination),
        )
        .await
        .map_err(PeerError::from)
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSessionImages {
    pub thread_id: crate::session::SessionRef,
}
impl Operation for LoadSessionImages {
    no_input!();
    type Output = Vec<crate::client::SessionImage>;
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::SessionImages {
            images: std::mem::take(output),
        }
    }
    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .client
            .session_images(&self.thread_id, context.session)
            .await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListWorktrees {}
rpc::rpc_method!(ListWorktrees, ListWorktrees, |self| crate::models::Empty {});
impl Operation for ListWorktrees {
    fn navigation_bound(&self) -> bool {
        false
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Worktrees)
    }
    rpc_operation!();
    fn apply(
        self,
        snapshot: &mut Snapshot,
        _pane_id: &str,
        worktrees: Self::Output,
    ) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.files.workspace).worktrees = Some(Arc::new(worktrees));
        Vec::new()
    }
}

pub use agent_protocol::operations::RemoveWorktree;

impl Operation for RemoveWorktree {
    fn navigation_bound(&self) -> bool {
        false
    }
    rpc_operation!();
    fn apply(self, _: &mut Snapshot, _pane_id: &str, _: Self::Output) -> Vec<Effect> {
        vec![Effect::execute(ListWorktrees {})]
    }
}

pub use agent_protocol::operations::LoadVisualization;

impl Operation for LoadVisualization {
    rpc_operation!();
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Visualization {
            html: std::mem::take(output),
        }
    }
}

#[cfg(test)]
mod read_result_tests {
    use super::*;
    #[rstest::rstest]
    #[case("/target.txt", true, true)]
    #[case("/target.txt", false, false)]
    #[case("/other.txt", true, false)]
    fn reads_preserve_drafts_and_only_the_truncated_target_rejects_changes(
        #[case] loaded_path: &str,
        #[case] truncated: bool,
        #[case] read_only: bool,
    ) {
        let path = "/target.txt".to_owned();
        let original = Arc::new(FileDraft {
            revision: "original".into(),
            text: "unsaved work".into(),
        });
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.files.file_drafts).insert(path.clone(), original.clone());
        ReadFile {
            path: loaded_path.into(),
            discard_draft: false,
        }
        .apply(
            &mut snapshot,
            "",
            FileContent {
                path: loaded_path.into(),
                revision: "prefix".into(),
                text: "preview".into(),
                size: if truncated { 2_000_000 } else { 7 },
                truncated,
            },
        );
        assert_eq!(snapshot.files.file_drafts[&path], original);
        assert_eq!(
            SaveFile { path: path.clone() }
                .capture(&snapshot, "")
                .is_err(),
            read_only
        );
        let (edited, effects) = crate::state::reduce(
            &snapshot,
            "",
            Event::Intent(Intent::SetFileDraft {
                path: path.clone(),
                text: "new user text".into(),
            }),
        );
        assert!(effects.is_empty());
        assert_eq!(edited.panes.primary().error.is_some(), read_only);
        assert_eq!(
            edited.files.file_drafts[&path].text,
            if read_only {
                "unsaved work"
            } else {
                "new user text"
            }
        );
        assert_eq!(
            edited.panes.primary().workspace.file.as_ref().unwrap().text,
            "preview"
        );
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn draft_review_loads_choices_once_and_running_conversation_reviews_only_read_changes() {
        let mut snapshot = Snapshot::default();
        let operation = ReviewWorkspace {
            base_ref: None,
            cwd: "/project".into(),
        };
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.get_mut("").unwrap()).navigation).cwd =
            "/project".into();
        assert!(operation.capture(&snapshot, "").unwrap());
        operation.clone().apply(
            &mut snapshot,
            "",
            crate::models::WorkspaceReview {
                controls: Default::default(),
                options: Some(agent_protocol::workspace::WorkspaceOptions {
                    is_repository: true,
                    default_worktree: true,
                    default_base: "origin/main".into(),
                    refs: vec!["origin/main".into()],
                    worktrees: vec!["/existing".into()],
                }),
                branch: "main".into(),
                base_ref: None,
                additions: 0,
                deletions: 0,
                files: vec![],
                diff: String::new(),
            },
        );
        assert!(!operation.capture(&snapshot, "").unwrap());
        assert_eq!(
            snapshot.files.workspace.options["/project"].default_base,
            "origin/main"
        );
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        assert_eq!(
            snapshot.input.drafts[&key].workspace,
            Some(agent_protocol::workspace::WorkspaceStrategy::Worktree {
                base_ref: "origin/main".into(),
                start_from_origin: true,
            })
        );
        assert!(
            snapshot
                .panes
                .primary()
                .workspace
                .review
                .as_ref()
                .unwrap()
                .options
                .is_none()
        );
        Arc::make_mut(&mut snapshot.files.workspace).options.clear();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.get_mut("").unwrap()).navigation)
            .thread_id = Some(
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "existing".into())
                .unwrap(),
        );
        assert!(!operation.capture(&snapshot, "").unwrap());
    }

    #[test]
    fn folder_navigation_accepts_home_paths_and_rejects_workspace_relative_paths() {
        let mut snapshot = Snapshot::default();
        for path in [
            "~",
            "~/Documents",
            "/workspace",
            "C:/Users/host",
            r"\\server\share",
        ] {
            let mut list = ListFiles { path: path.into() };
            assert!(list.prepare(&mut snapshot, "").is_ok(), "{path}");
        }
        for path in ["", "documents", "~someone/documents"] {
            let mut list = ListFiles { path: path.into() };
            assert!(list.prepare(&mut snapshot, "").is_err(), "{path}");
        }
    }
}
