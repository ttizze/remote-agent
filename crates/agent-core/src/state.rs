//! Immutable client state and pure conversation transitions.
use crate::models::{
    FileContent, HostStatus, Invitation, Item, ListQuery, Model, RemoteHost, Thread, ThreadList,
    WorkspaceReview, WorktreeSettings,
};
use agent_protocol::requests::{Answer, Request};
use operations as op;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub invocations: Vec<agent_protocol::composer::Invocation>,
    pub model: Option<crate::models::ModelRef>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    /// An explicit local choice awaiting an accepted submission. Reopening
    /// otherwise refreshes these options from the Host's latest conversation.
    #[serde(default)]
    #[cfg_attr(feature = "bindings", uniffi(default = false))]
    pub model_settings_changed: bool,
}
/// Explicit picker choices remembered on this device. Reading a conversation or
/// refreshing a catalog must never change what the next new chat starts with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelPreferences {
    pub provider: Option<crate::session::ProviderKind>,
    pub selections: HashMap<crate::session::ProviderKind, ModelSelection>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSelection {
    pub model: crate::models::ModelRef,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Attachment {
    pub path: String,
    pub name: String,
    pub is_image: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct FileDraft {
    pub revision: String,
    pub text: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(skip)]
    pub file_metadata: crate::presentation::files::FileMetadataCache,
    #[serde(skip)]
    pub file_trees: crate::presentation::files::FileTrees,
    pub file: Option<Arc<FileContent>>,
    pub review_cwd: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<crate::models::Worktree>>>,
}
/// Only conversation-owned selections are cached; Host settings stay shared.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkspaceLocation {
    cwd: String,
    file: Option<Arc<FileContent>>,
    review_cwd: Option<String>,
    review: Option<Arc<WorkspaceReview>>,
}
impl Workspace {
    fn location(&self, cwd: String) -> WorkspaceLocation {
        WorkspaceLocation {
            cwd,
            file: self.file.clone(),
            review_cwd: self.review_cwd.clone(),
            review: self.review.clone(),
        }
    }
}
/// New drafts have a local identity; existing drafts use the complete session identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DraftKey {
    Session { session: crate::session::SessionRef },
    Local { key: String },
}
impl Default for DraftKey {
    fn default() -> Self {
        Self::Local { key: String::new() }
    }
}
impl From<String> for DraftKey {
    fn from(key: String) -> Self {
        Self::Local { key }
    }
}
impl From<&str> for DraftKey {
    fn from(value: &str) -> Self {
        Self::from(value.to_owned())
    }
}
impl From<crate::session::SessionRef> for DraftKey {
    fn from(session: crate::session::SessionRef) -> Self {
        Self::Session { session }
    }
}
impl From<&crate::session::SessionRef> for DraftKey {
    fn from(value: &crate::session::SessionRef) -> Self {
        Self::from(value.clone())
    }
}
impl DraftKey {
    fn is_empty(&self) -> bool {
        matches!(self, Self::Local { key } if key.is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Navigation {
    pub thread_id: Option<crate::session::SessionRef>,
    pub cwd: String,
    pub draft_key: DraftKey,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(skip)]
    pub active: BTreeMap<crate::session::SessionRef, bool>,
    pub unread: BTreeSet<crate::session::SessionRef>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostManagement {
    pub status: Option<Arc<HostStatus>>,
    pub remotes: Vec<RemoteHost>,
    #[serde(skip)]
    pub invitation: Option<Arc<Invitation>>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountState {
    #[serde(skip)]
    pub codex_cli: Option<Arc<agent_protocol::operations::CodexCliStatus>>,
    pub accounts: Option<Arc<agent_protocol::operations::Accounts>>,
    #[serde(skip)]
    pub login: Option<Arc<agent_protocol::operations::AccountLogin>>,
    #[serde(skip)]
    pub dsh_credentials: Option<Arc<agent_protocol::dsh::Credentials>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmission {
    pub sequence: u64,
    pub submitted_at_ms: Option<u64>,
    pub draft_key: DraftKey,
    pub draft: Arc<Draft>,
    pub turn_id: Option<agent_protocol::ids::TurnId>,
    pub after_item_id: Option<agent_protocol::ids::ItemId>,
    pub accepted: bool,
    pub delivery_unknown: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TerminalPhase {
    Suspended,
    Detached,
    Starting,
    Running,
    Exited(i32),
    Failed(String),
}
impl TerminalPhase {
    pub fn label(&self) -> String {
        match self {
            Self::Starting => "起動中".into(),
            Self::Running => "実行中".into(),
            Self::Suspended => "再接続を待っています".into(),
            Self::Detached => "切断済み".into(),
            Self::Exited(code) => format!("終了 · {code}"),
            Self::Failed(error) => crate::presentation::error::error_text(error).to_string(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutput {
    pub sequence: u64,
    #[serde(with = "crate::protocol::bytes")]
    pub data: Vec<u8>,
    pub reset_size: Option<agent_protocol::operations::TerminalSize>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub cwd: String,
    pub size: agent_protocol::operations::TerminalSize,
    pub phase: TerminalPhase,
    pub output: VecDeque<Arc<TerminalOutput>>,
    pub sequence: u64,
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalView {
    pub status: Option<String>,
    pub loading: bool,
    pub accepts_input: bool,
    pub output: Vec<TerminalOutput>,
}
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectImportState {
    pub scan: Option<agent_protocol::project_import::ProjectScan>,
    pub selected: BTreeSet<String>,
    pub result: Option<agent_protocol::project_import::ProjectImportResult>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    #[serde(skip)]
    pub preferred_editor: Option<agent_protocol::editors::EditorId>,
    #[serde(skip)]
    pub file_open_capabilities: Option<Arc<agent_protocol::editors::FileOpenCapabilities>>,
    /// Desktop opts into this Host catalog; keep the subscription across reconnects.
    #[serde(skip)]
    pub file_open_capabilities_requested: bool,
    #[serde(skip)]
    pub project_import: Arc<ProjectImportState>,
    /// Mobile recent feed loads project pages independently of expansion.
    #[serde(skip)]
    pub recent_chats: bool,
    /// Device-owned organization, scoped by Host storage identity and registered project ID.
    #[serde(default)]
    pub project_preferences: Arc<BTreeMap<String, BTreeMap<String, ProjectPreferences>>>,
    #[serde(skip)]
    pub operations: Arc<BTreeMap<operations::OperationKey, operations::OperationState>>,
    #[serde(skip)]
    pub operation_sequence: u64,
    #[serde(default)]
    pub model_preferences: ModelPreferences,
    #[serde(skip)]
    pub permission_settings: Option<Arc<op::PermissionSettingsState>>,
    #[serde(skip)]
    pub composer_catalog: Option<Arc<agent_protocol::composer::ComposerCatalog>>,
    #[serde(skip)]
    pub host_name: Option<String>,
    /// The Host's latest reachable addresses; platforms save them for cold connections.
    #[serde(skip)]
    pub host_ticket: Option<String>,
    /// Retained while offline, but never inferred from workspace paths or persisted.
    #[serde(skip)]
    pub host_home_directory: Option<String>,
    #[serde(skip)]
    pub task_activity: Option<Arc<agent_protocol::live_activity::TaskActivityState>>,
    pub storage_scope: String,
    pub archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    #[serde(default)]
    pub account: Arc<AccountState>,
    #[serde(skip)]
    pub terminals: Arc<BTreeMap<String, Arc<Terminal>>>,
    #[serde(default)]
    #[serde(with = "crate::persistence::entries")]
    pub conversations: Arc<BTreeMap<crate::session::SessionRef, Arc<Thread>>>,
    #[serde(default)]
    pub threads: Option<Arc<ThreadList>>,
    #[serde(skip)]
    pub expanded_projects: Arc<BTreeMap<String, u32>>,
    #[serde(skip)]
    pub project_threads: Arc<BTreeMap<String, Arc<ThreadList>>>,
    /// Disk copies need one authoritative read; live lists continue through notifications.
    #[serde(skip)]
    pub unconfirmed_lists: Arc<BTreeSet<op::OperationKey>>,
    #[serde(skip)]
    pub observed_agents: Option<crate::session::SessionRef>,
    #[serde(default)]
    pub models: Arc<Vec<Model>>,
    #[serde(default)]
    pub model_errors: Arc<Map<String, Value>>,
    #[serde(with = "crate::persistence::entries")]
    pub drafts: Arc<BTreeMap<DraftKey, Arc<Draft>>>,
    /// Answers composed for awaiting questions; local to this device.
    #[serde(skip)]
    pub question_drafts:
        Arc<BTreeMap<agent_protocol::ids::RequestId, crate::questions::QuestionDraft>>,
    /// Startup identity changes outlive pending sends, which subscribers may coalesce.
    #[serde(skip)]
    pub draft_sessions: Arc<BTreeMap<DraftKey, crate::session::SessionRef>>,
    pub pending_submissions:
        Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    #[serde(default)]
    pub workspace: Arc<Workspace>,
    #[serde(skip)]
    pub workspaces: Arc<BTreeMap<DraftKey, WorkspaceLocation>>,
    pub navigation: Arc<Navigation>,
    pub activity: Arc<Activity>,
    #[serde(default)]
    pub management: Arc<HostManagement>,
    #[serde(skip)]
    pub list_query: Arc<ListQuery>,
    #[serde(default)]
    pub epoch: u64,
    #[serde(skip)]
    pub connected: bool,
    #[serde(skip)]
    pub connection_generation: u64,
    /// Why the latest connection attempt failed. Losing an established
    /// connection is not a failure, for example a socket the OS closed while
    /// the app was suspended: its replacement reports none until it fails.
    #[serde(skip)]
    pub connection_failure: Option<String>,
    #[serde(skip)]
    pub subscriptions: Arc<BTreeMap<crate::session::SessionRef, uuid::Uuid>>,
    #[serde(skip)]
    pub error: Option<String>,
}
impl Snapshot {
    pub(crate) fn listed_threads(&self) -> impl Iterator<Item = &Thread> {
        self.threads
            .iter()
            .chain(self.project_threads.values())
            .flat_map(|page| &page.data)
    }

    pub fn thread_metadata(&self, id: &crate::session::SessionRef) -> Option<&Thread> {
        self.conversations.get(id).map(AsRef::as_ref).or_else(|| {
            self.listed_threads()
                .find(|thread| thread.id.as_ref() == Some(id))
        })
    }

    pub fn requests(&self) -> impl Iterator<Item = &Arc<Request>> {
        self.conversations
            .values()
            .flat_map(|thread| thread.requests.values())
    }

    pub fn request(&self, id: &str) -> Option<&Arc<Request>> {
        self.conversations
            .values()
            .find_map(|thread| thread.requests.get(id))
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn operation_phase(
        &self,
        key: operations::OperationKey,
    ) -> Option<operations::OperationPhase> {
        self.operations
            .get(&key)
            .map(|operation| operation.phase.clone())
    }
    pub fn operation_running(&self, key: operations::OperationKey) -> bool {
        self.operations
            .get(&key)
            .is_some_and(|state| state.phase == operations::OperationPhase::Running)
    }
    pub fn project_import_busy(&self) -> bool {
        self.operation_running(op::OperationKey::ProjectScan)
            || self.operation_running(op::OperationKey::ProjectImport)
    }
    pub fn operation_error(
        &self,
        key: operations::OperationKey,
    ) -> Option<crate::presentation::error::ErrorText> {
        self.operations
            .get(&key)
            .and_then(|state| match &state.phase {
                operations::OperationPhase::Failed { message } => {
                    Some(crate::presentation::error::error_text(message))
                }
                _ => None,
            })
    }
    pub fn terminal_view(&self, handle: String) -> Option<TerminalView> {
        self.terminals.get(&handle).map(|terminal| TerminalView {
            status: match terminal.phase {
                TerminalPhase::Starting | TerminalPhase::Running => None,
                _ => Some(terminal.phase.label()),
            },
            loading: matches!(terminal.phase, TerminalPhase::Starting),
            accepts_input: matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running
            ),
            output: terminal
                .output
                .iter()
                .map(|chunk| (**chunk).clone())
                .collect(),
        })
    }

    pub fn model_error_messages(
        &self,
        provider: Option<crate::session::ProviderKind>,
    ) -> Vec<String> {
        let provider = provider.map(crate::session::ProviderKind::key);
        self.model_errors
            .iter()
            .filter(|(key, _)| provider.is_none_or(|provider| provider == key.as_str()))
            .map(|(provider, error)| {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string());
                format!("{provider}: {message}")
            })
            .collect()
    }
}

mod notifications;
use notifications::notification;
pub mod projects;
pub use projects::ProjectPreferences;
pub mod operations;
pub use operations::Intent;

/// Client-owned data from a previously configured Host storage area. Keeping
/// it separate prevents storage switches from deleting or mixing user drafts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopedData {
    #[serde(with = "crate::persistence::entries")]
    pub drafts: Arc<BTreeMap<DraftKey, Arc<Draft>>>,
    pub pending_submissions:
        Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    pub navigation: Arc<Navigation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    StorageScope(String),
    HostTicket(String),
    HostHomeDirectory(Option<String>),
    TerminalFailed { handle: String, reason: String },
    Intent(Intent),
    SubmissionFailed(agent_protocol::ids::ClientInputId),
    SubmissionUnknown(agent_protocol::ids::ClientInputId),

    Notification(crate::protocol::Notification),
    SessionUpdate(Box<crate::session::SessionUpdate>),
    ConnectionChecked { failure: Option<String> },
    Connected,
    Disconnected,
}
pub use crate::store::Effect;

// Invalidate both displayed content and responses still in flight. File drafts
// remain keyed by absolute path so navigation never discards unsaved edits.
fn clear_workspace_location(workspace: &mut Workspace) {
    workspace.file = None;
    workspace.review = None;
    workspace.review_cwd = None;
}

pub fn reduce(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    let (mut next, mut effects) = match event {
        Event::Intent(intent) => reduce_intent(previous, intent),

        Event::Notification(message) => notification(previous, message),
        Event::SessionUpdate(update) => notifications::session_update(previous, *update),
        event => reduce_event(previous, event),
    };
    if next.connected
        && previous.navigation.thread_id != next.navigation.thread_id
        && let Some(selected) = &next.navigation.thread_id
        && let Some((session, turn_id)) = next
            .task_activity
            .as_ref()
            .and_then(|state| state.unread.iter().find(|(id, _)| id == selected))
    {
        effects.push(Effect::execute(op::MarkSessionRead {
            session: session.clone(),
            turn_id: turn_id.clone(),
        }));
    }
    effects.extend(op::prefetch_composer_catalog(&mut next));
    effects.extend(op::prefetch_model_settings(
        next.connected,
        next.drafts
            .get(&next.navigation.draft_key)
            .and_then(|draft| draft.model.as_ref()),
        &next.models,
        next.operations.keys(),
    ));
    (next, effects)
}
macro_rules! prepare_operations {
    ($intent:expr, $next:ident, [$($variant:ident),* $(,)?], {$($local:tt)*}) => {
        match $intent {
            $(Intent::$variant(operation) => return prepare($next, operation),)*
            $($local)*
        }
    };
}

fn reduce_intent(previous: &Snapshot, intent: Intent) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    prepare_operations!(intent, next, [
        ReadTaskActivity, RegisterLiveActivity, UnregisterLiveActivity,
        ReadPermissionSettings, UpdatePermissionSettings,
        ManageCodexCli, ReadDshCredentials, UpdateDshCredential, ListAccounts, SelectAccount, SelectAccountForDraft, LogoutAccount, StartAccountLogin,
        ReadAccountLogin, CancelAccountLogin, SubmitAccountLogin, ForkSession,
        StartTerminal, DetachTerminal, KillTerminal, CreateInvitation, RemoveRemoteHost,
        RevokeDevice, ListFiles, ResolveFile, ResolveFileReference, ReadFile,
        ReadFileOpenCapabilities, OpenInEditor,
        SaveFile, ReviewWorkspace, ReadWorktreeSettings,
        UpdateWorktreeSettings, ListWorktrees, RemoveWorktree, ListProjectSessions,
        ListAgents, AddProject, ScanProjects, ImportProjects, CreateSession, RetryPreparation,
        ReadThread, OpenRequest, ReadItem, ResizeTerminal,
        Interrupt,
        WriteTerminal, DownloadFile, LoadSessionImages, LoadVisualization,
        LoadHostName, LoadHostManagement, LoadModels, ReadModelSettings,
        Respond, Transcribe, UploadAttachment, PairRemoteHost,
    ], {
        Intent::SetPreferredEditor { editor } => next.preferred_editor = Some(editor),
        Intent::EnsureFileMetadata { paths, now_ms } => {
            let effects = op::ensure_file_metadata(&mut next, paths, now_ms);
            return (next, effects);
        }
        Intent::FileTree { key, action } => {
            let effects = op::file_tree_action(&mut next, key, action);
            return (next, effects);
        }
        Intent::DismissError { id } => match id {
            op::ErrorId::Operation { key, generation } => {
                if next.operations.get(&key).is_some_and(|state| {
                    state.generation == generation && matches!(state.phase, op::OperationPhase::Failed { .. })
                }) { Arc::make_mut(&mut next.operations).remove(&key); }
            }
            op::ErrorId::Application { message } => {
                for error in [&mut next.connection_failure, &mut next.error] {
                    if error.as_deref() == Some(message.as_str()) {
                        *error = None;
                    }
                }
            }
        },
        Intent::WatchAgents { thread_id } => {
            let thread_id = thread_id.filter(|id| next.navigation.thread_id.as_ref() == Some(id));
            if next.observed_agents != thread_id {
                if let Some(session) = next.observed_agents.take() {
                    Arc::make_mut(&mut next.operations).remove(&op::OperationKey::Agents { session });
                }
                next.observed_agents = thread_id;
                let effects = op::refresh_agents(next.connected, next.observed_agents.as_ref());
                return (next, effects);
            }
        }
        Intent::SelectImportProject { path, selected } => {
            if !next.project_import_busy()
                && next.project_import.scan.as_ref().is_some_and(|scan| scan.projects.iter().any(|project| project.path == path && !project.registered))
            {
                let selection = &mut Arc::make_mut(&mut next.project_import).selected;
                if selected { selection.insert(path); } else { selection.remove(&path); }
            }
        }
        Intent::SelectAllImportProjects { selected } => {
            if !next.project_import_busy() {
                Arc::make_mut(&mut next.project_import).selected = next.project_import.scan.as_ref().map(|scan| scan.projects.iter()
                    .filter(|project| selected && !project.registered).map(|project| project.path.clone()).collect()).unwrap_or_default();
            }
        }
        Intent::ReadOlder { thread_id } => {
            if previous.operation_running(op::OperationKey::History { session: thread_id.clone() }) {
                return (next, Vec::new());
            }
            if let Some(cursor) = previous.conversations.get(&thread_id).and_then(|thread| thread.history_cursor.clone()) {
                return prepare(next, op::ReadHistory { session: thread_id, cursor, include_activity: false });
            }
            let window = op::HISTORY_WINDOW_TURNS;
            let limit = u32::try_from(previous.conversations.get(&thread_id).map_or(window.into(), |thread| {
                thread.history_limit
                    .unwrap_or_else(|| thread.turns.as_ref().map_or(window.into(), |turns| turns.len() as u64))
            })).unwrap_or(u32::MAX).saturating_add(window);
            return prepare(next, op::ReadThread { limit, ..op::ReadThread::new(thread_id) });
        }
        Intent::LoadTurnItems(params) => {
            if previous.conversations.get(&params.thread_id)
                .and_then(|thread| thread.turns.as_ref())
                .and_then(|turns| turns.iter().rfind(|turn| turn.id == params.turn_id))
                .is_some_and(|turn| turn.items_summary) {
                return prepare(next, params);
            }
        }
        Intent::CloseTerminal { handle } => {
            if let Some(terminal) = Arc::make_mut(&mut next.terminals).remove(&handle)
                && !matches!(terminal.phase, TerminalPhase::Exited(_) | TerminalPhase::Detached)
            {
                return (next, vec![Effect::execute(op::KillTerminal { handle })]);
            }
        }
        Intent::AcknowledgeTerminal { handle, sequence } => {
            if previous.terminals.get(&handle).is_some_and(|terminal| {
                terminal.output.front().is_some_and(|chunk| chunk.sequence <= sequence)
            }) && let Some(terminal) = shared_mut(&mut next.terminals, &handle)
            {
                while terminal
                    .output
                    .front()
                    .is_some_and(|chunk| chunk.sequence <= sequence)
                {
                    terminal.output.pop_front();
                }
            }
        }
        Intent::Submit {
            thread_id,
            client_user_message_id,
        } => {
            let thread_id = thread_id.or_else(|| previous.navigation.thread_id.clone());
            let draft_key = thread_id
                .clone()
                .map(DraftKey::from)
                .unwrap_or_else(|| previous.navigation.draft_key.clone());
            let draft = previous.drafts.get(&draft_key).cloned().unwrap_or_default();
            return submission(
                previous,
                thread_id,
                draft_key,
                draft,
                client_user_message_id,
                None,
            );
        }
        Intent::RestoreUnknownSubmission { client_user_message_id } => {
            if previous
                .pending_submissions
                .get(&client_user_message_id)
                .is_some_and(|pending| pending.delivery_unknown)
            {
                return reduce(previous, Event::SubmissionFailed(client_user_message_id));
            }
        }
        Intent::DiscardUnknownSubmission { client_user_message_id } => {
            if previous
                .pending_submissions
                .get(&client_user_message_id)
                .is_some_and(|pending| pending.delivery_unknown)
            {
                Arc::make_mut(&mut next.pending_submissions).remove(&client_user_message_id);
            }
        }
        Intent::RemoveAttachment { draft_key, index } => {
            let index = index as usize;
            if previous.drafts.get(&draft_key).is_some_and(|draft| index < draft.attachments.len())
                && let Some(draft) = shared_mut(&mut next.drafts, &draft_key)
            {
                draft.attachments.remove(index);
            }
        }

        Intent::ListSessions(mut operation) => {
            let refresh = operation.query.search_term == previous.list_query.search_term;
            if let Err(error) = op::Operation::prepare(&mut operation, &mut next) {
                next.error = Some(error);
                return (next, Vec::new());
            }
            let mut effects = vec![Effect::execute(operation)];
            if refresh && next.connected { effects.push(Effect::execute(op::ReadTaskActivity {})); }
            return (next, effects);
        }
        Intent::RefreshProject { project_id } => {
            let Some(limit) = previous.expanded_projects.get(&project_id).copied() else {
                return (next, Vec::new());
            };
            return prepare(next, op::ListProjectSessions {
                project_id,
                limit,
                search_term: previous.list_query.search_term.clone(),
            });
        }
        Intent::SetProjectPinned { project_id, pinned } => projects::update(&mut next, &project_id, |p| p.pinned = pinned),
        Intent::ReorderProject { project_id, target_id, after } => projects::reorder(&mut next, &project_id, &target_id, after),
        Intent::MoveChatToProject { thread_id, project_id } => {
            projects::move_chat(&mut next, &thread_id, &project_id);
            if next.connected && next.expanded_projects.contains_key(&project_id)
                && !next.project_threads.contains_key(&project_id)
                && next.chat_placement(&thread_id) == Some(project_id.as_str()) {
                return prepare(next, op::ListProjectSessions {
                    project_id, limit: 5, search_term: previous.list_query.search_term.clone(),
                });
            }
        },
        Intent::EditProject { project_id, name, section } => {
            let name = name.trim();
            let section = section.trim();
            if name.is_empty() || name.chars().count() > 100 || section.chars().count() > 60 {
                next.error = Some("表示名は1〜100文字、セクションは60文字以内で入力してください".into());
            } else {
                projects::update(&mut next, &project_id, |p| {
                    p.name = Some(name.into());
                    p.section = (!section.is_empty()).then(|| section.into());
                });
            }
        }
        Intent::SetProjectSection { project_id, section } => {
            let section = section.trim();
            if section.chars().count() <= 60 {
                projects::update(&mut next, &project_id, |p| p.section = (!section.is_empty()).then(|| section.into()));
            }
        }
        Intent::SetProjectHidden { project_id, hidden } => projects::update(&mut next, &project_id, |p| p.hidden = hidden),
        Intent::ArchiveProjectChats { project_id, before } => projects::archive(&mut next, &project_id, before),
        Intent::RestoreProjectChats { project_id } => projects::update(&mut next, &project_id, |p| {
            p.archived_before = None;
            p.archived_ids.clear();
        }),
        Intent::SetProjectExpanded { project_id, expanded } => {
            if !expanded {
                Arc::make_mut(&mut next.expanded_projects).remove(&project_id);
            } else if !previous.expanded_projects.contains_key(&project_id) {
                if previous.threads.as_ref().is_none_or(|page| {
                    !page.projects.iter().any(|project| project.id == project_id)
                }) {
                    return (next, Vec::new());
                }
                let page = previous.project_threads.get(&project_id);
                let limit = page.map_or(5, |page| page.limit);
                Arc::make_mut(&mut next.expanded_projects).insert(project_id.clone(), limit);
                if next.connected && (page.is_none() || next.unconfirmed_lists.contains(&op::OperationKey::ProjectList { project_id: project_id.clone() })) {
                    return prepare(next, op::ListProjectSessions {
                        project_id,
                        limit,
                        search_term: previous.list_query.search_term.clone(),
                    });
                }
            }
        }
        Intent::ExpandThreadList { project_id } => {
            if let Some(project_id) = project_id {
                let Some(limit) = previous.expanded_projects.get(&project_id) else {
                    return (next, Vec::new());
                };
                let limit = limit.saturating_add(10);
                Arc::make_mut(&mut next.expanded_projects).insert(project_id.clone(), limit);
                return prepare(next, op::ListProjectSessions {
                    project_id,
                    limit,
                    search_term: previous.list_query.search_term.clone(),
                });
            }
            let mut query = (*previous.list_query).clone();
            query.limit = query.limit.saturating_add(10);
            query.part = Some(crate::models::ListPart::Chats);
            return prepare(next, op::ListSessions::new(query));
        }
        Intent::ShowRecentChats | Intent::ExpandRecentChats => {
            let expanding = matches!(intent, Intent::ExpandRecentChats);
            if !expanding && previous.recent_chats {
                return (next, Vec::new());
            }
            next.recent_chats = true;
            let mut query = (*previous.list_query).clone();
            if expanding {
                query.limit = query.limit.saturating_add(10);
            }
            query.project_limit = query.project_limit.max(query.limit);
            query.part = None;
            if next.connected {
                return prepare(next, op::ListSessions::new(query));
            }
        }
        Intent::ExpandProjects => {
            let mut query = (*previous.list_query).clone();
            query.project_limit = query.project_limit.saturating_add(10);
            query.part = Some(crate::models::ListPart::Projects);
            return prepare(next, op::ListSessions::new(query));
        }

        Intent::ShowThreadList => {
            next.epoch += 1;
            navigate(&mut next, Navigation::default());
            return (next, Vec::new());
        }
        Intent::NewChat { cwd } => {
            next.epoch += 1;
            let key = DraftKey::Local { key: format!("new:{cwd}") };
            let existing = previous.drafts.get(&key);
            if existing.is_none_or(|draft| draft.model.is_none()) {
                let selection = previous.model_preferences.provider
                    .and_then(|provider| previous.model_preferences.selections.get(&provider));
                let mut draft = existing.map(|draft| (**draft).clone()).unwrap_or_default();
                let provider = selection.map(|selection| selection.model.provider)
                    .or_else(|| previous.model_provider_for_draft(key.clone()));
                let (model, effort, tier) = match provider {
                    Some(provider) => supported_settings(
                        selection.map(|selection| &selection.model),
                        draft.effort.as_deref().or_else(|| selection.and_then(|selection| selection.effort.as_deref())),
                        draft.service_tier.as_deref().or_else(|| selection.and_then(|selection| selection.service_tier.as_deref())),
                        Some(provider), &previous.models, &previous.model_errors, previous.allows_custom_codex_models(),
                    ),
                    None => (None, draft.effort.as_deref(), draft.service_tier.as_deref()),
                };
                let settings = (model.cloned(), effort.map(str::to_owned), tier.map(str::to_owned));
                (draft.model, draft.effort, draft.service_tier) = settings;
                Arc::make_mut(&mut next.drafts).insert(key.clone(), Arc::new(draft));
            }
            navigate(&mut next, Navigation {
                cwd,
                draft_key: key,
                ..Default::default()
            });
            let effects = op::review_workspace(&mut next).into_iter().collect();
            return (next, effects);
        }

        Intent::SetFileDraft { path, text } => {
            if previous.workspace.file.as_ref().is_some_and(|file| file.path == path && file.truncated) {
                next.error = Some("ファイルが大きすぎるため編集できません。".into());
                return (next, Vec::new());
            }
            let revision = previous
                .file_drafts
                .get(&path)
                .map(|draft| draft.revision.as_str())
                .or_else(|| {
                    previous
                        .workspace
                        .file
                        .as_ref()
                        .filter(|file| file.path == path)
                        .map(|file| file.revision.as_str())
                });
            if let Some(revision) = revision {
                Arc::make_mut(&mut next.file_drafts).insert(
                    path,
                    Arc::new(FileDraft {
                        revision: revision.into(),
                        text,
                    }),
                );
            } else {
                next.error = Some("file has not been loaded".into());
            }
        }

        Intent::SetDraft { thread_id, draft } => {
            Arc::make_mut(&mut next.drafts).insert(thread_id, Arc::new(draft));
        }
        Intent::EditComposer { thread_id, text, cursor } => {
            let load_catalog = thread_id == previous.navigation.draft_key
                && previous.connected
                && crate::composer::query(&text, cursor as usize).is_some_and(|(_, _, filter)| {
                    crate::composer::should_refresh_catalog(
                        previous.composer_catalog.as_ref()
                            .filter(|catalog| catalog.cwd == previous.navigation.cwd)
                            .map(|catalog| catalog.loading),
                        filter,
                        previous.drafts.get(&thread_id).is_none_or(|draft| draft.text != text),
                    )
                });
            set_draft_text(&mut next, thread_id, text);
            if load_catalog {
                let cwd = next.navigation.cwd.clone();
                return prepare(next, op::LoadComposerCatalog { cwd });
            }
        }
        Intent::InsertInvocation { thread_id, text, invocation } => {
            let draft = Arc::make_mut(Arc::make_mut(&mut next.drafts).entry(thread_id).or_default());
            let token = invocation.token();
            draft.invocations.retain(|item| item.token() != token && item.is_in(&text));
            draft.invocations.push(invocation);
            draft.text = text;
        }
        Intent::SetDraftText { thread_id, text } => set_draft_text(&mut next, thread_id, text),
        Intent::AnswerQuestion { request_id, action } => {
            if let Some(answer) = crate::questions::act(&mut next, request_id.clone(), action) {
                return prepare(next, op::Respond { request_id, answer });
            }
        }
        intent @ (Intent::SelectModel { .. }
        | Intent::SelectEffort { .. }
        | Intent::SelectServiceTier { .. }) => {
            let thread_id = match &intent {
                Intent::SelectModel { thread_id, .. }
                | Intent::SelectEffort { thread_id, .. }
                | Intent::SelectServiceTier { thread_id, .. } => thread_id,
                _ => unreachable!(),
            };
            let mut draft = previous
                .drafts
                .get(thread_id)
                .map(|draft| draft.as_ref().clone())
                .unwrap_or_default();
            let thread_id = match intent {
                Intent::SelectModel { thread_id, model } => {
                    if draft.model.as_ref() != Some(&model) {
                        if draft.model.as_ref().is_none_or(|selected| selected.provider != model.provider)
                        {
                            let settings = previous.model_preferences.selections.get(&model.provider);
                            draft.effort = settings.and_then(|settings| settings.effort.clone());
                            draft.service_tier = settings.and_then(|settings| settings.service_tier.clone());
                        } else {
                            draft.effort = None;
                            draft.service_tier = None;
                        }
                    }
                    draft.model = Some(model);
                    thread_id
                }
                Intent::SelectEffort { thread_id, effort } => {
                    draft.effort = Some(effort);
                    thread_id
                }
                Intent::SelectServiceTier {
                    thread_id,
                    service_tier,
                } => {
                    draft.service_tier = Some(service_tier);
                    thread_id
                }
                _ => unreachable!(),
            };
            if !previous.models.is_empty() {
                let (model, effort, tier) = supported_settings(draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref(), None, &previous.models, &previous.model_errors, previous.allows_custom_codex_models());
                let settings = (
                    model.cloned(),
                    effort.map(str::to_owned),
                    tier.map(str::to_owned),
                );
                (draft.model, draft.effort, draft.service_tier) = settings;
            }
            if let Some(model) = &draft.model {
                next.model_preferences.provider = Some(model.provider);
                next.model_preferences.selections.insert(model.provider, ModelSelection {
                    model: model.clone(),
                    effort: draft.effort.clone(),
                    service_tier: draft.service_tier.clone(),
                });
            }
            draft.model_settings_changed |= previous.drafts.get(&thread_id).is_none_or(|saved| {
                (saved.model.as_ref(), saved.effort.as_deref(), saved.service_tier.as_deref())
                    != (draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref())
            });
            if previous
                .drafts
                .get(&thread_id)
                .is_none_or(|previous| **previous != draft)
            {
                Arc::make_mut(&mut next.drafts).insert(thread_id, Arc::new(draft));
            }
        }
    });
    (next, Vec::new())
}
pub(crate) fn set_draft_text(snapshot: &mut Snapshot, thread_id: DraftKey, text: String) {
    if snapshot
        .drafts
        .get(&thread_id)
        .is_some_and(|draft| draft.text == text)
    {
        return;
    }
    let draft = Arc::make_mut(
        Arc::make_mut(&mut snapshot.drafts)
            .entry(thread_id)
            .or_default(),
    );
    draft.invocations.retain(|item| item.is_in(&text));
    draft.text = text;
}

fn navigate(snapshot: &mut Snapshot, navigation: Navigation) {
    if snapshot.navigation.draft_key != navigation.draft_key {
        Arc::make_mut(&mut snapshot.operations).retain(|key, _| {
            !matches!(
                key,
                op::OperationKey::Directory
                    | op::OperationKey::File
                    | op::OperationKey::SaveFile { .. }
                    | op::OperationKey::WorkspaceReview
            )
        });
        let workspaces = Arc::make_mut(&mut snapshot.workspaces);
        workspaces.insert(
            snapshot.navigation.draft_key.clone(),
            snapshot.workspace.location(snapshot.navigation.cwd.clone()),
        );
        let location = workspaces
            .remove(&navigation.draft_key)
            .filter(|location| location.cwd == navigation.cwd)
            .unwrap_or_default();
        let workspace = Arc::make_mut(&mut snapshot.workspace);
        workspace.file = location.file;
        workspace.review_cwd = location.review_cwd;
        workspace.review = location.review;
    } else if snapshot.navigation.cwd != navigation.cwd || navigation.draft_key.is_empty() {
        clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
    }
    let previous_id = snapshot.navigation.thread_id.clone();
    if previous_id != navigation.thread_id {
        snapshot.observed_agents = None;
    }
    let _ = previous_id
        .filter(|id| navigation.thread_id.as_ref() != Some(id))
        .filter(|id| {
            snapshot.activity.active.get(id) != Some(&true)
                && snapshot
                    .conversations
                    .get(id)
                    .is_none_or(|thread| thread.requests.is_empty())
        })
        .and_then(|id| Arc::make_mut(&mut snapshot.subscriptions).remove(&id));
    if let Some(id) = &navigation.thread_id
        && snapshot.activity.unread.contains(id)
    {
        Arc::make_mut(&mut snapshot.activity).unread.remove(id);
    }
    snapshot.navigation = Arc::new(navigation);
}

fn prepare<O: operations::Operation>(
    mut next: Snapshot,
    mut operation: O,
) -> (Snapshot, Vec<Effect>) {
    if operation.invalidates() {
        next.epoch += 1;
    }
    if let Err(error) = operation.prepare(&mut next) {
        if let Some(key) = operation.key() {
            next.fail_operation(key, error);
        } else {
            next.error = Some(error);
        }
        return (next, Vec::new());
    }
    (next, vec![Effect::execute(operation)])
}

impl Snapshot {
    fn fail_operation(&mut self, key: op::OperationKey, message: String) {
        self.operation_sequence += 1;
        Arc::make_mut(&mut self.operations).insert(
            key,
            op::OperationState {
                generation: self.operation_sequence,
                phase: op::OperationPhase::Failed { message },
            },
        );
    }
}
fn reduce_event(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    match event {
        Event::TerminalFailed { handle, reason } => {
            if previous.connected
                && let Some(terminal) = shared_mut(&mut next.terminals, &handle)
                && matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Running | TerminalPhase::Suspended
                )
            {
                terminal.phase = TerminalPhase::Failed(reason);
            }
        }

        Event::HostTicket(ticket) => {
            next.host_ticket = Some(ticket);
        }
        Event::HostHomeDirectory(home) => {
            next.host_home_directory = home;
        }
        Event::StorageScope(scope) => {
            if next.storage_scope == scope {
                return (next, Vec::new());
            }
            next.project_import = Arc::default();
            next.terminals = Arc::default();
            next.observed_agents = None;
            next.task_activity = None;
            next.activity = Arc::default();
            next.expanded_projects = Arc::default();
            next.project_threads = Arc::default();
            next.unconfirmed_lists = Arc::default();
            if !next.storage_scope.is_empty() {
                let archived = ScopedData {
                    drafts: std::mem::take(&mut next.drafts),
                    pending_submissions: std::mem::take(&mut next.pending_submissions),
                    file_drafts: std::mem::take(&mut next.file_drafts),
                    navigation: std::mem::take(&mut next.navigation),
                };
                Arc::make_mut(&mut next.archived_scopes)
                    .insert(next.storage_scope.clone(), Arc::new(archived));
                if let Some(saved) = Arc::make_mut(&mut next.archived_scopes).remove(&scope) {
                    next.drafts = saved.drafts.clone();
                    next.pending_submissions = saved.pending_submissions.clone();
                    next.file_drafts = saved.file_drafts.clone();
                    next.navigation = saved.navigation.clone();
                }
                next.conversations = Arc::default();
                next.threads = None;
                next.workspace = Arc::default();
                next.workspaces = Arc::default();
                next.draft_sessions = Arc::default();
                next.account = Arc::default();
                next.models = Arc::default();
                next.model_errors = Arc::default();
                next.reset_session();
            }
            next.storage_scope = scope;
        }
        Event::SubmissionUnknown(id) => {
            if let Some(pending) = shared_mut(&mut next.pending_submissions, &id)
                && !pending.accepted
            {
                pending.delivery_unknown = true;
            }
        }
        Event::SubmissionFailed(id) => {
            if let Some(pending) = Arc::make_mut(&mut next.pending_submissions).remove(&id) {
                let draft = Arc::make_mut(
                    Arc::make_mut(&mut next.drafts)
                        .entry(pending.draft_key.clone())
                        .or_default(),
                );
                let mut restored = pending.draft.text.clone();
                append_transcript(&mut restored, &draft.text);
                draft.text = restored;
                for invocation in &pending.draft.invocations {
                    if !draft.invocations.contains(invocation) {
                        draft.invocations.push(invocation.clone());
                    }
                }
                for attachment in &pending.draft.attachments {
                    if !draft
                        .attachments
                        .iter()
                        .any(|current| current.path == attachment.path)
                    {
                        draft.attachments.push(attachment.clone());
                    }
                }
            }
        }
        Event::ConnectionChecked { failure } => next.connection_failure = failure,
        Event::Connected => {
            next.connection_generation = previous.connection_generation.saturating_add(1);
            next.reset_list_revisions();
            next.operations = Arc::default();
            next.reset_session();
            next.connected = true;
            next.task_activity = None;
            next.error = None;
            next.connection_failure = None;
            next.list_query = Arc::new((*next.list_query).clone().for_connection());
            let effects = next.prepare_connection_refresh();
            return (next, effects);
        }
        Event::Disconnected => {
            next.operations = Arc::default();
            next.reset_session();
            next.connected = false;
            for terminal in Arc::make_mut(&mut next.terminals).values_mut() {
                if matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Running
                ) {
                    Arc::make_mut(terminal).phase = TerminalPhase::Suspended;
                }
            }
            next.error = None;
        }
        Event::Intent(_) | Event::Notification(_) | Event::SessionUpdate(_) => {
            unreachable!("handled by the reducer router")
        }
    }
    (next, Vec::new())
}

impl Snapshot {
    fn reset_list_revisions(&mut self) {
        for page in self
            .threads
            .iter_mut()
            .chain(Arc::make_mut(&mut self.project_threads).values_mut())
        {
            if page.revision == 0
                && page.data.iter().all(|row| row.list_revision == 0)
                && page
                    .projects
                    .iter()
                    .all(|project| project.list_revision == 0)
            {
                continue;
            }
            let page = Arc::make_mut(page);
            page.revision = 0;
            for row in &mut page.data {
                row.list_revision = 0;
            }
            for project in &mut page.projects {
                project.list_revision = 0;
            }
        }
        for thread in Arc::make_mut(&mut self.conversations).values_mut() {
            if thread.list_revision != 0 {
                Arc::make_mut(thread).list_revision = 0;
            }
        }
    }

    fn prepare_connection_refresh(&mut self) -> Vec<Effect> {
        let mut effects = vec![Effect::execute(op::ReadTaskActivity {})];
        if self.file_open_capabilities_requested {
            effects.push(Effect::execute(op::ReadFileOpenCapabilities {}));
        }
        effects.extend(op::reconnect_file_trees(self));
        if self.threads.is_none()
            || self
                .unconfirmed_lists
                .contains(&op::OperationKey::SessionList)
        {
            effects.push(Effect::execute(op::ListSessions::new(
                (*self.list_query).clone(),
            )));
        }
        // The Host catalog can change while a mobile client is backgrounded.
        // Keep cached choices visible until the fresh catalog arrives.
        effects.push(Effect::execute(op::LoadModels {}));
        if self.account.accounts.is_none() {
            effects.push(Effect::execute(op::ListAccounts {}));
        }
        effects.extend(op::refresh_agents(
            self.connected,
            self.observed_agents.as_ref(),
        ));
        if let Some(thread_id) = &self.navigation.thread_id {
            effects.push(Effect::execute(op::ReadThread::open(thread_id.clone())));
        } else {
            effects.extend(op::review_workspace(self));
        }
        for (handle, terminal) in self.terminals.iter() {
            if terminal.phase == TerminalPhase::Suspended {
                effects.push(Effect::execute(op::StartTerminal {
                    handle: handle.clone(),
                    cwd: terminal.cwd.clone(),
                    size: terminal.size,
                }));
            }
        }
        effects
    }

    /// Cached history and drafts survive; IDs and activity belong to one connection.
    /// Pending submissions are persisted only to recover dictation after a crash.
    fn reset_session(&mut self) {
        Arc::make_mut(&mut self.workspace)
            .file_metadata
            .disconnected();
        Arc::make_mut(&mut self.workspace).file_trees.disconnected();
        self.file_open_capabilities = None;
        self.composer_catalog = None;
        self.permission_settings = None;
        self.subscriptions = Arc::default();
        for thread in Arc::make_mut(&mut self.conversations).values_mut() {
            if !thread.requests.is_empty() || !thread.submissions.is_empty() {
                let thread = Arc::make_mut(thread);
                thread.requests.clear();
                thread.submissions.clear();
            }
        }
        if !self.activity.active.is_empty() {
            Arc::make_mut(&mut self.activity).active.clear();
        }
        self.epoch += 1;
        for pending in Arc::make_mut(&mut self.pending_submissions).values_mut() {
            Arc::make_mut(pending).delivery_unknown = true;
        }
        if self
            .conversations
            .values()
            .any(|thread| has_session_status(thread))
        {
            for thread in Arc::make_mut(&mut self.conversations).values_mut() {
                if has_session_status(thread) {
                    clear_session_status(Arc::make_mut(thread));
                }
            }
        }
        if self.listed_threads().any(has_session_status) {
            for list in self
                .threads
                .iter_mut()
                .chain(Arc::make_mut(&mut self.project_threads).values_mut())
            {
                if list.data.iter().any(has_session_status) {
                    for thread in &mut Arc::make_mut(list).data {
                        clear_session_status(thread);
                    }
                }
            }
        }
    }
}

fn has_session_status(thread: &Thread) -> bool {
    crate::models::task_active(None, thread.status)
        || thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
}
fn clear_session_status(thread: &mut Thread) {
    if crate::models::task_active(None, thread.status) {
        thread.status = crate::models::SessionStatus::Unknown;
    }
    for turn in thread.turns.iter_mut().flatten() {
        if turn.status == agent_protocol::execution::TurnStatus::Running {
            Arc::make_mut(turn).status = crate::models::TurnStatus::Unknown;
        }
    }
}

pub(crate) fn allows_custom_codex_models(
    accounts: Option<&agent_protocol::operations::Accounts>,
) -> bool {
    let Some(accounts) = accounts else {
        return true;
    };
    let provider = crate::session::ProviderKind::Codex;
    let account = accounts.selected.get(&provider).and_then(|id| {
        accounts
            .accounts
            .iter()
            .find(|account| account.provider == provider && &account.id == id)
    });
    account
        .and_then(|account| account.plan_type.as_deref())
        .is_none_or(|plan| plan == "API key")
}

impl Snapshot {
    pub(crate) fn normalize_model_drafts(&mut self) {
        let remembered = self
            .model_preferences
            .provider
            .and_then(|provider| self.model_preferences.selections.get(&provider));
        let allow_custom_codex = self.allows_custom_codex_models();
        let mut next = self.drafts.clone();
        for (key, previous) in self.drafts.iter() {
            // Let the first native read seed a conversation before catalog defaults.
            if previous.model.is_none()
                && let DraftKey::Session { session } = key
                && !self.conversations.contains_key(session)
            {
                continue;
            }
            let selected_provider = self.draft_provider(key, previous.model.as_ref());
            if selected_provider.is_none() && key != &self.navigation.draft_key {
                continue;
            }
            let selection = selected_provider.is_none().then_some(remembered).flatten();
            let provider = selected_provider
                .or_else(|| selection.map(|selection| selection.model.provider))
                .or_else(|| {
                    crate::presentation::model_settings::automatic_provider(
                        &self.models,
                        self.account
                            .accounts
                            .as_ref()
                            .map(|accounts| &accounts.selected),
                        &self.model_errors,
                    )
                });
            let Some(provider) = provider else {
                continue;
            };
            let (model, effort, tier) = supported_settings(
                previous
                    .model
                    .as_ref()
                    .or_else(|| selection.map(|selection| &selection.model)),
                previous
                    .effort
                    .as_deref()
                    .or_else(|| selection.and_then(|selection| selection.effort.as_deref())),
                previous
                    .service_tier
                    .as_deref()
                    .or_else(|| selection.and_then(|selection| selection.service_tier.as_deref())),
                Some(provider),
                &self.models,
                &self.model_errors,
                allow_custom_codex,
            );
            if (model, effort, tier)
                != (
                    previous.model.as_ref(),
                    previous.effort.as_deref(),
                    previous.service_tier.as_deref(),
                )
            {
                let draft = shared_mut(&mut next, key).expect("draft came from this map");
                draft.model = model.cloned();
                draft.effort = effort.map(str::to_owned);
                draft.service_tier = tier.map(str::to_owned);
            }
        }
        self.drafts = next;
    }
}

pub(crate) fn supported_settings<'a>(
    selected_model: Option<&'a crate::models::ModelRef>,
    selected_effort: Option<&'a str>,
    selected_tier: Option<&'a str>,
    default_provider: Option<crate::session::ProviderKind>,
    models: &'a [Model],
    catalog_errors: &serde_json::Map<String, serde_json::Value>,
    allow_custom_codex: bool,
) -> (
    Option<&'a crate::models::ModelRef>,
    Option<&'a str>,
    Option<&'a str>,
) {
    let provider = selected_model
        .filter(|model| !model.id.is_empty())
        .map(|model| model.provider)
        .or(default_provider);
    // Codex also accepts explicit IDs outside its recommended picker catalog.
    // Absence in an incomplete catalog cannot invalidate a saved choice either.
    if (provider.is_some_and(|provider| catalog_errors.contains_key(provider.key()))
        || selected_model.is_some_and(|model| {
            allow_custom_codex
                && model.provider == crate::session::ProviderKind::Codex
                && !model.id.is_empty()
        }))
        && selected_model.is_some()
        && !models
            .iter()
            .any(|model| Some(&model.model) == selected_model)
    {
        return (selected_model, selected_effort, selected_tier);
    }
    let mut available = models
        .iter()
        .filter(|model| provider.is_none_or(|provider| model.model.provider == provider));
    let model = available
        .clone()
        .find(|model| Some(&model.model) == selected_model)
        .or_else(|| {
            available
                .clone()
                .find(|model| model.is_default == Some(true))
        })
        .or_else(|| available.next());
    let Some(model) = model else {
        // Missing catalog data cannot validate this draft's options.
        return (selected_model, selected_effort, selected_tier);
    };
    let Some(settings) = &model.settings else {
        return (Some(&model.model), selected_effort, selected_tier);
    };
    let changed = selected_model.is_some() && selected_model != Some(&model.model);
    let (effort, tier) = supported_model_options(
        settings,
        selected_effort.filter(|_| !changed),
        selected_tier.filter(|_| !changed),
    );
    (Some(&model.model), effort, Some(tier))
}

pub(crate) fn supported_model_options<'a>(
    settings: &'a crate::models::ModelSettings,
    selected_effort: Option<&'a str>,
    selected_tier: Option<&'a str>,
) -> (Option<&'a str>, &'a str) {
    let effort = settings
        .supported_reasoning_efforts
        .iter()
        .find(|effort| Some(effort.reasoning_effort.as_str()) == selected_effort)
        .or_else(|| {
            settings
                .supported_reasoning_efforts
                .iter()
                .find(|effort| effort.reasoning_effort == settings.default_reasoning_effort)
        })
        .or_else(|| settings.supported_reasoning_efforts.first());
    let supported_tier = |id: &str| {
        id == "default"
            || settings
                .service_tiers
                .as_ref()
                .is_some_and(|tiers| tiers.iter().any(|tier| tier.id == id))
    };
    let tier = selected_tier
        .filter(|tier| supported_tier(tier))
        .or_else(|| {
            settings
                .default_service_tier
                .as_deref()
                .filter(|tier| supported_tier(tier))
        })
        .unwrap_or("default");
    (effort.map(|effort| effort.reasoning_effort.as_str()), tier)
}

fn shared_mut<'a, K: Ord + Clone + std::borrow::Borrow<Q>, Q: Ord + ?Sized, T: Clone>(
    values: &'a mut Arc<BTreeMap<K, Arc<T>>>,
    key: &Q,
) -> Option<&'a mut T> {
    if !values.contains_key(key) {
        return None;
    }
    Arc::make_mut(values).get_mut(key).map(Arc::make_mut)
}

fn upsert_item(
    previous: &Snapshot,
    thread_id: &crate::session::SessionRef,
    turn_id: &agent_protocol::ids::TurnId,
    item: Item,
) -> Snapshot {
    let mut next = previous.clone();
    if let Some(thread) = previous.conversations.get(thread_id) {
        match (crate::session::SessionChange::Item {
            turn_id: turn_id.clone(),
            item: item.into(),
        })
        .apply(thread)
        {
            Ok(thread) => {
                Arc::make_mut(&mut next.conversations).insert(thread_id.clone(), Arc::new(thread));
            }
            Err(error) => next.error = Some(error.to_string()),
        }
    }
    next
}

// Publish sent content immediately as pending input. Later receipts and echoes
// must not clear newer draft edits. Restore definite failures alongside newer
// input; keep unknown delivery visible and ordered without automatic resubmission.
fn submission(
    previous: &Snapshot,
    thread_id: Option<crate::session::SessionRef>,
    draft_key: DraftKey,
    draft: Arc<Draft>,
    client_user_message_id: agent_protocol::ids::ClientInputId,
    clear_draft: Option<Arc<Draft>>,
) -> (Snapshot, Vec<Effect>) {
    let provider = previous
        .draft_provider(&draft_key, draft.model.as_ref())
        .or_else(|| previous.model_provider_for_draft(draft_key.clone()));
    if thread_id.is_none() && provider.is_none() {
        let mut next = previous.clone();
        next.fail_operation(
            op::OperationKey::Submission {
                draft_key: draft_key.clone(),
            },
            "利用可能なエージェントを選択してください。".into(),
        );
        return (next, Vec::new());
    }
    let mut next = previous.clone();
    if let Some(reason) = thread_id
        .as_ref()
        .and_then(|id| {
            previous
                .thread_metadata(id)
                .map(|thread| thread.can_accept_direct_input)
        })
        .and_then(crate::session::direct_input_unavailable_reason)
    {
        next.fail_operation(
            op::OperationKey::Submission {
                draft_key: draft_key.clone(),
            },
            reason,
        );
        return (next, Vec::new());
    }
    let cleared = clear_draft.as_ref().unwrap_or(&draft);
    if let Some(current) = shared_mut(&mut next.drafts, &draft_key) {
        if current.text == cleared.text {
            current.text.clear();
            current.invocations.clear();
        }
        current.attachments.retain(|attachment| {
            !cleared
                .attachments
                .iter()
                .any(|sent| sent.path == attachment.path)
        });
    }
    let anchor = thread_id
        .as_ref()
        .and_then(|id| previous.conversations.get(id))
        .and_then(|thread| thread.turns.as_ref())
        .and_then(|turns| turns.last());
    let sequence = previous
        .pending_submissions
        .values()
        .map(|pending| pending.sequence)
        .max()
        .map_or(0, |last| last + 1);
    Arc::make_mut(&mut next.pending_submissions).insert(
        client_user_message_id.clone(),
        Arc::new(PendingSubmission {
            sequence,
            submitted_at_ms: None,
            draft_key: draft_key.clone(),
            draft: draft.clone(),
            turn_id: anchor.map(|turn| turn.id.clone()),
            after_item_id: anchor
                .and_then(|turn| turn.items.as_ref())
                .and_then(|items| items.last())
                .map(|item| item.id.clone()),
            accepted: false,
            delivery_unknown: false,
        }),
    );
    let effect = match thread_id {
        Some(thread_id) => Effect::execute(op::SendSubmission {
            thread_id,
            client_user_message_id,
            draft,
        }),
        None => Effect::execute(op::StartSubmission {
            provider: provider.expect("new submission has a provider"),
            draft_key,
            cwd: (!previous.navigation.cwd.trim().is_empty())
                .then(|| previous.navigation.cwd.clone()),
            client_user_message_id,
            draft,
        }),
    };
    (next, vec![effect])
}

#[cfg(test)]
mod workspace_navigation_tests {
    use super::*;
    #[test]
    fn same_directory_conversations_restore_their_own_workspace_without_persisting_host_results() {
        let a = DraftKey::from("a");
        let b = DraftKey::from("b");
        let navigation = |key: DraftKey| Navigation {
            draft_key: key,
            cwd: "/project".into(),
            ..Default::default()
        };
        let mut state = Snapshot {
            navigation: Arc::new(navigation(a.clone())),
            ..Default::default()
        };
        Arc::make_mut(&mut state.workspace).review_cwd = Some("a-review".into());
        navigate(&mut state, navigation(b.clone()));
        assert!(state.workspace.review_cwd.is_none());
        Arc::make_mut(&mut state.workspace).review_cwd = Some("b-review".into());
        navigate(&mut state, navigation(a));
        assert_eq!(state.workspace.review_cwd.as_deref(), Some("a-review"));
        navigate(&mut state, navigation(b));
        assert_eq!(state.workspace.review_cwd.as_deref(), Some("b-review"));
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&state).unwrap()).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.workspace, Arc::default());
    }
}

#[cfg(test)]
mod submission_tests {
    use super::*;

    #[rstest::rstest]
    fn read_only_subagents_keep_drafts_without_dispatching_input(
        #[values(None, Some(false), Some(true))] can_accept_direct_input: Option<bool>,
        #[values(false, true)] loaded: bool,
    ) {
        let id =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "child".into())
                .unwrap();
        let key = DraftKey::from(id.clone());
        let thread = crate::models::Thread {
            id: Some(id.clone()),
            can_accept_direct_input,
            ..Default::default()
        };
        let snapshot = Snapshot {
            conversations: Arc::new(if loaded {
                [(id.clone(), Arc::new(thread.clone()))].into()
            } else {
                Default::default()
            }),
            threads: Some(Arc::new(crate::models::ThreadList {
                revision: 0,
                data: vec![thread],
                projects: vec![],
                has_more: false,
                has_more_projects: false,
                limit: 5,
                project_pages: Default::default(),
                provider_errors: None,
            })),
            drafts: Arc::new(
                [(
                    key.clone(),
                    Arc::new(Draft {
                        text: "Keep my draft".into(),
                        ..Default::default()
                    }),
                )]
                .into(),
            ),
            ..Default::default()
        };
        let (next, effects) = reduce_intent(
            &snapshot,
            Intent::Submit {
                thread_id: Some(id),
                client_user_message_id: "input".into(),
            },
        );
        if can_accept_direct_input == Some(false) {
            assert_eq!(next.drafts[&key].text, "Keep my draft");
            assert!(next.pending_submissions.is_empty());
            assert!(effects.is_empty());
            assert!(
                next.operation_error(op::OperationKey::Submission { draft_key: key })
                    .unwrap()
                    .message
                    .contains("閲覧専用")
            );
        } else {
            assert!(next.drafts[&key].text.is_empty());
            assert_eq!(next.pending_submissions.len(), 1);
            assert_eq!(effects.len(), 1);
        }
    }
}

fn append_transcript(text: &mut String, transcript: &str) {
    if transcript.is_empty() {
        return;
    }
    text.reserve(transcript.len() + usize::from(!text.is_empty()));
    if !text.is_empty() && !text.ends_with(char::is_whitespace) {
        text.push('\n');
    }
    text.push_str(transcript);
}

fn reconcile_pending(snapshot: &mut Snapshot, thread_id: &crate::session::SessionRef) {
    if snapshot.pending_submissions.is_empty() {
        return;
    }
    let Some(thread) = snapshot.conversations.get(thread_id) else {
        return;
    };
    let deliveries: Vec<_> = snapshot
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == DraftKey::from(thread_id))
        .filter_map(|(id, _)| {
            thread
                .submissions
                .get(id)
                .map(|delivery| (id.clone(), delivery.clone()))
        })
        .collect();
    for (id, delivery) in deliveries {
        use crate::session::SubmissionDelivery;
        if delivery == SubmissionDelivery::Rejected {
            *snapshot = reduce_event(snapshot, Event::SubmissionFailed(id)).0;
            continue;
        }
        let mut pending = (*snapshot.pending_submissions[&id]).clone();
        match delivery {
            SubmissionDelivery::Sending => pending.delivery_unknown = false,
            SubmissionDelivery::Accepted { turn_id } => {
                if let Some(draft) = shared_mut(&mut snapshot.drafts, &pending.draft_key)
                    && draft.model == pending.draft.model
                    && draft.effort == pending.draft.effort
                    && draft.service_tier == pending.draft.service_tier
                {
                    draft.model_settings_changed = false;
                }
                pending.accepted = true;
                pending.delivery_unknown = false;
                if turn_id.is_some() && pending.turn_id != turn_id {
                    pending.turn_id = turn_id;
                    pending.after_item_id = None;
                }
            }
            SubmissionDelivery::Unknown => pending.delivery_unknown = true,
            SubmissionDelivery::Rejected => unreachable!(),
        }
        if pending != *snapshot.pending_submissions[&id] {
            Arc::make_mut(&mut snapshot.pending_submissions).insert(id, Arc::new(pending));
        }
    }
    let thread = &snapshot.conversations[thread_id];
    let echoed = |id: &agent_protocol::ids::ClientInputId, pending: &Arc<PendingSubmission>| {
        pending.draft_key == DraftKey::from(thread_id)
            && thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .any(|item| {
                    matches!(item.body(), crate::models::ItemBody::UserMessage { .. })
                        && item.client_input_id.as_ref() == Some(id)
                })
    };
    if snapshot
        .pending_submissions
        .iter()
        .any(|(id, pending)| echoed(id, pending))
    {
        Arc::make_mut(&mut snapshot.pending_submissions).retain(|id, pending| !echoed(id, pending));
    }
}

#[cfg(test)]
#[path = "state/model_selection_tests.rs"]
mod model_selection_tests;

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::session::SessionRef;
    use operations::{OperationKey, OperationPhase, OperationState};

    #[test]
    fn older_history_deduplicates_only_a_running_request_for_the_same_session() {
        let session =
            SessionRef::new(crate::session::ProviderKind::Codex, "history".into()).unwrap();
        let other = SessionRef::new(crate::session::ProviderKind::Codex, "other".into()).unwrap();
        for cursor in [None, Some("older".to_owned())] {
            for (owner, phase, blocked) in [
                (session.clone(), OperationPhase::Running, true),
                (other.clone(), OperationPhase::Running, false),
                (
                    session.clone(),
                    OperationPhase::Failed {
                        message: "retry".into(),
                    },
                    false,
                ),
            ] {
                let snapshot = Snapshot {
                    connected: true,
                    conversations: Arc::new(
                        [(
                            session.clone(),
                            Arc::new(Thread {
                                id: Some(session.clone()),
                                history_cursor: cursor.clone(),
                                history_has_more: Some(true),
                                ..Default::default()
                            }),
                        )]
                        .into(),
                    ),
                    operations: Arc::new(
                        [(
                            OperationKey::History { session: owner },
                            OperationState {
                                generation: 1,
                                phase,
                            },
                        )]
                        .into(),
                    ),
                    ..Default::default()
                };
                let (next, effects) = reduce(
                    &snapshot,
                    Event::Intent(Intent::ReadOlder {
                        thread_id: session.clone(),
                    }),
                );
                if blocked {
                    assert_eq!(next, snapshot);
                    assert!(
                        effects.is_empty(),
                        "A pending page cannot be requested twice"
                    );
                } else {
                    assert_eq!(
                        effects.len(),
                        1,
                        "A failed request must be retryable independently of other sessions"
                    );
                }
            }
        }
    }
}
