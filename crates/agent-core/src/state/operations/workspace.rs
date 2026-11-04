use super::*;
use crate::client::ClientExt;

#[cfg(test)]
mod path_tests {
    use super::*;

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
            assert!(
                ListFiles { path: path.into() }
                    .prepare(&mut snapshot)
                    .is_ok()
            );
        }
        for path in ["", "documents", "~someone/documents"] {
            assert!(
                ListFiles { path: path.into() }
                    .prepare(&mut snapshot)
                    .is_err()
            );
        }
    }
}

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
        Some(OperationKey::Directory)
    }
    rpc_operation!();
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::Directory {
            listing: output.clone(),
        }
    }
    fn prepare(&mut self, _: &mut Snapshot) -> Result<(), String> {
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
        Some(OperationKey::File)
    }
    rpc_operation!(workspace.file);
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::FileRead {
            file: output.clone(),
        }
    }
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if self.discard_draft {
            Arc::make_mut(&mut snapshot.file_drafts).remove(&self.path);
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
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::SaveFile {
            path: self.path.clone(),
        })
    }
    type Input = Arc<FileDraft>;
    fn capture(&self, snapshot: &Snapshot) -> Result<Self::Input, PeerError> {
        if snapshot
            .workspace
            .file
            .as_ref()
            .is_some_and(|file| file.path == self.path && file.truncated)
        {
            return Err(PeerError::InvalidMessage(
                "ファイルが大きすぎるため編集できません。".into(),
            ));
        }
        snapshot
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
    fn apply(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        let effects = super::file_tree::refresh_file_trees(snapshot, &file.path);
        if snapshot
            .workspace
            .file
            .as_ref()
            .is_some_and(|current| current.path == file.path)
        {
            Arc::make_mut(&mut snapshot.workspace).file = Some(Arc::new(file));
        }
        effects
    }
    fn stale(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        Vec::new()
    }
}
impl SaveFile {
    fn rebase_draft(snapshot: &mut Snapshot, submitted: &FileDraft, file: &FileContent) {
        Arc::make_mut(&mut snapshot.workspace)
            .file_metadata
            .invalidate(&file.path);
        let drafts = Arc::make_mut(&mut snapshot.file_drafts);
        if let Some(current) = drafts.get_mut(&file.path) {
            if current.as_ref() == submitted {
                drafts.remove(&file.path);
            } else if current.revision == submitted.revision {
                Arc::make_mut(current).revision = file.revision.clone();
            }
        }
    }
}

pub use agent_protocol::operations::ReviewWorkspace;

/// Navigation and notifications already belong to an epoch. Their review read
/// shares it instead of dispatching a second intent that invalidates siblings.
pub(in crate::state) fn review_workspace(snapshot: &mut Snapshot) -> Option<Effect> {
    // A preparing task still sits in its source checkout until the Host moves
    // it to a worktree; those changes are not the task's. T3 Code shows none.
    let preparing = snapshot
        .navigation
        .thread_id
        .as_ref()
        .and_then(|id| snapshot.conversations.get(id))
        .is_some_and(|thread| thread.preparation.is_some());
    if preparing || snapshot.selected_directory().is_empty() {
        if snapshot.workspace.review.is_some() || snapshot.workspace.review_cwd.is_some() {
            let workspace = Arc::make_mut(&mut snapshot.workspace);
            workspace.review = None;
            workspace.review_cwd = None;
        }
        return None;
    }
    if !snapshot.connected {
        return None;
    }
    let mut operation = ReviewWorkspace {
        cwd: snapshot.navigation.cwd.clone(),
    };
    operation
        .prepare(snapshot)
        .expect("selecting a review directory is infallible");
    Some(Effect::execute(operation))
}

impl Operation for ReviewWorkspace {
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestReview
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorkspaceReview)
    }
    rpc_operation!();
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        let workspace = Arc::make_mut(&mut snapshot.workspace);
        if workspace.review_cwd.as_ref() != Some(&self.cwd) {
            workspace.review = None;
        }
        workspace.review_cwd = Some(self.cwd.clone());
        Ok(())
    }
    fn apply(self, snapshot: &mut Snapshot, review: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).review = Some(Arc::new(review));
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
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorktreeSettings)
    }
    rpc_operation!(workspace.settings);
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
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::WorktreeSettings)
    }
    rpc_operation!(workspace.settings);
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
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Worktrees)
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, worktrees: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).worktrees = Some(Arc::new(worktrees));
        Vec::new()
    }
}

pub use agent_protocol::operations::RemoveWorktree;

impl Operation for RemoveWorktree {
    rpc_operation!();
    fn apply(self, _: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
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
    #[test]
    fn truncated_reads_keep_old_drafts_but_reject_edits_and_save_requests() {
        let path = "/large.txt".to_owned();
        let original = Arc::new(FileDraft {
            revision: "original".into(),
            text: "unsaved work".into(),
        });
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.file_drafts).insert(path.clone(), original.clone());
        ReadFile {
            path: path.clone(),
            discard_draft: false,
        }
        .apply(
            &mut snapshot,
            FileContent {
                path: path.clone(),
                revision: "prefix".into(),
                text: "preview".into(),
                size: 2_000_000,
                truncated: true,
            },
        );
        assert!(SaveFile { path: path.clone() }.capture(&snapshot).is_err());
        let (edited, effects) = crate::state::reduce(
            &snapshot,
            Event::Intent(Intent::SetFileDraft {
                path: path.clone(),
                text: "replace with prefix".into(),
            }),
        );
        assert!(effects.is_empty());
        assert!(edited.error.is_some());
        assert_eq!(edited.file_drafts[&path], original);
        assert_eq!(edited.workspace.file.as_ref().unwrap().text, "preview");
    }
    #[test]
    fn each_read_receipt_keeps_its_content_when_another_file_replaces_the_editor_snapshot() {
        let file = |path: &str, text: &str| FileContent {
            path: path.into(),
            text: text.into(),
            revision: "r1".into(),
            size: text.len() as u64,
            truncated: false,
        };
        let mut parent = file("/repo/parent.md", "[child](child.md)");
        let receipt = ReadFile::outcome(&mut parent);
        let mut snapshot = Snapshot::default();
        ReadFile {
            path: parent.path.clone(),
            discard_draft: false,
        }
        .apply(&mut snapshot, parent);
        ReadFile {
            path: "/repo/child.md".into(),
            discard_draft: false,
        }
        .apply(&mut snapshot, file("/repo/child.md", "child content"));
        assert_eq!(
            snapshot.workspace.file.as_ref().unwrap().path,
            "/repo/child.md"
        );
        let Outcome::FileRead { file } = receipt else {
            panic!("file read result")
        };
        assert_eq!(file.path, "/repo/parent.md");
        assert_eq!(file.text, "[child](child.md)");
    }
}
