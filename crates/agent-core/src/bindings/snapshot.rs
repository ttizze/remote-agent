//! Export the core snapshot and shared graph directly; only ABI getters live here.
use super::{AgentError, error};
use crate::{
    models::{FileContent, FileList, ListQuery, Model, WorktreeSettings},
    presentation::conversation::{
        ItemPresentation, RenderedConversation, RenderedItem, RenderedTurn,
    },
    session::ProviderKind,
    state::{Draft, FileDraft, Navigation, Snapshot},
};
use agent_protocol::operations::{AccountLogin, Accounts};
use std::sync::Arc;

#[uniffi::export]
impl Snapshot {
    pub fn conversation_source(&self) -> Option<Arc<Thread>> {
        self.conversation_thread()
            .map(|value| Arc::new(Thread(value)))
    }
    pub fn list_unchanged(&self, other: Arc<Self>) -> bool {
        let same_list = match (&self.threads, &other.threads) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        same_list
            && Arc::ptr_eq(&self.activity, &other.activity)
            && Arc::ptr_eq(&self.expanded_projects, &other.expanded_projects)
            && Arc::ptr_eq(&self.project_threads, &other.project_threads)
            && Arc::ptr_eq(&self.archived_scopes, &other.archived_scopes)
            && self.connected == other.connected
            && self.threads.as_ref().is_none_or(|list| {
                list.projects.iter().all(|project| {
                    let key = crate::state::operations::OperationKey::ProjectList {
                        project_id: project.id.clone(),
                    };
                    self.operations.get(&key).map(|state| &state.phase)
                        == other.operations.get(&key).map(|state| &state.phase)
                })
            })
    }
    pub fn models_unchanged(&self, other: Arc<Self>) -> bool {
        Arc::ptr_eq(&self.models, &other.models)
    }
    pub fn conversation_unchanged(&self, other: Arc<Self>) -> bool {
        let id = self.navigation.thread_id.as_ref();
        if id != other.navigation.thread_id.as_ref()
            || self.navigation.draft_key != other.navigation.draft_key
        {
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
        same_thread && Arc::ptr_eq(&self.pending_submissions, &other.pending_submissions)
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
    /// Full in-memory snapshot for inspection; use serialize_local_state for durable storage.
    pub fn serialize(&self) -> Result<Vec<u8>, AgentError> {
        serde_json::to_vec(self).map_err(error)
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
    pub fn navigation(&self) -> Navigation {
        self.navigation.as_ref().clone()
    }
    pub fn draft(&self, key: crate::state::DraftKey) -> Draft {
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
    pub fn conversation(&self, id: crate::session::SessionRef) -> Option<Arc<Thread>> {
        self.conversations
            .get(&id)
            .map(|value| Arc::new(Thread(value.clone())))
    }
    pub fn directory(&self) -> Option<FileList> {
        self.workspace.directory.as_deref().cloned()
    }
    pub fn file(&self) -> Option<FileContent> {
        self.workspace.file.as_deref().cloned()
    }
    pub fn file_draft(&self, path: String) -> Option<FileDraft> {
        self.file_drafts
            .get(&path)
            .map(|draft| draft.as_ref().clone())
    }
    pub fn review(&self) -> Option<Arc<WorkspaceReview>> {
        self.workspace
            .review
            .as_ref()
            .map(|value| Arc::new(WorkspaceReview(value.clone())))
    }
    pub fn worktree_settings(&self) -> Option<WorktreeSettings> {
        self.workspace.settings.as_deref().cloned()
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.account.accounts.as_deref().cloned()
    }
    pub fn account_is_selected(&self, provider: ProviderKind, id: String) -> bool {
        self.account.accounts.as_ref().is_some_and(|accounts| {
            accounts
                .accounts
                .iter()
                .find(|account| account.provider == provider && account.id == id)
                .is_some_and(|account| accounts.is_selected(account))
        })
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.account.login.as_deref().cloned()
    }
}
#[uniffi::export]
impl Thread {
    pub fn active_turn_id(&self) -> Option<agent_protocol::ids::TurnId> {
        self.0.active_turn_id()
    }
    pub fn id(&self) -> Option<crate::session::SessionRef> {
        self.0.id.clone()
    }
    pub fn title(&self) -> String {
        self.0.name.clone().unwrap_or_default()
    }
    pub fn input_unavailable_reason(&self) -> Option<String> {
        crate::session::input_unavailable_reason(&self.0)
    }
    pub fn history_notice(&self) -> Option<String> {
        crate::presentation::conversation::history_notice(&self.0)
    }
    pub fn has_more_history(&self) -> bool {
        self.0.history_has_more == Some(true)
    }
    pub fn turn_count(&self) -> u64 {
        self.0.turns.as_ref().map_or(0, |turns| turns.len() as u64)
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
    pub fn unplaced_requests(&self) -> Vec<crate::presentation::conversation::ConversationRow> {
        self.request_rows.clone()
    }
}
#[uniffi::export]
impl RenderedTurn {
    pub fn id(&self) -> String {
        self.source.id.to_string()
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
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
    pub fn unknown_submission_id(&self) -> Option<String> {
        match &self.source {
            crate::presentation::conversation::ItemSource::Pending(id, pending)
                if pending.delivery_unknown =>
            {
                Some(id.clone())
            }
            _ => None,
        }
    }
}

// The conversation badge reads counts without copying the potentially large diff.
#[uniffi::export]
impl WorkspaceReview {
    pub fn diff_files(&self) -> Vec<crate::presentation::diff::WorkspaceDiffFile> {
        crate::presentation::diff::diff_files(&self.0)
    }
    pub fn file_count(&self) -> u64 {
        self.0.files.len() as u64
    }
    pub fn additions(&self) -> u64 {
        self.0.additions
    }
    pub fn deletions(&self) -> u64 {
        self.0.deletions
    }
}

#[derive(uniffi::Object)]
pub struct Thread(Arc<crate::models::Thread>);
#[derive(uniffi::Object)]
pub struct WorkspaceReview(Arc<crate::models::WorkspaceReview>);

#[uniffi::export]
pub fn project_conversation(
    snapshot: &Snapshot,
    source: Arc<Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    crate::presentation::conversation::project_conversation(snapshot, source.0.clone(), previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_list_publication_follows_project_lifecycle_without_republishing_other_operations() {
        use crate::state::{
            Event, Intent,
            operations::{
                ListProjectSessions, Operation, OperationKey, OperationPhase, OperationState,
            },
        };
        let mut initial = Snapshot {
            connected: true,
            ..Default::default()
        };
        initial.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "data":[], "projects":[{"id":"old","name":"Archive","roots":[]}], "hasMore":true
            }))
            .unwrap(),
        ));
        assert!(initial.list_unchanged(Arc::new(initial.clone())));
        let mut unrelated = initial.clone();
        Arc::make_mut(&mut unrelated.operations).insert(
            OperationKey::TaskActivity,
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(unrelated.list_unchanged(Arc::new(initial.clone())));
        let (opened, _) = crate::state::reduce(
            &initial,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            }),
        );
        assert!(opened.thread_list().unwrap().projects[0].expanded);
        assert!(!opened.list_unchanged(Arc::new(initial)));
        let mut disconnected = opened.clone();
        disconnected.connected = false;
        assert!(
            disconnected.thread_list().unwrap().projects[0]
                .error
                .is_some()
        );
        assert!(!disconnected.list_unchanged(Arc::new(opened.clone())));
        let key = OperationKey::ProjectList {
            project_id: "old".into(),
        };
        let mut loading = opened.clone();
        Arc::make_mut(&mut loading.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(loading.thread_list().unwrap().projects[0].loading);
        assert!(!loading.list_unchanged(Arc::new(opened)));
        let mut failed = loading.clone();
        Arc::make_mut(&mut failed.operations)
            .get_mut(&key)
            .unwrap()
            .generation = 2;
        assert!(failed.list_unchanged(Arc::new(loading.clone())));
        Arc::make_mut(&mut failed.operations)
            .get_mut(&key)
            .unwrap()
            .phase = OperationPhase::Failed {
            message: "unavailable".into(),
        };
        let project = &failed.thread_list().unwrap().projects[0];
        assert!(!project.loading);
        assert_eq!(project.error.as_deref(), Some("unavailable"));
        assert!(!failed.list_unchanged(Arc::new(loading)));
        let mut loaded = failed.clone();
        ListProjectSessions { project_id: "old".into(), limit: 5, search_term: String::new() }.apply(&mut loaded, serde_json::from_value(serde_json::json!({
            "data":[{"id":{"provider":"codex","id":"old-task"},"name":"Old task","projectId":"old","status":"running"}], "projects":[], "hasMore":false
        })).unwrap());
        Arc::make_mut(&mut loaded.operations).remove(&key);
        let project = &loaded.thread_list().unwrap().projects[0];
        assert_eq!(project.threads[0].title, "Old task");
        assert!(project.threads[0].active);
        assert!(!project.loading);
        assert!(project.error.is_none());
        assert!(!loaded.list_unchanged(Arc::new(failed)));
        let (closed, _) = crate::state::reduce(
            &loaded,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: false,
            }),
        );
        assert!(!closed.thread_list().unwrap().projects[0].expanded);
        assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
        assert!(!closed.list_unchanged(Arc::new(loaded)));
        let mut archived = closed.clone();
        Arc::make_mut(&mut archived.archived_scopes).insert("previous".into(), Arc::default());
        assert!(archived.thread_list().unwrap().notice.is_some());
        assert!(!archived.list_unchanged(Arc::new(closed)));
    }

    #[test]
    fn abi_handles_preserve_conversation_projection_identity() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id =
            Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "chat".into(),
            });
        Arc::make_mut(&mut snapshot.conversations).insert(
            agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "chat".into(),
            },
            Arc::new(crate::models::Thread {
                id: Some(agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chat".into(),
                }),
                ..Default::default()
            }),
        );
        let first = project_conversation(&snapshot, snapshot.conversation_source().unwrap(), &None);
        let second = project_conversation(
            &snapshot,
            snapshot.conversation_source().unwrap(),
            &Some(first.clone()),
        );
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            snapshot
                .conversation(agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chat".into()
                })
                .unwrap()
                .id(),
            Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "chat".into()
            })
        );
    }
}
