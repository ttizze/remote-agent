//! Immutable client state and pure conversation transitions.
use crate::models::{
    FileContent, HostStatus, Invitation, ListQuery, Model, RemoteHost, Thread, ThreadList,
    WorkspaceReview, WorktreeSettings,
};
use agent_protocol::requests::{Answer, Request};
use operations as op;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    #[cfg_attr(feature = "bindings", uniffi(default = None))]
    pub workspace: Option<agent_protocol::workspace::WorkspaceStrategy>,
    pub text: String,
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub invocations: Vec<agent_protocol::composer::Invocation>,
    #[serde(default)]
    pub contexts: Vec<agent_protocol::context::MessageContext>,
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
    pub options: HashMap<String, Arc<agent_protocol::workspace::WorkspaceOptions>>,
    #[serde(skip)]
    pub file_metadata: crate::presentation::files::FileMetadataCache,
    #[serde(skip)]
    pub file_trees: crate::presentation::files::FileTrees,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<crate::models::Worktree>>>,
}
/// Each pane owns its navigation and workspace selection; resources stay shared.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub navigation: Arc<Navigation>,
    // Stable draft slots are reclaimed when a pane reopens. Operation identity
    // remains the unique runtime pane ID, so old responses cannot reach it.
    #[serde(skip)]
    pub draft_slot: u64,
    #[serde(default, rename = "workspace_selection")]
    pub workspace: Arc<WorkspaceLocation>,
    #[serde(skip)]
    pub workspaces: Arc<BTreeMap<DraftKey, WorkspaceLocation>>,
    #[serde(skip)]
    pub observed_agents: Option<crate::session::SessionRef>,
    #[serde(skip)]
    pub epoch: u64,
    #[serde(skip)]
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLocation {
    pub cwd: String,
    pub file: Option<Arc<FileContent>>,
    pub review_cwd: Option<String>,
    pub comparison: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
}
impl WorkspaceLocation {
    fn location(&self, cwd: String) -> Self {
        Self {
            cwd,
            ..self.clone()
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
    fn new_chat(cwd: &str, slot: u64) -> Self {
        Self::Local {
            key: if slot == 0 {
                format!("new:{cwd}")
            } else {
                format!("new:{slot}:{cwd}")
            },
        }
    }
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
    /// The draft that adopts the account the pending login adds.
    #[serde(skip)]
    pub login_draft: Option<DraftKey>,
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
    /// Sent to wait behind a running turn: the queue above the composer shows
    /// it until the turn it starts shows it.
    #[serde(default)]
    pub queued: bool,
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
    pub fn accepts_input(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TerminalTarget {
    Shell {
        command: Option<String>,
        cwd: String,
    },
    Account {
        provider: agent_protocol::session::ProviderKind,
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub target: TerminalTarget,
    pub size: agent_protocol::operations::TerminalSize,
    pub phase: TerminalPhase,
    pub output: VecDeque<Arc<TerminalOutput>>,
    pub sequence: u64,
}
#[derive(Debug, Clone, PartialEq)]
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

/// Identity of a published value. Shared allocations compare by pointer; small
/// values compare by equality. Groups of state are "the same" when every field is.
pub trait Same {
    fn same(&self, other: &Self) -> bool;
}
impl<T: ?Sized> Same for Arc<T> {
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(self, other)
    }
}
impl<T: Same> Same for Option<T> {
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        }
    }
}
macro_rules! same_by_eq {
    ($($type:ty),* $(,)?) => {
        $(impl Same for $type {
            fn same(&self, other: &Self) -> bool {
                self == other
            }
        })*
    };
}
same_by_eq!(
    bool,
    u64,
    String,
    ModelPreferences,
    crate::localization::UiLanguage,
    crate::presentation::web_links::WebLinkTarget,
    follow_up::ComposerPreferences,
    agent_protocol::editors::EditorId,
    crate::session::SessionRef,
);

/// State fields and their shared-reference comparison are declared together.
macro_rules! group {
    ($(#[$meta:meta])* pub struct $name:ident { $($(#[$field_meta:meta])* pub $field:ident : $type:ty,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $type,)*
        }
        impl Same for $name {
            fn same(&self, other: &Self) -> bool {
                true $(&& self.$field.same(&other.$field))*
            }
        }
    };
}

pub use operations::OperationStates as Operations;

group! {
    /// The Host connection and facts the Host states about itself.
    pub struct Connection {
        #[serde(default)]
        pub connection_disabled: bool,
        #[serde(skip)]
        pub connected: bool,
        #[serde(default)]
        pub epoch: u64,
        #[serde(skip)]
        pub connection_generation: u64,
        /// Why the latest connection attempt failed. Losing an established
        /// connection is not a failure, for example a socket the OS closed while
        /// the app was suspended: its replacement reports none until it fails.
        #[serde(skip)]
        pub connection_failure: Option<String>,
        #[serde(skip)]
        pub host_name: Option<String>,
        /// The Host's latest reachable addresses; platforms save them for cold connections.
        #[serde(skip)]
        pub host_ticket: Option<String>,
        /// Retained while offline, but never inferred from workspace paths or persisted.
        #[serde(skip)]
        pub host_home_directory: Option<String>,
        pub storage_scope: String,
        #[serde(default)]
        pub management: Arc<HostManagement>,
    }
}

group! {
    /// Host catalogs that control what the composer and settings offer.
    pub struct Catalog {
        #[serde(skip)]
        pub provider_settings: Option<Arc<agent_protocol::providers::ProviderSettings>>,
        #[serde(skip)]
        pub host_software: Option<Arc<agent_protocol::operations::HostSoftware>>,
        #[serde(default)]
        pub models: Arc<Vec<Model>>,
        #[serde(default)]
        pub model_errors: Arc<Map<String, Value>>,
        #[serde(default)]
        pub model_preferences: ModelPreferences,
        #[serde(default)]
        pub ui_language: crate::localization::UiLanguage,
        #[serde(skip)]
        pub web_link_target: crate::presentation::web_links::WebLinkTarget,
        #[serde(default)]
        pub account: Arc<AccountState>,
        #[serde(skip)]
        pub permission_settings: Arc<BTreeMap<crate::session::ProviderKind, Arc<op::PermissionSettingsState>>>,
        #[serde(skip)]
        pub composer_catalogs: Arc<BTreeMap<String, Arc<agent_protocol::composer::ComposerCatalog>>>,
    }
}

group! {
    /// Conversation lists, projects and the activity shown beside them.
    pub struct Lists {
        #[serde(default)]
        pub threads: Option<Arc<ThreadList>>,
        #[serde(skip)]
        pub project_threads: Arc<BTreeMap<String, Arc<ThreadList>>>,
        #[serde(skip)]
        pub expanded_projects: Arc<BTreeMap<String, u32>>,
        #[serde(skip)]
        pub list_query: Arc<ListQuery>,
        pub activity: Arc<Activity>,
        #[serde(skip)]
        pub task_activity: Option<Arc<agent_protocol::live_activity::TaskActivityState>>,
        /// Device-owned organization, scoped by Host storage identity and registered project ID.
        #[serde(default)]
        pub project_preferences: Arc<BTreeMap<String, BTreeMap<String, ProjectPreferences>>>,
        pub archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
        /// Mobile recent feed loads project pages independently of expansion.
        #[serde(skip)]
        pub recent_chats: bool,
        /// Disk copies need one authoritative read; live lists continue through notifications.
        #[serde(skip)]
        pub unconfirmed_lists: Arc<BTreeSet<op::OperationKey>>,
    }
}

group! {
    /// Open conversations and the input they have accepted or are still receiving.
    pub struct Sessions {
        #[serde(default)]
        #[serde(with = "crate::persistence::entries")]
        pub conversations: Arc<BTreeMap<crate::session::SessionRef, Arc<Thread>>>,
        #[serde(skip)]
        pub subscriptions: Arc<BTreeMap<crate::session::SessionRef, uuid::Uuid>>,
        pub pending_submissions:
            Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
        /// Startup identity changes outlive pending sends, which subscribers may coalesce.
        #[serde(skip)]
        pub draft_sessions: Arc<BTreeMap<DraftKey, crate::session::SessionRef>>,
    }
}

group! {
    /// Text and attachments typed on this device and not yet handed to the Host.
    pub struct Input {
        #[serde(with = "crate::persistence::entries")]
        pub drafts: Arc<BTreeMap<DraftKey, Arc<Draft>>>,
        #[serde(default)]
        pub stashes: Arc<Vec<crate::composer::stash::StashedPrompt>>,
        /// Answers composed for awaiting questions; local to this device.
        #[serde(skip)]
        pub question_drafts:
            Arc<BTreeMap<agent_protocol::ids::RequestId, crate::questions::QuestionDraft>>,
        /// This device's composer behavior; device preferences restore it.
        #[serde(skip)]
        pub composer_preferences: follow_up::ComposerPreferences,
        /// Queued messages being edited in their conversation's composer.
        #[serde(skip)]
        pub queued_edits: Arc<BTreeMap<crate::session::SessionRef, follow_up::QueuedEdit>>,
    }
}

group! {
    /// Shared files, review and editor state.
    pub struct Files {
        pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
        #[serde(default)]
        pub workspace: Arc<Workspace>,
        #[serde(skip)]
        pub preferred_editor: Option<agent_protocol::editors::EditorId>,
        #[serde(skip)]
        pub file_open_capabilities: Option<Arc<agent_protocol::editors::FileOpenCapabilities>>,
        /// Desktop opts into this Host catalog; keep the subscription across reconnects.
        #[serde(skip)]
        pub file_open_capabilities_requested: bool,
    }
}

// State fields and their publication contract share one declaration.
macro_rules! snapshot {
    ($($(#[$meta:meta])* $field:ident: $type:ty,)*) => {
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        #[cfg_attr(feature = "bindings", derive(uniffi::Record))]
        pub struct Changed {
            pub any: bool,
            $(pub $field: bool,)*
        }
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[cfg_attr(feature = "bindings", derive(uniffi::Object))]
        pub struct Snapshot {
            $($(#[$meta])* pub $field: $type,)*
        }
        impl Changed {
            pub(crate) fn selected_pane(mut self, changed: bool) -> Self {
                self.panes = changed;
                self.any = false $(|| self.$field)*;
                self
            }
        }
        impl Snapshot {
            pub fn changes_from(&self, previous: &Self) -> Changed {
                let mut changed = Changed {
                    any: false,
                    $($field: !self.$field.same(&previous.$field),)*
                };
                changed.any = false $(|| changed.$field)*;
                changed
            }
        }
    };
}
snapshot! {
    #[serde(flatten)] connection: Connection,
    #[serde(flatten)] catalog: Catalog,
    #[serde(flatten)] lists: Lists,
    #[serde(flatten)] sessions: Sessions,
    #[serde(flatten)] input: Input,
    #[serde(flatten)] files: Files,
    #[serde(flatten)] panes: PaneStates,
    #[serde(skip)] terminals: Arc<BTreeMap<String, Arc<Terminal>>>,
    #[serde(skip)] project_import: Arc<ProjectImportState>,
    #[serde(skip)] operations: Operations,
    #[serde(skip)] operation_sequence: u64,
}

#[cfg(test)]
mod input_tests;
mod panes;
pub use panes::PaneStates;
#[cfg(test)]
mod metadata_tests;

/// Cache unresolved batch lookups, including absent rows. Read conversations keep
/// their existing priority; listed duplicates keep their first occurrence.
pub(crate) fn index_missing_metadata<'a>(
    conversations: &BTreeMap<crate::session::SessionRef, Arc<Thread>>,
    listed: impl Iterator<Item = &'a Thread>,
    requested: impl Iterator<Item = &'a crate::session::SessionRef>,
) -> HashMap<&'a crate::session::SessionRef, Option<&'a Thread>> {
    let mut indexed: HashMap<_, Option<&Thread>> = requested
        .filter(|id| !conversations.contains_key(*id))
        .map(|id| (id, None))
        .collect();
    let mut missing = indexed.len();
    for thread in listed {
        if missing == 0 {
            break;
        }
        if let Some(entry) = thread.id.as_ref().and_then(|id| indexed.get_mut(id))
            && entry.is_none()
        {
            *entry = Some(thread);
            missing -= 1;
        }
    }
    indexed
}

impl Snapshot {
    pub(crate) fn listed_threads(&self) -> impl Iterator<Item = &Thread> {
        self.lists
            .threads
            .iter()
            .chain(self.lists.project_threads.values())
            .flat_map(|page| &page.data)
    }

    pub fn thread_metadata(&self, id: &crate::session::SessionRef) -> Option<&Thread> {
        self.sessions
            .conversations
            .get(id)
            .map(AsRef::as_ref)
            .or_else(|| {
                self.listed_threads()
                    .find(|thread| thread.id.as_ref() == Some(id))
            })
    }

    pub fn requests(&self) -> impl Iterator<Item = &Arc<Request>> {
        self.sessions
            .conversations
            .values()
            .flat_map(|thread| thread.requests.values())
    }

    pub fn request(&self, id: &str) -> Option<&Arc<Request>> {
        self.sessions
            .conversations
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
                operations::OperationPhase::Failed { message } => Some(
                    crate::presentation::error::error_text(self.catalog.ui_language, message),
                ),
                _ => None,
            })
    }
    pub fn terminal_accepts_input(&self, handle: String) -> bool {
        self.terminals
            .get(&handle)
            .is_some_and(|terminal| terminal.phase.accepts_input())
    }
    pub fn terminals(&self) -> Arc<crate::presentation::terminals::Terminals> {
        Arc::new(crate::presentation::terminals::Terminals {
            states: self.terminals.clone(),
        })
    }
}

mod notifications;
use notifications::notification;
pub mod projects;
pub use projects::ProjectPreferences;
pub mod follow_up;
pub use follow_up::{ComposerPreferences, SendShortcut};
pub mod operations;
pub use operations::Intent;

/// Client-owned data from a previously configured Host storage area. Keeping
/// it separate prevents storage switches from deleting or mixing user drafts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScopedData {
    #[serde(with = "crate::persistence::entries")]
    pub drafts: Arc<BTreeMap<DraftKey, Arc<Draft>>>,
    #[serde(default)]
    pub stashes: Arc<Vec<crate::composer::stash::StashedPrompt>>,
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
fn clear_workspace_location(workspace: &mut WorkspaceLocation) {
    workspace.file = None;
    workspace.review = None;
    workspace.review_cwd = None;
    workspace.comparison = None;
}

pub fn reduce(previous: &Snapshot, pane_id: &str, event: Event) -> (Snapshot, Vec<Effect>) {
    let (mut next, mut effects) = match event {
        Event::Intent(intent) => reduce_intent(previous, pane_id, intent),
        Event::Notification(message) => notification(previous, message),
        Event::SessionUpdate(update) => notifications::session_update(previous, *update),
        event => reduce_event(previous, event),
    };
    let before = previous
        .panes
        .get(pane_id)
        .and_then(|pane| pane.navigation.thread_id.as_ref());
    let selected = next
        .panes
        .get(pane_id)
        .and_then(|pane| pane.navigation.thread_id.as_ref());
    if next.connection.connected
        && before != selected
        && let Some(selected) = selected
        && let Some((session, turn_id)) = next
            .lists
            .task_activity
            .as_ref()
            .and_then(|state| state.unread.iter().find(|(id, _)| id == selected))
    {
        effects.push(Effect::execute(op::MarkSessionRead {
            session: session.clone(),
            turn_id: turn_id.clone(),
        }));
    }
    effects.extend(op::prefetch_composer_catalog(
        next.connection.connected,
        &mut next.catalog.composer_catalogs,
        next.panes
            .0
            .iter()
            .map(|(id, pane)| (id.as_str(), pane.navigation.as_ref())),
    ));
    effects.extend(op::prefetch_model_settings(
        next.connection.connected,
        next.panes
            .get(pane_id)
            .and_then(|pane| next.input.drafts.get(&pane.navigation.draft_key))
            .and_then(|draft| draft.model.as_ref()),
        &next.catalog.models,
        next.operations.keys(),
    ));
    (next, effects)
}
macro_rules! prepare_operations {
    ($intent:expr, $next:ident, $pane:expr, [$($variant:ident),* $(,)?], {$($local:tt)*}) => {
        match $intent {
            $(Intent::$variant(operation) => return prepare($next, $pane, operation),)*
            $($local)*
        }
    };
}

fn reduce_intent(previous: &Snapshot, pane_id: &str, intent: Intent) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    let Some(pane) = previous.panes.get(pane_id) else {
        return (next, Vec::new());
    };
    prepare_operations!(intent, next, pane_id, [
        ReadTaskActivity, RegisterLiveActivity, UnregisterLiveActivity, ReadHostSoftware,
        ReadPermissionSettings, UpdatePermissionSettings,
        ManageCodexCli, ReadProviderSettings, SetProviderEnabled, ReadDshCredentials, UpdateDshCredential, ListAccounts, SelectAccount, SelectAccountForDraft, LogoutAccount, StartAccountLogin,
        ReadAccountLogin, CancelAccountLogin, SubmitAccountLogin, ForkSession, RewindSession,
        StartTerminal, StartAccountTerminal, DetachTerminal, KillTerminal, CreateInvitation, RemoveRemoteHost,
        RevokeDevice, ListFiles, ResolveFile, ResolveFileReference, ReadFile,
        ReadFileOpenCapabilities, OpenInEditor,
        SaveFile, WriteWorkspaceFile, ReviewWorkspace, ChangeWorkspace, ReadWorktreeSettings,
        UpdateWorktreeSettings, ListWorktrees, RemoveWorktree, ListProjectSessions,
        ListAgents, AddProject, ScanProjects, ImportProjects, CreateSession, RetryPreparation,
        ReadThread, OpenRequest, ReadItem, ResizeTerminal,
        Interrupt,
        WriteTerminal, DownloadFile, LoadSessionImages, LoadVisualization,
        LoadHostName, LoadHostManagement, LoadModels, ReadModelSettings,
        Respond, Transcribe, UploadAttachment, PairRemoteHost,
    ], {
        Intent::RefreshModels => return (next, op::refresh_models()),
        Intent::SetConnectionEnabled { enabled } => next.connection.connection_disabled = !enabled,
        // Store commits this device-owned value after admitting the action.
        Intent::SetPreferredEditor { .. } => (),
        Intent::EnsureFileMetadata { paths, now_ms } => {
            let effects = op::ensure_file_metadata(&mut next, paths, now_ms);
            return (next, effects);
        }
        Intent::FileTree { key, action } => {
            let effects = op::file_tree_action(&mut next, key, action);
            return (next, effects);
        }
        Intent::RecoverError { id } => {
            if let Some(recovery) = previous.error_recovery(id) {
                return reduce_intent(previous, pane_id, recovery.intent);
            }
        },
        Intent::DismissError { id } => match id {
            op::ErrorId::Operation { key, generation } => {
                if next.operations.get(&key).is_some_and(|state| {
                    state.generation == generation && matches!(state.phase, op::OperationPhase::Failed { .. })
                }) { Arc::make_mut(&mut next.operations).remove(&key); }
            }
            op::ErrorId::Application { message } => {
                let pane = Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists"));
                for error in [&mut next.connection.connection_failure, &mut pane.error] {
                    if error.as_deref() == Some(message.as_str()) {
                        *error = None;
                    }
                }
            }
        },
        Intent::WatchAgents { thread_id } => {
            let thread_id = thread_id.filter(|id| pane.navigation.thread_id.as_ref() == Some(id));
            if pane.observed_agents != thread_id {
                let target = Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists"));
                let retired = std::mem::replace(&mut target.observed_agents, thread_id.clone());
                if let Some(session) = retired
                    && !next.panes.0.values().any(|pane| pane.observed_agents.as_ref() == Some(&session)) {
                    Arc::make_mut(&mut next.operations).remove(&op::OperationKey::Agents { session });
                }
                let effects = op::refresh_agents(next.connection.connected, thread_id.as_ref());
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
            if let Some(cursor) = previous.sessions.conversations.get(&thread_id).and_then(|thread| thread.history_cursor.clone()) {
                return prepare(next, pane_id, op::ReadHistory { session: thread_id, cursor, include_activity: false });
            }
            let window = op::HISTORY_WINDOW_TURNS;
            let limit = u32::try_from(previous.sessions.conversations.get(&thread_id).map_or(window.into(), |thread| {
                thread.history_limit
                    .unwrap_or_else(|| thread.turns.as_ref().map_or(window.into(), |turns| turns.len() as u64))
            })).unwrap_or(u32::MAX).saturating_add(window);
            return prepare(next, pane_id, op::ReadThread { limit, ..op::ReadThread::new(thread_id) });
        }
        Intent::LoadTurnItems(params) => {
            if previous.sessions.conversations.get(&params.thread_id)
                .and_then(|thread| thread.turns.as_ref())
                .and_then(|turns| turns.iter().rfind(|turn| turn.id == params.turn_id))
                .is_some_and(|turn| turn.items_summary) {
                return prepare(next, pane_id, params);
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
            alternate,
        } => {
            let thread_id = thread_id.or_else(|| pane.navigation.thread_id.clone());
            if let Some(effects) = thread_id
                .as_ref()
                .and_then(|id| follow_up::save_edit(&mut next, id))
            {
                return (next, effects);
            }
            let draft_key = thread_id
                .clone()
                .map(DraftKey::from)
                .unwrap_or_else(|| pane.navigation.draft_key.clone());
            let draft = previous.input.drafts.get(&draft_key).cloned().unwrap_or_default();
            return submission(
                previous,
                Destination {
                    thread_id,
                    cwd: (!pane.navigation.cwd.trim().is_empty())
                        .then(|| pane.navigation.cwd.clone()),
                    draft_key,
                },
                draft,
                client_user_message_id,
                None,
                follow_up::requested(previous.input.composer_preferences.follow_up, alternate),
            );
        }
        // Store commits this device-owned value after admitting the action.
        Intent::SetComposerPreferences { .. } => (),
        Intent::Queue { thread_id, action } => {
            let effects = follow_up::reduce(&mut next, thread_id, action);
            return (next, effects);
        }
        Intent::RestoreUnknownSubmission { client_user_message_id } => {
            if previous
                .sessions.pending_submissions
                .get(&client_user_message_id)
                .is_some_and(|pending| pending.delivery_unknown)
            {
                return reduce(previous, pane_id, Event::SubmissionFailed(client_user_message_id));
            }
        }
        Intent::DiscardUnknownSubmission { client_user_message_id } => {
            if previous
                .sessions.pending_submissions
                .get(&client_user_message_id)
                .is_some_and(|pending| pending.delivery_unknown)
            {
                Arc::make_mut(&mut next.sessions.pending_submissions).remove(&client_user_message_id);
            }
        }
        Intent::RemoveAttachment { draft_key, index } => {
            let index = index as usize;
            if previous.input.drafts.get(&draft_key).is_some_and(|draft| index < draft.attachments.len())
                && let Some(draft) = shared_mut(&mut next.input.drafts, &draft_key)
            {
                draft.attachments.remove(index);
            }
        }

        Intent::ListSessions(mut operation) => {
            let refresh = operation.query.search_term == previous.lists.list_query.search_term;
            if let Err(error) = op::Operation::prepare(&mut operation, &mut next, pane_id) {
                Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error = Some(error);
                return (next, Vec::new());
            }
            let mut effects = vec![Effect::execute(operation)];
            if refresh && next.connection.connected { effects.push(Effect::execute(op::ReadTaskActivity {})); }
            return (next, effects);
        }
        Intent::SetWorkspaceComparison { cwd, base_ref } => {
            if cwd.is_empty() || next.selected_directory(next.panes.get(pane_id).expect("admitted pane exists")) != cwd {
                return (next, Vec::new());
            }
            let pane = Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists"));
            Arc::make_mut(&mut pane.workspace).comparison = base_ref;
            let effects = op::review_workspace(&mut next, pane_id).into_iter().collect();
            return (next, effects);
        }
        Intent::RefreshProject { project_id } => {
            let Some(limit) = previous.lists.expanded_projects.get(&project_id).copied() else {
                return (next, Vec::new());
            };
            return prepare(next, pane_id, op::ListProjectSessions {
                project_id,
                limit,
                search_term: previous.lists.list_query.search_term.clone(),
            });
        }
        Intent::SetProjectPinned { project_id, pinned } => projects::update(&mut next, &project_id, |p| p.pinned = pinned),
        Intent::ReorderProject { project_id, target_id, after } => projects::reorder(&mut next, &project_id, &target_id, after),
        Intent::MoveChatToProject { thread_id, project_id } => {
            projects::move_chat(&mut next, &thread_id, &project_id);
            if next.connection.connected && next.lists.expanded_projects.contains_key(&project_id)
                && !next.lists.project_threads.contains_key(&project_id)
                && projects::chat_placement(next.lists.project_preferences.get(&next.connection.storage_scope), &thread_id) == Some(project_id.as_str()) {
                return prepare(next, pane_id, op::ListProjectSessions {
                    project_id, limit: 5, search_term: previous.lists.list_query.search_term.clone(),
                });
            }
        },
        Intent::EditProject { project_id, name, section } => {
            match projects::validate_project_labels(&name, &section) {
                Ok((name, section)) => projects::update(&mut next, &project_id, |p| {
                    p.name = Some(name.into());
                    p.section = (!section.is_empty()).then(|| section.into());
                }),
                Err(reason) => {
                    Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error =
                        Some(reason.into())
                }
            }
        }
        Intent::SetProjectSection { project_id, section } => {
            if let Some(section) = projects::project_section(&section) {
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
                Arc::make_mut(&mut next.lists.expanded_projects).remove(&project_id);
            } else if !previous.lists.expanded_projects.contains_key(&project_id) {
                if previous.lists.threads.as_ref().is_none_or(|page| {
                    !page.projects.iter().any(|project| project.id == project_id)
                }) {
                    return (next, Vec::new());
                }
                let page = previous.lists.project_threads.get(&project_id);
                let limit = page.map_or(5, |page| page.limit);
                Arc::make_mut(&mut next.lists.expanded_projects).insert(project_id.clone(), limit);
                if next.connection.connected && (page.is_none() || next.lists.unconfirmed_lists.contains(&op::OperationKey::ProjectList { project_id: project_id.clone() })) {
                    return prepare(next, pane_id, op::ListProjectSessions {
                        project_id,
                        limit,
                        search_term: previous.lists.list_query.search_term.clone(),
                    });
                }
            }
        }
        Intent::ExpandThreadList { project_id } => {
            if let Some(project_id) = project_id {
                let Some(limit) = previous.lists.expanded_projects.get(&project_id) else {
                    return (next, Vec::new());
                };
                let limit = limit.saturating_add(10);
                Arc::make_mut(&mut next.lists.expanded_projects).insert(project_id.clone(), limit);
                return prepare(next, pane_id, op::ListProjectSessions {
                    project_id,
                    limit,
                    search_term: previous.lists.list_query.search_term.clone(),
                });
            }
            let mut query = (*previous.lists.list_query).clone();
            query.limit = query.limit.saturating_add(10);
            query.part = Some(crate::models::ListPart::Chats);
            return prepare(next, pane_id, op::ListSessions::new(query));
        }
        Intent::ShowRecentChats | Intent::ExpandRecentChats => {
            let expanding = matches!(intent, Intent::ExpandRecentChats);
            if !expanding && previous.lists.recent_chats {
                return (next, Vec::new());
            }
            next.lists.recent_chats = true;
            let mut query = (*previous.lists.list_query).clone();
            if expanding {
                query.limit = query.limit.saturating_add(10);
            }
            query.project_limit = query.project_limit.max(query.limit);
            query.part = None;
            if next.connection.connected {
                return prepare(next, pane_id, op::ListSessions::new(query));
            }
        }
        Intent::ExpandProjects => {
            let mut query = (*previous.lists.list_query).clone();
            query.project_limit = query.project_limit.saturating_add(10);
            query.part = Some(crate::models::ListPart::Projects);
            return prepare(next, pane_id, op::ListSessions::new(query));
        }

        Intent::ShowThreadList => {
            next.invalidate_pane(pane_id);
            navigate(&mut next, pane_id, Navigation::default());
            return (next, Vec::new());
        }
        Intent::NewChat { cwd } => {
            next.invalidate_pane(pane_id);
            let key = DraftKey::new_chat(&cwd, pane.draft_slot);
            let existing = previous.input.drafts.get(&key);
            if existing.is_none_or(|draft| draft.model.is_none()) {
                let selection = previous.catalog.model_preferences.provider
                    .and_then(|provider| previous.catalog.model_preferences.selections.get(&provider));
                let mut draft = existing.map(|draft| (**draft).clone()).unwrap_or_default();
                let provider = selection.map(|selection| selection.model.provider)
                    .or_else(|| previous.model_provider_for_draft(key.clone()));
                let (model, effort, tier) = match provider {
                    Some(provider) => supported_settings(
                        selection.map(|selection| &selection.model),
                        draft.effort.as_deref().or_else(|| selection.and_then(|selection| selection.effort.as_deref())),
                        draft.service_tier.as_deref().or_else(|| selection.and_then(|selection| selection.service_tier.as_deref())),
                        Some(provider), &previous.catalog.models, &previous.catalog.model_errors, previous.allows_custom_codex_models(),
                    ),
                    None => (None, draft.effort.as_deref(), draft.service_tier.as_deref()),
                };
                let settings = (model.cloned(), effort.map(str::to_owned), tier.map(str::to_owned));
                (draft.model, draft.effort, draft.service_tier) = settings;
                Arc::make_mut(&mut next.input.drafts).insert(key.clone(), Arc::new(draft));
            }
            navigate(&mut next, pane_id, Navigation {
                cwd,
                draft_key: key,
                ..Default::default()
            });
            let selected = next.panes.get(pane_id).unwrap().navigation.cwd.clone();
            Arc::make_mut(&mut next.files.workspace).options.remove(&selected);
            let effects = op::review_workspace(&mut next, pane_id).into_iter().collect();
            return (next, effects);
        }

        Intent::SetFileDraft { path, text } => {
            if pane.workspace.file.as_ref().is_some_and(|file| file.path == path && file.truncated) {
                Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error = Some("ファイルが大きすぎるため編集できません。".into());
                return (next, Vec::new());
            }
            let revision = previous
                .files.file_drafts
                .get(&path)
                .map(|draft| draft.revision.as_str())
                .or_else(|| {
                    pane.workspace
                        .file
                        .as_ref()
                        .filter(|file| file.path == path)
                        .map(|file| file.revision.as_str())
                });
            if let Some(revision) = revision {
                Arc::make_mut(&mut next.files.file_drafts).insert(
                    path,
                    Arc::new(FileDraft {
                        revision: revision.into(),
                        text,
                    }),
                );
            } else {
                Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error = Some("file has not been loaded".into());
            }
        }

        Intent::SetDraft { thread_id, draft } => {
            Arc::make_mut(&mut next.input.drafts).insert(thread_id, Arc::new(draft));
        }
        Intent::Stash {storage_scope,thread_id,action} => {
            if storage_scope == previous.connection.storage_scope {
                let empty = Draft::default();
                let draft = previous.input.drafts.get(&thread_id).map(AsRef::as_ref).unwrap_or(&empty);
                let busy = previous.operation_running(op::OperationKey::Attachment {draft_key:thread_id.clone()})
                    || previous.operation_running(op::OperationKey::Dictation {draft_key:thread_id.clone()})
                    || matches!(&thread_id, DraftKey::Session {session} if previous.input.queued_edits.contains_key(session));
                match crate::composer::stash::change(&previous.input.stashes,draft,busy,action) {
                    Ok(change) => next.input.apply_stash_change(thread_id, change),
                    Err(reason) => Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error = Some(reason.into()),
                }
            }
        }
        Intent::ChangeContext { storage_scope, thread_id, action } => {
            if storage_scope == previous.connection.storage_scope {
                let current = previous.input.drafts.get(&thread_id).map_or(&[][..], |draft| &draft.contexts);
                match crate::composer::context::change(current, action) {
                    Ok(contexts) if contexts != current => Arc::make_mut(Arc::make_mut(&mut next.input.drafts).entry(thread_id).or_default()).contexts = contexts,
                    Ok(_) => {}
                    Err(reason) => Arc::make_mut(next.panes.get_mut(pane_id).expect("admitted pane exists")).error = Some(reason.into()),
                }
            }
        }
        Intent::SelectWorkspace { thread_id, workspace } => {
            Arc::make_mut(Arc::make_mut(&mut next.input.drafts).entry(thread_id).or_default()).workspace = workspace;
        }
        Intent::AppendTranscript { thread_id, text } => {
            if !text.is_empty() {
                let mut combined = previous.input.drafts.get(&thread_id)
                    .map(|draft| draft.text.clone()).unwrap_or_default();
                append_transcript(&mut combined, &text);
                let cursor = u32::try_from(combined.len()).unwrap_or(u32::MAX);
                return reduce_intent(previous, pane_id, Intent::EditComposer { thread_id, text: combined, cursor });
            }
        },
        Intent::EditComposer { thread_id, text, cursor } => {
            let load_catalog = thread_id == pane.navigation.draft_key
                && previous.connection.connected
                && crate::composer::query(&text, cursor as usize).is_some_and(|(_, _, filter)| {
                    crate::composer::should_refresh_catalog(
                        previous.catalog.composer_catalogs.get(&pane.navigation.cwd)
                            .filter(|catalog| catalog.cwd == pane.navigation.cwd)
                            .map(|catalog| catalog.loading),
                        filter,
                        previous.input.drafts.get(&thread_id).is_none_or(|draft| draft.text != text),
                    )
                });
            set_draft_text(&mut next, thread_id, text);
            if load_catalog {
                let cwd = pane.navigation.cwd.clone();
                return prepare(next, pane_id, op::LoadComposerCatalog { cwd });
            }
        }
        Intent::InsertInvocation { thread_id, text, invocation } => {
            let draft = Arc::make_mut(Arc::make_mut(&mut next.input.drafts).entry(thread_id).or_default());
            let token = invocation.token();
            draft.invocations.retain(|item| item.token() != token && item.is_in(&text));
            draft.invocations.push(invocation);
            draft.text = text;
        }
        Intent::SetDraftText { thread_id, text } => set_draft_text(&mut next, thread_id, text),
        Intent::QuoteSelection { storage_scope, thread_id, text } => {
            if previous.connection.storage_scope == storage_scope {
                let draft = previous.input.drafts.get(&thread_id)
                    .map(|draft| draft.text.as_str()).unwrap_or_default();
                set_draft_text(&mut next, thread_id,
                    crate::composer::append_selection_to_draft(draft, &text));
            }
        },
        Intent::AnswerQuestion { request_id, action } => {
            if let Some(answer) = crate::questions::act(&mut next, request_id.clone(), action) {
                return prepare(next, pane_id, op::Respond { request_id, answer });
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
                .input.drafts
                .get(thread_id)
                .map(|draft| draft.as_ref().clone())
                .unwrap_or_default();
            let thread_id = match intent {
                Intent::SelectModel { thread_id, model } => {
                    if draft.model.as_ref() != Some(&model) {
                        if draft.model.as_ref().is_none_or(|selected| selected.provider != model.provider)
                        {
                            let settings = previous.catalog.model_preferences.selections.get(&model.provider);
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
            if !previous.catalog.models.is_empty() {
                let (model, effort, tier) = supported_settings(draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref(), None, &previous.catalog.models, &previous.catalog.model_errors, previous.allows_custom_codex_models());
                let settings = (
                    model.cloned(),
                    effort.map(str::to_owned),
                    tier.map(str::to_owned),
                );
                (draft.model, draft.effort, draft.service_tier) = settings;
            }
            draft.model_settings_changed |= previous.input.drafts.get(&thread_id).is_none_or(|saved| {
                (saved.model.as_ref(), saved.effort.as_deref(), saved.service_tier.as_deref())
                    != (draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref())
            });
            if previous
                .input.drafts
                .get(&thread_id)
                .is_none_or(|previous| **previous != draft)
            {
                Arc::make_mut(&mut next.input.drafts).insert(thread_id, Arc::new(draft));
            }
        }
    });
    (next, Vec::new())
}
pub(crate) fn set_draft_text(snapshot: &mut Snapshot, thread_id: DraftKey, text: String) {
    if snapshot
        .input
        .drafts
        .get(&thread_id)
        .is_some_and(|draft| draft.text == text)
    {
        return;
    }
    let draft = Arc::make_mut(
        Arc::make_mut(&mut snapshot.input.drafts)
            .entry(thread_id)
            .or_default(),
    );
    draft.invocations.retain(|item| item.is_in(&text));
    draft.text = text;
}

fn navigate(snapshot: &mut Snapshot, pane_id: &str, navigation: Navigation) {
    let Some(target) = snapshot.panes.get_mut(pane_id) else {
        return;
    };
    let pane = Arc::make_mut(target);
    let previous_id = pane.navigation.thread_id.clone();
    if pane.navigation.draft_key != navigation.draft_key {
        Arc::make_mut(&mut snapshot.operations).retain(|key, _| {
            key.pane() != Some(pane_id) && !matches!(key, op::OperationKey::SaveFile { .. })
        });
        let saved = pane.workspace.location(pane.navigation.cwd.clone());
        let workspaces = Arc::make_mut(&mut pane.workspaces);
        workspaces.insert(pane.navigation.draft_key.clone(), saved);
        let location = workspaces
            .remove(&navigation.draft_key)
            .filter(|location| location.cwd == navigation.cwd)
            .unwrap_or_default();
        let workspace = Arc::make_mut(&mut pane.workspace);
        workspace.file = location.file;
        workspace.review_cwd = location.review_cwd;
        workspace.comparison = location.comparison;
        workspace.review = location.review;
    } else if pane.navigation.cwd != navigation.cwd || navigation.draft_key.is_empty() {
        clear_workspace_location(Arc::make_mut(&mut pane.workspace));
    }
    if previous_id != navigation.thread_id {
        pane.observed_agents = None;
    }
    let selected = navigation.thread_id.clone();
    pane.navigation = Arc::new(navigation);
    let _ = previous_id
        .filter(|id| selected.as_ref() != Some(id) && !snapshot.selected_in_any_pane(id))
        .filter(|id| {
            snapshot.lists.activity.active.get(id) != Some(&true)
                && snapshot
                    .sessions
                    .conversations
                    .get(id)
                    .is_none_or(|thread| thread.requests.is_empty())
        })
        .and_then(|id| Arc::make_mut(&mut snapshot.sessions.subscriptions).remove(&id));
    if let Some(id) = selected
        && snapshot.lists.activity.unread.contains(&id)
    {
        Arc::make_mut(&mut snapshot.lists.activity)
            .unread
            .remove(&id);
    }
}

fn prepare<O: operations::Operation>(
    mut next: Snapshot,
    pane_id: &str,
    mut operation: O,
) -> (Snapshot, Vec<Effect>) {
    if operation.invalidates() {
        next.invalidate_pane(pane_id);
    }
    if let Err(error) = operation.prepare(&mut next, pane_id) {
        if let Some(key) = operation.key() {
            next.fail_operation(key.in_pane(pane_id), error);
        } else {
            if let Some(pane) = next.panes.get_mut(pane_id) {
                Arc::make_mut(pane).error = Some(error);
            }
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
pub(crate) fn reduce_event(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    match event {
        Event::TerminalFailed { handle, reason } => {
            if previous.connection.connected
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
            next.connection.host_ticket = Some(ticket);
        }
        Event::HostHomeDirectory(home) => {
            next.connection.host_home_directory = home;
        }
        Event::StorageScope(scope) => {
            if next.connection.storage_scope == scope {
                return (next, Vec::new());
            }
            next.project_import = Arc::default();
            next.terminals = Arc::default();
            Arc::make_mut(next.panes.primary_mut()).observed_agents = None;
            next.catalog.provider_settings = None;
            next.catalog.host_software = None;
            next.lists.task_activity = None;
            next.lists.activity = Arc::default();
            next.lists.expanded_projects = Arc::default();
            next.lists.project_threads = Arc::default();
            next.lists.unconfirmed_lists = Arc::default();
            if !next.connection.storage_scope.is_empty() {
                let archived = ScopedData {
                    drafts: std::mem::take(&mut next.input.drafts),
                    stashes: std::mem::take(&mut next.input.stashes),
                    pending_submissions: std::mem::take(&mut next.sessions.pending_submissions),
                    file_drafts: std::mem::take(&mut next.files.file_drafts),
                    navigation: std::mem::take(
                        &mut Arc::make_mut(next.panes.primary_mut()).navigation,
                    ),
                };
                Arc::make_mut(&mut next.lists.archived_scopes)
                    .insert(next.connection.storage_scope.clone(), Arc::new(archived));
                if let Some(saved) = Arc::make_mut(&mut next.lists.archived_scopes).remove(&scope) {
                    next.input.drafts = saved.drafts.clone();
                    next.input.stashes = saved.stashes.clone();
                    next.sessions.pending_submissions = saved.pending_submissions.clone();
                    next.files.file_drafts = saved.file_drafts.clone();
                    Arc::make_mut(next.panes.primary_mut()).navigation = saved.navigation.clone();
                }
                for (_, pane) in Arc::make_mut(&mut next.panes.0)
                    .iter_mut()
                    .filter(|(id, _)| !id.is_empty())
                {
                    let cwd = pane.navigation.cwd.clone();
                    *pane = Arc::new(PaneState {
                        draft_slot: pane.draft_slot,
                        epoch: pane.epoch.wrapping_add(1),
                        navigation: Arc::new(Navigation {
                            draft_key: DraftKey::new_chat(&cwd, pane.draft_slot),
                            cwd,
                            ..Default::default()
                        }),
                        ..Default::default()
                    });
                }
                next.sessions.conversations = Arc::default();
                next.lists.threads = None;
                next.files.workspace = Arc::default();
                Arc::make_mut(next.panes.primary_mut()).workspace = Arc::default();
                Arc::make_mut(next.panes.primary_mut()).workspaces = Arc::default();
                next.sessions.draft_sessions = Arc::default();
                next.catalog.account = Arc::default();
                next.catalog.models = Arc::default();
                next.catalog.model_errors = Arc::default();
                next.reset_session();
            }
            next.connection.storage_scope = scope;
        }
        Event::SubmissionUnknown(id) => {
            if let Some(pending) = shared_mut(&mut next.sessions.pending_submissions, &id)
                && !pending.accepted
            {
                pending.delivery_unknown = true;
            }
        }
        Event::SubmissionFailed(id) => {
            if let Some(pending) = Arc::make_mut(&mut next.sessions.pending_submissions).remove(&id)
            {
                let draft = Arc::make_mut(
                    Arc::make_mut(&mut next.input.drafts)
                        .entry(pending.draft_key.clone())
                        .or_default(),
                );
                *draft = restore_sent_draft(draft, &pending.draft);
            }
        }
        Event::ConnectionChecked { failure } => next.connection.connection_failure = failure,
        Event::Connected => {
            next.connection.connection_generation =
                previous.connection.connection_generation.saturating_add(1);
            next.reset_list_revisions();
            next.operations = Default::default();
            next.reset_session();
            next.connection.connected = true;
            next.lists.task_activity = None;
            next.connection.connection_failure = None;
            next.lists.list_query = Arc::new((*next.lists.list_query).clone().for_connection());
            let effects = next.prepare_connection_refresh();
            return (next, effects);
        }
        Event::Disconnected => {
            next.operations = Default::default();
            next.reset_session();
            next.connection.connected = false;
            for terminal in Arc::make_mut(&mut next.terminals).values_mut() {
                if matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Running
                ) {
                    Arc::make_mut(terminal).phase = TerminalPhase::Suspended;
                }
            }
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
            .lists
            .threads
            .iter_mut()
            .chain(Arc::make_mut(&mut self.lists.project_threads).values_mut())
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
        for thread in Arc::make_mut(&mut self.sessions.conversations).values_mut() {
            if thread.list_revision != 0 {
                Arc::make_mut(thread).list_revision = 0;
            }
        }
    }

    fn prepare_connection_refresh(&mut self) -> Vec<Effect> {
        let mut effects = vec![Effect::execute(op::ReadTaskActivity {})];
        if self.catalog.provider_settings.take().is_some() {
            effects.push(Effect::execute(op::ReadProviderSettings {}));
        }
        if self.catalog.host_software.take().is_some() {
            effects.push(Effect::execute(op::ReadHostSoftware {}));
        }
        if self.files.file_open_capabilities_requested {
            effects.push(Effect::execute(op::ReadFileOpenCapabilities {}));
        }
        effects.extend(op::reconnect_file_trees(self));
        if self.lists.threads.is_none()
            || self
                .lists
                .unconfirmed_lists
                .contains(&op::OperationKey::SessionList)
        {
            effects.push(Effect::execute(op::ListSessions::new(
                (*self.lists.list_query).clone(),
            )));
        }
        // The Host catalog can change while a mobile client is backgrounded.
        // Keep cached choices visible until the fresh catalog arrives.
        effects.extend(op::refresh_models());
        if self.catalog.account.accounts.is_none() {
            effects.push(Effect::execute(op::ListAccounts {}));
        }
        effects.extend(op::poll_account_login(self));
        let panes: Vec<_> = self.panes.0.keys().cloned().collect();
        for id in panes {
            effects.extend(
                self.prepare_pane_refresh(&id)
                    .into_iter()
                    .map(|effect| effect.in_pane(&id)),
            );
        }
        for (handle, terminal) in self.terminals.iter() {
            if terminal.phase == TerminalPhase::Suspended {
                effects.push(match &terminal.target {
                    TerminalTarget::Shell { cwd, command } => Effect::execute(op::StartTerminal {
                        command: command.clone(),
                        handle: handle.clone(),
                        cwd: cwd.clone(),
                        size: terminal.size,
                    }),
                    TerminalTarget::Account { provider, id } => {
                        Effect::execute(op::StartAccountTerminal {
                            handle: handle.clone(),
                            provider: *provider,
                            id: id.clone(),
                            size: terminal.size,
                        })
                    }
                });
            }
        }
        effects
    }

    fn prepare_pane_refresh(&mut self, pane_id: &str) -> Vec<Effect> {
        let Some(pane) = self.panes.get(pane_id) else {
            return Vec::new();
        };
        let mut effects =
            op::refresh_agents(self.connection.connected, pane.observed_agents.as_ref());
        if let Some(thread_id) = &pane.navigation.thread_id {
            effects.push(Effect::execute(op::ReadThread::open(thread_id.clone())));
        } else {
            effects.extend(op::review_workspace(self, pane_id));
        }
        effects
    }

    /// Cached history and drafts survive; IDs and activity belong to one connection.
    /// Pending submissions are persisted only to recover dictation after a crash.
    fn reset_session(&mut self) {
        for pane in Arc::make_mut(&mut self.panes.0).values_mut() {
            if pane.error.is_some() {
                Arc::make_mut(pane).error = None;
            }
        }

        Arc::make_mut(&mut self.files.workspace)
            .file_metadata
            .disconnected();
        Arc::make_mut(&mut self.files.workspace)
            .file_trees
            .disconnected();
        self.files.file_open_capabilities = None;
        self.catalog.composer_catalogs = Arc::default();
        self.catalog.permission_settings = Arc::default();
        self.sessions.subscriptions = Arc::default();
        for thread in Arc::make_mut(&mut self.sessions.conversations).values_mut() {
            if !thread.requests.is_empty() || !thread.submissions.is_empty() {
                let thread = Arc::make_mut(thread);
                thread.requests.clear();
                thread.submissions.clear();
            }
        }
        if !self.lists.activity.active.is_empty() {
            Arc::make_mut(&mut self.lists.activity).active.clear();
        }
        self.connection.epoch += 1;
        for pending in Arc::make_mut(&mut self.sessions.pending_submissions).values_mut() {
            Arc::make_mut(pending).delivery_unknown = true;
        }
        if self
            .sessions
            .conversations
            .values()
            .any(|thread| has_session_status(thread))
        {
            for thread in Arc::make_mut(&mut self.sessions.conversations).values_mut() {
                if has_session_status(thread) {
                    clear_session_status(Arc::make_mut(thread));
                }
            }
        }
        if self.listed_threads().any(has_session_status) {
            for list in self
                .lists
                .threads
                .iter_mut()
                .chain(Arc::make_mut(&mut self.lists.project_threads).values_mut())
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
            .catalog
            .model_preferences
            .provider
            .and_then(|provider| self.catalog.model_preferences.selections.get(&provider));
        let allow_custom_codex = self.allows_custom_codex_models();
        let mut next = self.input.drafts.clone();
        for (key, previous) in self.input.drafts.iter() {
            // Let the first native read seed a conversation before catalog defaults.
            if previous.model.is_none()
                && let DraftKey::Session { session } = key
                && !self.sessions.conversations.contains_key(session)
            {
                continue;
            }
            let selected_provider = self.draft_provider(key, previous.model.as_ref());
            if selected_provider.is_none()
                && !self
                    .panes
                    .0
                    .values()
                    .any(|pane| &pane.navigation.draft_key == key)
            {
                continue;
            }
            let selection = selected_provider.is_none().then_some(remembered).flatten();
            let provider = selected_provider
                .or_else(|| selection.map(|selection| selection.model.provider))
                .or_else(|| {
                    crate::presentation::model_settings::automatic_provider(
                        &self.catalog.models,
                        self.catalog
                            .account
                            .accounts
                            .as_ref()
                            .map(|accounts| &accounts.selected),
                        &self.catalog.model_errors,
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
                &self.catalog.models,
                &self.catalog.model_errors,
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
        self.input.drafts = next;
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

// Publish sent content immediately as pending input. Later receipts and echoes
// must not clear newer draft edits. Restore definite failures alongside newer
// input; keep unknown delivery visible and ordered without automatic resubmission.
/// Where a submission goes: its conversation, or a new one in `cwd`, and the
/// draft it is sent from.
struct Destination {
    thread_id: Option<crate::session::SessionRef>,
    cwd: Option<String>,
    draft_key: DraftKey,
}

fn submission(
    previous: &Snapshot,
    Destination {
        thread_id,
        cwd,
        draft_key,
    }: Destination,
    draft: Arc<Draft>,
    client_user_message_id: agent_protocol::ids::ClientInputId,
    clear_draft: Option<Arc<Draft>>,
    requested: agent_protocol::operations::FollowUp,
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
    if let Err(reason) = agent_protocol::context::validate_contexts(&draft.contexts) {
        next.fail_operation(
            op::OperationKey::Submission {
                draft_key: draft_key.clone(),
            },
            reason.into(),
        );
        return (next, Vec::new());
    }
    if let Some(reason) = thread_id
        .as_ref()
        .and_then(|id| previous.submission_unavailable_reason(id, provider))
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
    if let Some(current) = shared_mut(&mut next.input.drafts, &draft_key) {
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
        current
            .contexts
            .retain(|context| !cleared.contexts.contains(context));
    }
    let anchor = thread_id
        .as_ref()
        .and_then(|id| previous.sessions.conversations.get(id))
        .and_then(|thread| thread.turns.as_ref())
        .and_then(|turns| turns.last());
    let sequence = previous
        .sessions
        .pending_submissions
        .values()
        .map(|pending| pending.sequence)
        .max()
        .map_or(0, |last| last + 1);
    Arc::make_mut(&mut next.sessions.pending_submissions).insert(
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
            queued: thread_id
                .as_ref()
                .and_then(|id| previous.sessions.conversations.get(id))
                .is_some_and(|thread| follow_up::waits(thread, requested)),
        }),
    );
    let effect = match thread_id {
        Some(thread_id) => Effect::execute(op::SendSubmission {
            thread_id,
            client_user_message_id,
            draft,
            follow_up: requested,
        }),
        None => Effect::execute(op::StartSubmission {
            provider: provider.expect("new submission has a provider"),
            draft_key,
            cwd,
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
            panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
                crate::state::PaneState {
                    navigation: Arc::new(navigation(a.clone())),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).workspace).review_cwd =
            Some("a-review".into());
        navigate(&mut state, "", navigation(b.clone()));
        assert!(state.panes.primary().workspace.review_cwd.is_none());
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).workspace).review_cwd =
            Some("b-review".into());
        navigate(&mut state, "", navigation(a));
        assert_eq!(
            state.panes.primary().workspace.review_cwd.as_deref(),
            Some("a-review")
        );
        navigate(&mut state, "", navigation(b));
        assert_eq!(
            state.panes.primary().workspace.review_cwd.as_deref(),
            Some("b-review")
        );
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&state).unwrap()).unwrap();
        assert!(restored.panes.primary().workspaces.is_empty());
        assert_eq!(restored.files.workspace, Arc::default());
    }
}

#[cfg(test)]
mod submission_tests {
    use super::*;

    #[rstest::rstest]
    fn switching_a_busy_conversation_explains_the_block_and_retains_the_draft(
        #[values(
            crate::session::ProviderKind::Codex,
            crate::session::ProviderKind::Claude
        )]
        active: crate::session::ProviderKind,
        #[values(false, true)] busy: bool,
        #[values(false, true)] switching: bool,
    ) {
        let session = crate::session::SessionRef::new(
            crate::session::ProviderKind::Codex,
            "conversation".into(),
        )
        .unwrap();
        let key = DraftKey::from(session.clone());
        let selected = if switching {
            crate::session::ProviderKind::DeepSeek
        } else {
            active
        };
        let draft = Arc::new(Draft {
            text: "Keep this message".into(),
            model: Some(crate::models::ModelRef {
                provider: selected,
                id: "model".into(),
            }),
            ..Default::default()
        });
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(Thread {
                id: Some(session.clone()),
                active_provider: Some(active),
                status: if busy {
                    crate::models::SessionStatus::Running
                } else {
                    crate::models::SessionStatus::Idle
                },
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), draft.clone());
        let reason = snapshot.input_unavailable_reason(session.clone(), Some(selected));
        let (next, effects) = submission(
            &snapshot,
            Destination {
                thread_id: Some(session),
                cwd: None,
                draft_key: key.clone(),
            },
            draft.clone(),
            "send".into(),
            None,
            agent_protocol::operations::FollowUp::Queue,
        );
        if busy && switching {
            assert!(reason.unwrap().contains("新しいチャット"));
            assert_eq!(next.input.drafts[&key], draft);
            assert!(effects.is_empty());
            assert!(next.sessions.pending_submissions.is_empty());
            assert!(
                next.operation_error(op::OperationKey::Submission { draft_key: key })
                    .is_some()
            );
        } else {
            assert!(reason.is_none());
            assert_eq!(effects.len(), 1);
            assert!(next.input.drafts[&key].text.is_empty());
        }
    }

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
            sessions: crate::state::Sessions {
                conversations: Arc::new(if loaded {
                    [(id.clone(), Arc::new(thread.clone()))].into()
                } else {
                    Default::default()
                }),
                ..Default::default()
            },
            lists: crate::state::Lists {
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
                ..Default::default()
            },
            input: crate::state::Input {
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
            },
            ..Default::default()
        };
        let (next, effects) = reduce_intent(
            &snapshot,
            "",
            Intent::Submit {
                thread_id: Some(id),
                client_user_message_id: "input".into(),
                alternate: false,
            },
        );
        if can_accept_direct_input == Some(false) {
            assert_eq!(next.input.drafts[&key].text, "Keep my draft");
            assert!(next.sessions.pending_submissions.is_empty());
            assert!(effects.is_empty());
            assert!(
                next.operation_error(op::OperationKey::Submission { draft_key: key })
                    .unwrap()
                    .message
                    .contains("閲覧専用")
            );
        } else {
            assert!(next.input.drafts[&key].text.is_empty());
            assert_eq!(next.sessions.pending_submissions.len(), 1);
            assert_eq!(effects.len(), 1);
        }
    }
}

fn restore_sent_draft(current: &Draft, sent: &Draft) -> Draft {
    let mut restored = current.clone();
    restored.text = sent.text.clone();
    append_transcript(&mut restored.text, &current.text);
    for invocation in &sent.invocations {
        if !restored.invocations.contains(invocation) {
            restored.invocations.push(invocation.clone());
        }
    }
    for context in &sent.contexts {
        if !restored
            .contexts
            .iter()
            .any(|current| current.id == context.id)
        {
            restored.contexts.push(context.clone());
        }
    }
    for attachment in &sent.attachments {
        if !restored
            .attachments
            .iter()
            .any(|current| current.path == attachment.path)
        {
            restored.attachments.push(attachment.clone());
        }
    }
    restored
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
    follow_up::reconcile_edit(snapshot, thread_id);
    if snapshot.sessions.pending_submissions.is_empty() {
        return;
    }
    let Some(thread) = snapshot.sessions.conversations.get(thread_id) else {
        return;
    };
    let draft_key = DraftKey::from(thread_id);
    let deliveries: Vec<_> = snapshot
        .sessions
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
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
        // Someone removed it from the queue: it is gone, not returned to the composer.
        if delivery == SubmissionDelivery::Withdrawn {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).remove(&id);
            continue;
        }
        let mut pending = (*snapshot.sessions.pending_submissions[&id]).clone();
        match delivery {
            SubmissionDelivery::Sending => pending.delivery_unknown = false,
            SubmissionDelivery::Accepted { turn_id } => {
                if let Some(draft) = shared_mut(&mut snapshot.input.drafts, &pending.draft_key)
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
            SubmissionDelivery::Rejected | SubmissionDelivery::Withdrawn => unreachable!(),
        }
        if pending != *snapshot.sessions.pending_submissions[&id] {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(id, Arc::new(pending));
        }
    }
    let mut pending_ids = snapshot
        .sessions
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
        .map(|(id, _)| id)
        .peekable();
    let Some(first) = pending_ids.next() else {
        return;
    };
    let thread = &snapshot.sessions.conversations[thread_id];
    let mut native_echoes = thread
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| turn.items.iter().flatten())
        .filter(|item| matches!(item.body(), crate::models::ItemBody::UserMessage { .. }))
        .filter_map(|item| item.client_input_id.as_ref());
    if pending_ids.peek().is_none() {
        if let Some(id) = native_echoes.find(|id| *id == first) {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).remove(id);
        }
        return;
    }
    let pending_ids: HashSet<_> = std::iter::once(first).chain(pending_ids).collect();
    let mut echoed = HashSet::new();
    for id in native_echoes {
        if pending_ids.contains(id) {
            echoed.insert(id);
            if echoed.len() == pending_ids.len() {
                break;
            }
        }
    }
    if !echoed.is_empty() {
        Arc::make_mut(&mut snapshot.sessions.pending_submissions)
            .retain(|id, _| !echoed.contains(id));
    }
}

#[cfg(test)]
#[path = "state/pending_tests.rs"]
mod pending_tests;

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
                    connection: crate::state::Connection {
                        connected: true,
                        ..Default::default()
                    },
                    sessions: crate::state::Sessions {
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
                        ..Default::default()
                    },
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
                    "",
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

#[cfg(test)]
#[path = "state/workspace_tests.rs"]
mod workspace_tests;
