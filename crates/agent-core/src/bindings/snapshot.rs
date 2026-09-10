//! Export the core snapshot and shared graph directly; only ABI getters live here.
use super::{AgentError, error};
use crate::{
    client::{AccountLogin, AccountLoginStatus, Accounts},
    models::{
        FileContent, FileList, Item, ListQuery, Model, Thread, Turn, WorkspaceReview,
        WorktreeSettings,
    },
    presentation::conversation::{
        ItemPresentation, RenderedConversation, RenderedItem, RenderedTurn, Request,
        TurnPresentationData, request,
    },
    state::{Draft, FileDraft, Navigation, Snapshot},
};
use std::sync::Arc;

#[uniffi::export]
impl Snapshot {
    pub fn list_unchanged(&self, other: Arc<Self>) -> bool {
        let same_list = match (&self.threads, &other.threads) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_list && Arc::ptr_eq(&self.activity, &other.activity)
    }
    pub fn models_unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.models, &other.models)
    }
    pub fn requests_unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.requests, &other.requests)
    }
    pub fn conversation_unchanged(&self, other: Arc<Self>) -> bool {
        let id = self.navigation.thread_id.as_ref();
        if id != other.navigation.thread_id.as_ref() {
            return false;
        }
        let same_thread = match (
            id.and_then(|id| self.conversations.get(id)),
            id.and_then(|id| other.conversations.get(id)),
        ) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_thread
            && Arc::ptr_eq(&self.pending_submissions, &other.pending_submissions)
            && Arc::ptr_eq(&self.requests, &other.requests)
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
    pub fn serialize(&self) -> Result<Vec<u8>, AgentError> {
        serde_json::to_vec(self).map_err(error)
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
    pub fn navigation(&self) -> Navigation {
        self.navigation.as_ref().clone()
    }
    pub fn draft(&self, key: String) -> Draft {
        self.drafts
            .get(&key)
            .map(|draft| draft.as_ref().clone())
            .unwrap_or_default()
    }
    pub fn list_query(&self) -> ListQuery {
        self.list_query.as_ref().clone()
    }
    pub fn models(&self) -> Vec<Model> {
        self.models.as_ref().clone()
    }
    pub fn conversation(&self, id: String) -> Option<Arc<Thread>> {
        self.conversations.get(&id).cloned()
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .iter()
            .map(|(key, source)| request(key, source))
            .collect()
    }
    pub fn directory(&self) -> Option<FileList> {
        self.workspace.directory.as_deref().cloned()
    }
    pub fn file(&self) -> Option<FileContent> {
        self.workspace.file.as_deref().cloned()
    }
    pub fn file_draft(&self, path: String) -> Option<FileDraft> {
        self.file_drafts.get(&path).cloned()
    }
    pub fn review(&self) -> Option<Arc<WorkspaceReview>> {
        self.workspace.review.clone()
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.workspace.settings.as_deref().cloned()
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.account.accounts.as_deref().cloned()
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.account.login.as_deref().cloned()
    }
    pub fn account_login_status(&self) -> Option<AccountLoginStatus> {
        self.account.login_status.as_deref().cloned()
    }
}
#[uniffi::export]
impl Thread {
    pub fn id(&self) -> String {
        self.id.clone().unwrap_or_default()
    }
    pub fn title(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
    pub fn cwd(&self) -> String {
        self.cwd.clone().unwrap_or_default()
    }
    pub fn history_cursor(&self) -> Option<String> {
        self.history_cursor.clone().flatten()
    }
    pub fn turns(&self) -> Vec<Arc<Turn>> {
        self.turns.clone().unwrap_or_default()
    }
}
#[uniffi::export]
impl Turn {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    pub fn items_cursor(&self) -> Option<String> {
        self.items_next_cursor.clone().flatten()
    }
    pub fn items(&self) -> Vec<Arc<Item>> {
        self.items.clone().unwrap_or_default()
    }
}
#[uniffi::export]
impl Item {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    pub fn client_id(&self) -> Option<String> {
        self.client_id.clone()
    }
    pub fn expanded_body(&self) -> String {
        crate::presentation::body::expanded_body(self)
    }
}
#[uniffi::export]
impl RenderedConversation {
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
    pub fn turns(&self) -> Vec<Arc<RenderedTurn>> {
        self.turns.clone()
    }
    pub fn queued(&self) -> Vec<Arc<RenderedItem>> {
        self.queued.clone()
    }
}
#[uniffi::export]
impl RenderedTurn {
    pub fn id(&self) -> String {
        self.source.id.clone()
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
    pub fn rows(&self) -> Vec<TurnPresentationData> {
        self.rows.clone()
    }
}
#[uniffi::export]
impl RenderedItem {
    pub fn id(&self) -> String {
        self.data.id.clone()
    }
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
    pub fn file_count(&self) -> u64 {
        self.files.len() as u64
    }
    pub fn additions(&self) -> u64 {
        self.additions
    }
    pub fn deletions(&self) -> u64 {
        self.deletions
    }
    pub fn diff(&self) -> String {
        self.diff.clone()
    }
}
