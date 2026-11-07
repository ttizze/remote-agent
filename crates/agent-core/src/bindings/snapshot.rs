//! Export the core snapshot and shared graph directly; only ABI getters live here.
use super::{AgentError, error};
use crate::presentation::files::Files;
use crate::{
    models::{FileContent, ListQuery, Model, WorktreeSettings},
    presentation::conversation::{
        ItemPresentation, RenderedConversation, RenderedItem, RenderedTurn,
    },
    state::{Draft, FileDraft, Navigation, Snapshot},
};
use std::sync::Arc;

#[uniffi::export]
impl Snapshot {
    pub fn connection_generation(&self) -> u64 {
        self.connection.connection_generation
    }
    pub fn conversation_source(&self) -> Option<Arc<Thread>> {
        self.conversation_thread()
            .map(|value| Arc::new(Thread(value)))
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
    pub fn connected(&self) -> bool {
        self.connection.connected
    }
    pub fn host_name(&self) -> Option<String> {
        self.connection.host_name.clone()
    }
    pub fn host_ticket(&self) -> Option<String> {
        self.connection.host_ticket.clone()
    }
    pub fn host_home_directory(&self) -> Option<String> {
        self.connection.host_home_directory.clone()
    }
    pub fn error(&self) -> Option<crate::presentation::error::ErrorText> {
        self.pane
            .error
            .as_deref()
            .map(crate::presentation::error::error_text)
    }
    pub fn files(&self) -> Arc<Files> {
        self.files_view()
    }
    pub fn navigation(&self) -> Navigation {
        self.pane.navigation.as_ref().clone()
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
    pub fn models(&self) -> Vec<Model> {
        self.catalog.models.as_ref().clone()
    }
    pub fn conversation(&self, id: crate::session::SessionRef) -> Option<Arc<Thread>> {
        self.sessions
            .conversations
            .get(&id)
            .map(|value| Arc::new(Thread(value.clone())))
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
    pub fn source(&self) -> Arc<Thread> {
        Arc::new(Thread(self.source.clone()))
    }
    pub fn unchanged(&self, other: Arc<Self>) -> bool {
        std::ptr::eq(self, other.as_ref())
    }
    pub fn turns(&self) -> Vec<Arc<RenderedTurn>> {
        self.turns.clone()
    }
    pub fn queued(&self) -> Vec<Arc<RenderedItem>> {
        self.queued.clone()
    }
    pub fn session_rows(&self) -> Vec<crate::presentation::conversation::ConversationRow> {
        self.session_rows.clone()
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
pub struct Thread(Arc<crate::models::Thread>);
#[derive(uniffi::Object)]
pub struct WorkspaceReview(Arc<crate::models::WorkspaceReview>);

#[uniffi::export]
pub fn project_conversation(
    snapshot: &Snapshot,
    source: Arc<Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    crate::presentation::conversation::project_conversation(
        &(snapshot).pane.navigation.draft_key,
        &(snapshot).sessions.pending_submissions,
        source.0.clone(),
        previous,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_clients_receive_readable_errors_without_rpc_envelopes() {
        let snapshot = crate::state::Snapshot {connection:crate::state::Connection {..Default::default()},pane:Arc::new(crate::state::PaneState {error:Some(r#"remote RPC error: {"code":"thread_in_use","message":"private","delivery":"notSent"}"#.into()),..Default::default()}),..Default::default()};
        assert_eq!(
            snapshot.error().map(|error| error.message).as_deref(),
            Some(agent_protocol::error::THREAD_IN_USE_MESSAGE)
        );
        assert!(snapshot.pane.error.as_deref().unwrap().contains("private"));
    }

    use super::*;

    #[tokio::test]
    async fn native_snapshot_identity_distinguishes_edits_from_duplicate_publication() {
        let store = crate::store::Store::offline(Snapshot::default());
        let before = store.snapshot();
        let edit = || crate::state::Intent::SetDraftText {
            thread_id: crate::state::DraftKey::Local {
                key: "draft".into(),
            },
            text: "日本語の下書き".into(),
        };
        store.dispatch(edit()).await.unwrap();
        let edited = store.snapshot();
        assert!(edited.changes_from(&before).any);
        store.dispatch(edit()).await.unwrap();
        assert!(!edited.changes_from(&store.snapshot()).any);
        store.close().await.unwrap();
    }

    #[test]
    fn native_list_publication_follows_project_lifecycle_without_republishing_other_operations() {
        use crate::state::{
            Event, Intent,
            operations::{
                ListProjectSessions, Operation, OperationKey, OperationPhase, OperationState,
            },
        };
        let mut initial = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            ..Default::default()
        };
        initial.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "data":[], "projects":[{"id":"old","name":"Archive","roots":[]}], "hasMore":true,"hasMoreProjects":false,"projectPages":{},"limit":5
            }))
            .unwrap(),
        ));
        assert!(initial.list_source().equivalent(&initial.list_source()));
        let mut unrelated = initial.clone();
        unrelated.operations.insert(
            OperationKey::TaskActivity,
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(unrelated.list_source().equivalent(&initial.list_source()));
        let mut root_loading = initial.clone();
        root_loading.operations.insert(
            OperationKey::SessionList,
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(
            !root_loading
                .list_source()
                .equivalent(&initial.list_source())
        );
        assert!(
            root_loading
                .list_source()
                .equivalent(&root_loading.list_source())
        );
        let loading = root_loading.clone();
        root_loading.operations = Default::default();
        assert!(
            !root_loading
                .list_source()
                .equivalent(&loading.list_source())
        );
        assert!(
            root_loading
                .list_source()
                .equivalent(&initial.list_source())
        );
        let (opened, _) = crate::state::reduce(
            &initial,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: true,
            }),
        );
        assert!(opened.thread_list().unwrap().projects[0].expanded);
        assert!(!opened.list_source().equivalent(&initial.list_source()));
        let mut disconnected = opened.clone();
        disconnected.connection.connected = false;
        assert!(
            disconnected.thread_list().unwrap().projects[0]
                .error
                .is_some()
        );
        assert!(!disconnected.list_source().equivalent(&opened.list_source()));
        let key = OperationKey::ProjectList {
            project_id: "old".into(),
        };
        let mut loading = opened.clone();
        loading.operations.insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(loading.thread_list().unwrap().projects[0].loading);
        assert!(!loading.list_source().equivalent(&opened.list_source()));
        let mut failed = loading.clone();
        failed.operations.get_mut(&key).unwrap().generation = 2;
        assert!(failed.list_source().equivalent(&loading.list_source()));
        failed.operations.get_mut(&key).unwrap().phase = OperationPhase::Failed {
            message: "unavailable".into(),
        };
        let project = &failed.thread_list().unwrap().projects[0];
        assert!(!project.loading);
        assert_eq!(
            project
                .error
                .as_ref()
                .and_then(|error| error.details.as_deref()),
            Some("unavailable")
        );
        assert!(!failed.list_source().equivalent(&loading.list_source()));
        let mut loaded = failed.clone();
        ListProjectSessions { project_id: "old".into(), limit: 5, search_term: String::new() }.apply(&mut loaded, serde_json::from_value(serde_json::json!({
            "data":[{"id":{"provider":"codex","id":"old-task"},"name":"Old task","projectId":"old","status":"running"}], "projects":[], "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
        })).unwrap());
        loaded.operations.remove(&key);
        let project = &loaded.thread_list().unwrap().projects[0];
        assert_eq!(project.threads[0].title, "Old task");
        assert!(project.threads[0].active);
        assert!(!project.loading);
        assert!(project.error.is_none());
        assert!(!loaded.list_source().equivalent(&failed.list_source()));
        let (closed, _) = crate::state::reduce(
            &loaded,
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "old".into(),
                expanded: false,
            }),
        );
        assert!(!closed.thread_list().unwrap().projects[0].expanded);
        assert!(closed.thread_list().unwrap().projects[0].threads.is_empty());
        assert!(!closed.list_source().equivalent(&loaded.list_source()));
        let mut archived = closed.clone();
        Arc::make_mut(&mut archived.lists.archived_scopes)
            .insert("previous".into(), Arc::default());
        assert!(archived.thread_list() == closed.thread_list());
        assert!(!archived.list_source().equivalent(&closed.list_source()));
    }

    #[test]
    fn abi_handles_preserve_conversation_projection_identity() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).thread_id =
            Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "chat".into(),
            });
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
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
