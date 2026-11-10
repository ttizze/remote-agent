//! Export the core snapshot and shared graph directly; only ABI getters live here.
use super::{AgentError, error};
use crate::presentation::files::Files;
use crate::{
    models::{FileContent, ListQuery, WorktreeSettings},
    presentation::conversation::{ItemPresentation, RenderedConversation, RenderedItem},
    state::{Draft, FileDraft, Snapshot},
};
use std::sync::Arc;

#[uniffi::export]
impl Snapshot {
    pub fn ui_language(&self) -> crate::localization::UiLanguage {
        self.catalog.ui_language
    }
    pub fn connection_generation(&self) -> u64 {
        self.connection.connection_generation
    }
    #[uniffi::constructor]
    pub fn empty() -> Arc<Self> {
        Arc::default()
    }
    #[uniffi::constructor]
    pub fn restore(bytes: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        Ok(Arc::new(if bytes.is_empty() {
            Self::default()
        } else {
            serde_json::from_slice(&bytes).map_err(error)?
        }))
    }
    /// Shared state and primary selection for inspection; live side panes are not restored.
    /// Use serialize_local_state for durable storage.
    pub fn serialize(&self) -> Result<Vec<u8>, AgentError> {
        serde_json::to_vec(self).map_err(error)
    }
    pub fn serialize_local_state(&self) -> Result<Vec<u8>, AgentError> {
        crate::persistence::encode(self).map_err(error)
    }
    pub fn connected(&self) -> bool {
        self.connection.connected
    }
    pub fn host_name(&self) -> Option<String> {
        self.connection.host_name.clone()
    }
    pub fn host_ticket(&self) -> Option<String> {
        self.connection.host_ticket.clone()
    }
    pub fn files(&self, pane_id: &str) -> Option<Arc<Files>> {
        self.panes.get(pane_id).map(|pane| {
            Arc::new(Files {
                resources: self.files.clone(),
                selection: pane.workspace.clone(),
            })
        })
    }
    pub fn draft(&self, key: crate::state::DraftKey) -> Draft {
        self.input
            .drafts
            .get(&key)
            .map(|draft| draft.as_ref().clone())
            .unwrap_or_default()
    }
    pub fn list_query(&self) -> ListQuery {
        self.lists.list_query.as_ref().clone()
    }
}
#[uniffi::export]
impl RenderedConversation {
    pub fn id(&self) -> Option<crate::session::SessionRef> {
        self.source.id.clone()
    }
    pub fn turn_count(&self) -> u64 {
        self.source
            .turns
            .as_ref()
            .map_or(0, |turns| turns.len() as u64)
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
}
#[uniffi::export]
impl RenderedItem {
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
    pub fn presentation(&self) -> ItemPresentation {
        self.data.clone()
    }
}

// The conversation badge reads counts without copying the potentially large diff.
#[uniffi::export]
impl WorkspaceReview {
    pub fn diff_files(&self) -> Vec<crate::presentation::diff::WorkspaceDiffFile> {
        crate::presentation::diff::diff_files(&self.0)
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[uniffi::export]
impl Files {
    pub fn file_draft(&self, path: String) -> Option<FileDraft> {
        self.resources
            .file_drafts
            .get(&path)
            .map(|draft| draft.as_ref().clone())
    }

    pub fn file(&self) -> Option<FileContent> {
        self.selection.file.as_deref().cloned()
    }
    pub fn review(&self) -> Option<Arc<WorkspaceReview>> {
        self.selection
            .review
            .as_ref()
            .map(|value| Arc::new(WorkspaceReview(value.clone())))
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.resources.workspace.settings.as_deref().cloned()
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        crate::state::Same::same(&self.resources, &other.resources)
            && Arc::ptr_eq(&self.selection, &other.selection)
    }
}

#[derive(uniffi::Object)]
pub struct WorkspaceReview(Arc<crate::models::WorkspaceReview>);
