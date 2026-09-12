use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFiles {
    pub path: String,
}
rpc::rpc_method!(ListFiles, FileList, "host/file/list");

impl Operation for ListFiles {
    rpc_operation!(workspace.directory);
    const INVALIDATES: bool = true;
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFile {
    pub path: String,
    pub discard_draft: bool,
}
impl rpc::RpcMethod for ReadFile {
    type Output = FileContent;
    const METHOD: &'static str = "host/file/read";
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut params = serializer.serialize_struct("ReadFile", 1)?;
        params.serialize_field("path", &self.path)?;
        params.end()
    }
}

impl Operation for ReadFile {
    rpc_operation!(workspace.file);
    fn prepare(&mut self, snapshot: &mut Snapshot) -> Result<(), String> {
        if self.discard_draft {
            Arc::make_mut(&mut snapshot.file_drafts).remove(&self.path);
        }
        Ok(())
    }
    const INVALIDATES: bool = true;
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFile {
    pub path: String,
}
impl Operation for SaveFile {
    type Output = (FileDraft, FileContent);
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let submitted = context
            .snapshot
            .file_drafts
            .get(&self.path)
            .ok_or_else(|| PeerError::InvalidMessage("file has no draft to save".into()))?
            .clone();
        let file = context
            .call(&rpc::WriteFile {
                path: &self.path,
                revision: &submitted.revision,
                text: &submitted.text,
            })
            .await?;
        Ok((submitted, file))
    }
    fn apply(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        if snapshot
            .workspace
            .file
            .as_ref()
            .is_some_and(|current| current.path == file.path)
        {
            Arc::make_mut(&mut snapshot.workspace).file = Some(Arc::new(file));
        }
        Vec::new()
    }
    fn stale(self, snapshot: &mut Snapshot, (submitted, file): Self::Output) -> Vec<Effect> {
        Self::rebase_draft(snapshot, &submitted, &file);
        Vec::new()
    }
}
impl SaveFile {
    fn rebase_draft(snapshot: &mut Snapshot, submitted: &FileDraft, file: &FileContent) {
        if let Some(current) = Arc::make_mut(&mut snapshot.file_drafts).get_mut(&file.path) {
            if current == submitted {
                Arc::make_mut(&mut snapshot.file_drafts).remove(&file.path);
            } else if current.revision == submitted.revision {
                current.revision = file.revision.clone();
            }
        }
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}
rpc::rpc_method!(ReviewWorkspace, WorkspaceReview, "host/workspace/review");

/// Navigation and notifications already belong to an epoch. Their review read
/// shares it instead of dispatching a second intent that invalidates siblings.
pub(in crate::state) fn review_workspace(snapshot: &mut Snapshot) -> Option<Effect> {
    if snapshot.selected_directory().is_empty() {
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
    rpc_operation!();
    fn invalidates(&self, snapshot: &Snapshot) -> bool {
        snapshot.workspace.review_cwd.as_ref() != Some(&self.cwd)
    }
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
rpc::rpc_method!(
    ReadWorktreeSettings,
    super::WorktreeSettings,
    "host/worktree/settings/read"
);

impl Operation for ReadWorktreeSettings {
    rpc_operation!(workspace.settings);
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UpdateWorktreeSettings {
    pub settings: WorktreeSettings,
}
rpc::rpc_method!(
    UpdateWorktreeSettings,
    super::WorktreeSettings,
    "host/worktree/settings/update"
);

impl Operation for UpdateWorktreeSettings {
    rpc_operation!(workspace.settings);
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadFile {
    pub source: String,
    pub destination: String,
}
impl Operation for DownloadFile {
    type Output = ();

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let session = context.session.ok_or_else(|| {
            PeerError::InvalidMessage("binary transfers require an iroh session".into())
        })?;
        crate::transfers::download_file(
            context.peer,
            || async { session.open_stream().await.map_err(std::io::Error::other) },
            std::path::Path::new(&self.source),
            std::path::Path::new(&self.destination),
        )
        .await
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSessionImages {
    pub thread_id: String,
}
impl Operation for LoadSessionImages {
    type Output = Vec<rpc::SessionImage>;
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::SessionImages {
            images: std::mem::take(output),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.session_images(&self.thread_id).await
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListWorktrees {}
impl rpc::RpcMethod for ListWorktrees {
    type Output = Vec<crate::models::Worktree>;
    const METHOD: &'static str = "host/worktree/list";
}
impl Operation for ListWorktrees {
    rpc_operation!();
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, worktrees: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.workspace).worktrees = Some(Arc::new(worktrees));
        Vec::new()
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}
impl rpc::RpcMethod for RemoveWorktree {
    type Output = ();
    const METHOD: &'static str = "host/worktree/remove";
}
impl Operation for RemoveWorktree {
    rpc_operation!();
    const ORDERED: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _: Self::Output) -> Vec<Effect> {
        if let Some(worktrees) = Arc::make_mut(&mut snapshot.workspace).worktrees.as_mut() {
            Arc::make_mut(worktrees).retain(|worktree| worktree.path != self.path);
        }
        vec![Effect::execute(ListWorktrees {})]
    }
}
