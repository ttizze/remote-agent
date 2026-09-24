use super::*;
use crate::client::ClientExt;

pub use agent_protocol::operations::ListFiles;

impl Operation for ListFiles {
    rpc_operation!(workspace.directory);
    fn prepare(&mut self, _: &mut Snapshot) -> Result<(), String> {
        if !std::path::Path::new(&self.path).is_absolute() {
            return Err("絶対パスを入力してください。".into());
        }
        Ok(())
    }
    const INVALIDATES: bool = true;
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
    type Output = (Arc<FileDraft>, FileContent);
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let submitted = context
            .snapshot
            .file_drafts
            .get(&self.path)
            .ok_or_else(|| PeerError::InvalidMessage("file has no draft to save".into()))?
            .clone();
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
rpc::rpc_method!(ReadWorktreeSettings, ReadWorktreeSettings, |self| {
    crate::models::Empty {}
});

impl Operation for ReadWorktreeSettings {
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
            context.client,
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
    type Output = Vec<crate::client::SessionImage>;
    fn outcome(output: &mut Self::Output) -> Outcome {
        Outcome::SessionImages {
            images: std::mem::take(output),
        }
    }
    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
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
