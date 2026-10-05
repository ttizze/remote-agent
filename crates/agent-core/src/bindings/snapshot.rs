//! Mobile receives the same presentation records the desktop uses.
use super::{AgentError, error};
use crate::{
    models::{FileContent, FileList, Model, Project, WorktreeSettings},
    presentation::{self, ConversationView, Shelf},
    state::{Draft, Snapshot},
};
use agent_protocol::operations::{AccountLogin, Accounts};
use std::sync::Arc;
#[uniffi::export]
impl Snapshot {
    pub fn connection_setup(&self) -> crate::presentation::connections::ConnectionSetup {
        crate::presentation::connections::connection_setup(self)
    }
    pub fn model_errors(&self) -> std::collections::HashMap<String, String> {
        self.model_errors
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
    #[uniffi::constructor]
    pub fn empty() -> Arc<Self> {
        Arc::default()
    }
    #[uniffi::constructor]
    pub fn restore(bytes: Vec<u8>) -> Result<Arc<Self>, AgentError> {
        Ok(Arc::new(crate::persistence::decode(&bytes).map_err(error)?))
    }
    pub fn serialize_local_state(&self) -> Result<Vec<u8>, AgentError> {
        crate::persistence::encode(self).map_err(error)
    }
    pub fn serialize_model_preferences(&self) -> Result<Vec<u8>, AgentError> {
        crate::persistence::encode_model_preferences(self).map_err(error)
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn host_name(&self) -> Option<String> {
        self.host_name.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn selected_thread_id(&self) -> Option<String> {
        self.selected_thread.as_ref().map(ToString::to_string)
    }
    pub fn selected_project_id(&self) -> Option<String> {
        self.selected_project.clone()
    }
    pub fn current_directory(&self) -> String {
        self.cwd()
    }
    pub fn draft(&self) -> Draft {
        self.current_draft()
    }
    pub fn draft_context_key(&self) -> String {
        self.draft_key()
    }
    pub fn conversation(&self) -> ConversationView {
        presentation::conversation(self)
    }
    pub fn archived_threads(
        &self,
        now_millis: i64,
    ) -> Result<Vec<presentation::ThreadRow>, AgentError> {
        Ok(presentation::archived_threads(
            self,
            &orchestration::Timestamp::from_millis(now_millis).map_err(error)?,
        ))
    }
    pub fn shelves(&self, now_millis: i64, settled_limit: u32) -> Result<Vec<Shelf>, AgentError> {
        Ok(presentation::shelves(
            self,
            &orchestration::Timestamp::from_millis(now_millis).map_err(error)?,
            settled_limit as usize,
        ))
    }
    pub fn search_query(&self) -> String {
        self.search.clone()
    }
    pub fn model_choices(&self) -> Vec<presentation::ModelChoice> {
        presentation::model_choices(self)
    }
    pub fn can_open_terminal(&self) -> bool {
        self.connected && !self.cwd().is_empty()
    }
    pub fn review_revision(&self) -> Option<String> {
        self.workspace
            .review
            .as_ref()
            .map(|_| format!("{}:{}", self.cwd(), self.workspace.review_generation))
    }
    pub fn models(&self) -> Vec<Model> {
        self.models.clone()
    }
    pub fn current_draft_key(&self) -> String {
        self.draft_key()
    }
    pub fn projects(&self) -> Vec<Project> {
        self.projects.clone()
    }
    pub fn directory(&self) -> Option<FileList> {
        self.workspace.directory.clone()
    }
    pub fn file(&self) -> Option<FileContent> {
        self.workspace
            .file
            .as_ref()
            .map(|file| file.as_ref().clone())
    }
    pub fn file_draft(&self, path: String) -> Option<String> {
        self.workspace
            .file_drafts
            .get(&path)
            .map(|draft| draft.text.clone())
    }
    pub fn review(&self) -> Option<Arc<WorkspaceReview>> {
        self.workspace
            .review
            .as_ref()
            .map(|r| Arc::new(WorkspaceReview(r.clone())))
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.workspace.worktree_settings.clone()
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.accounts.clone()
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.account_login.clone()
    }
    pub fn terminal(&self, handle: String, after: u64) -> crate::state::TerminalView {
        self.terminal_view(&handle, after)
    }
}
#[derive(uniffi::Object)]
pub struct WorkspaceReview(pub(crate) Arc<crate::models::WorkspaceReview>);
#[uniffi::export]
impl WorkspaceReview {
    pub fn diff_files(&self) -> Vec<crate::presentation::diff::WorkspaceDiffFile> {
        crate::presentation::diff::diff_files(&self.0)
    }
    pub fn file_count(&self) -> u64 {
        self.0.files.len() as u64
    }
    pub fn branch(&self) -> String {
        self.0.branch.clone()
    }
    pub fn additions(&self) -> u64 {
        self.0.additions
    }
    pub fn deletions(&self) -> u64 {
        self.0.deletions
    }
}
