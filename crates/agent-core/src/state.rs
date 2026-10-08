//! Immutable client state and pure conversation transitions.
use crate::models::{
    FileContent, FileList, HostStatus, Invitation, Item, ListQuery, Model, RemoteHost, Thread,
    ThreadList, WorkspaceReview, WorktreeSettings,
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
}
/// Device preferences for new chats and each provider, with independent choices.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelDefaults {
    pub new_chat_model: Option<crate::models::ModelRef>,
    pub providers: HashMap<crate::session::ProviderKind, ProviderModelDefaults>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderModelDefaults {
    pub model: Option<crate::models::ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ModelDefaultsScope {
    #[default]
    Global,
    Environment {
        id: String,
    },
    Project {
        environment: String,
        project: String,
    },
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
    pub directory: Option<Arc<FileList>>,
    pub file: Option<Arc<FileContent>>,
    pub review_cwd: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<crate::models::Worktree>>>,
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
    pub accounts: Option<Arc<agent_protocol::operations::Accounts>>,
    #[serde(skip)]
    pub login: Option<Arc<agent_protocol::operations::AccountLogin>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmission {
    pub sequence: u64,
    pub draft_key: DraftKey,
    pub draft: Arc<Draft>,
    pub turn_id: Option<agent_protocol::ids::TurnId>,
    pub after_item_id: Option<agent_protocol::ids::ItemId>,
    pub accepted: bool,
    pub delivery_unknown: bool,
}
impl PendingSubmission {
    pub fn delivery_label(&self) -> &'static str {
        if self.delivery_unknown {
            "送信結果不明（自動再送しません）"
        } else if self.accepted {
            "送信済み"
        } else {
            "送信中…"
        }
    }
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
            Self::Failed(error) => error.clone(),
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    #[serde(skip)]
    pub operations: Arc<BTreeMap<operations::OperationKey, operations::OperationState>>,
    #[serde(skip)]
    pub operation_sequence: u64,
    #[serde(default)]
    pub model_defaults: ModelDefaults,
    #[serde(default, with = "crate::persistence::entries")]
    pub scoped_model_defaults: Arc<BTreeMap<ModelDefaultsScope, ModelDefaults>>,
    #[serde(skip)]
    pub permission_settings: Option<Arc<op::PermissionSettingsState>>,
    #[serde(skip)]
    pub composer_catalog: Option<Arc<agent_protocol::composer::ComposerCatalog>>,
    #[serde(skip)]
    pub host_name: Option<String>,
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
    #[serde(default)]
    pub models: Arc<Vec<Model>>,
    #[serde(default)]
    pub model_errors: Arc<Map<String, Value>>,
    #[serde(with = "crate::persistence::entries")]
    pub drafts: Arc<BTreeMap<DraftKey, Arc<Draft>>>,
    pub pending_submissions:
        Arc<BTreeMap<agent_protocol::ids::ClientInputId, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, Arc<FileDraft>>>,
    #[serde(default)]
    pub workspace: Arc<Workspace>,
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
    pub subscriptions: Arc<BTreeMap<crate::session::SessionRef, uuid::Uuid>>,
    #[serde(skip)]
    pub error: Option<String>,
}
impl Snapshot {
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
    pub fn operation_running(&self, key: operations::OperationKey) -> bool {
        self.operations
            .get(&key)
            .is_some_and(|state| state.phase == operations::OperationPhase::Running)
    }
    pub fn operation_error(&self, key: operations::OperationKey) -> Option<String> {
        self.operations
            .get(&key)
            .and_then(|state| match &state.phase {
                operations::OperationPhase::Failed { message } => Some(message.clone()),
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
        let provider = provider.map(|provider| match provider {
            crate::session::ProviderKind::Codex => "codex",
            crate::session::ProviderKind::Claude => "claude",
        });
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
pub mod operations;
pub use operations::Intent;
use operations::add_attachment;

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
    pub activity: Arc<Activity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    StorageScope(String),
    TerminalFailed { handle: String, reason: String },
    Intent(Intent),
    SubmissionFailed(agent_protocol::ids::ClientInputId),
    SubmissionUnknown(agent_protocol::ids::ClientInputId),

    Notification(crate::protocol::Notification),
    SessionUpdate(Box<crate::session::SessionUpdate>),
    Connected,
    Disconnected(String),
    Failed(String),
}
pub use crate::store::Effect;

// Invalidate both displayed content and responses still in flight. File drafts
// remain keyed by absolute path so navigation never discards unsaved edits.
fn clear_workspace_location(workspace: &mut Workspace) {
    workspace.directory = None;
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
    effects.extend(op::prefetch_composer_catalog(&mut next));
    (next, effects)
}
macro_rules! prepare_operations {
    ($intent:expr, $previous:expr, $next:ident, [$($variant:ident),* $(,)?], {$($local:tt)*}) => {
        match $intent {
            $(Intent::$variant(operation) => return prepare($previous, $next, operation),)*
            $($local)*
        }
    };
}

fn reduce_intent(previous: &Snapshot, intent: Intent) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    prepare_operations!(intent, previous, next, [
        RegisterLiveActivity, UnregisterLiveActivity,
        ReadPermissionSettings, UpdatePermissionSettings,
        ListAccounts, SelectAccount, SelectAccountForDraft, LogoutAccount, StartAccountLogin,
        ReadAccountLogin, CancelAccountLogin, SubmitAccountLogin, ForkSession,
        StartTerminal, DetachTerminal, KillTerminal, CreateInvitation, RemoveRemoteHost,
        RevokeDevice, ListFiles, ReadFile,
        SaveFile, ReviewWorkspace, ReadWorktreeSettings,
        UpdateWorktreeSettings, ListWorktrees, RemoveWorktree, ListSessions, AddProject, CreateSession,
        ReadThread, OpenRequest, ReadItem, ResizeTerminal,
        Interrupt,
        WriteTerminal, DownloadFile, LoadSessionImages, LoadVisualization,
        LoadHostName, LoadHostManagement, LoadModels,
        Respond, Transcribe, UploadAttachment, PairRemoteHost,
    ], {
        Intent::ReadOlder { thread_id } => {
            if let Some(cursor) = previous.conversations.get(&thread_id).and_then(|thread| thread.history_cursor.clone()) {
                return prepare(previous, next, op::ReadHistory { session: thread_id, cursor, include_activity: false });
            }
            let limit = u32::try_from(previous.conversations.get(&thread_id).map_or(5, |thread| {
                thread.history_limit
                    .unwrap_or_else(|| thread.turns.as_ref().map_or(5, |turns| turns.len() as u64))
            })).unwrap_or(u32::MAX).saturating_add(5);
            return prepare(previous, next, op::ReadThread { limit, ..op::ReadThread::new(thread_id) });
        }
        Intent::LoadTurnItems(params) => {
            if previous.conversations.get(&params.thread_id)
                .and_then(|thread| thread.turns.as_ref())
                .and_then(|turns| turns.iter().rfind(|turn| turn.id == params.turn_id))
                .is_some_and(|turn| turn.items_summary) {
                return prepare(previous, next, params);
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
        Intent::AddAttachment {
            draft_key,
            attachment,
        } => {
            add_attachment(&mut next, draft_key, attachment);
        }
        Intent::RemoveAttachment { draft_key, index } => {
            let index = index as usize;
            if previous.drafts.get(&draft_key).is_some_and(|draft| index < draft.attachments.len())
                && let Some(draft) = shared_mut(&mut next.drafts, &draft_key)
            {
                draft.attachments.remove(index);
            }
        }

        Intent::ExpandThreadList { project_id, projects } => {
            let mut query = (*previous.list_query).clone();
            let limit = if let Some(id) = project_id {
                query.project_thread_limits.entry(id).or_insert(5)
            } else if projects { &mut query.project_limit } else { &mut query.chat_limit };
            *limit = limit.saturating_add(10);
            return prepare(previous, next, operations::ListSessions { query });
        }

        Intent::ShowThreadList => {
            next.epoch += 1;
            navigate(&mut next, Navigation::default());
            return (next, Vec::new());
        }
        Intent::NewChat { cwd } => {
            next.epoch += 1;
            let key = DraftKey::Local { key: format!("new:{cwd}") };
            if !previous.drafts.contains_key(&key) {
                let preferences = previous.model_defaults_for_cwd(&cwd);
                let provider = preferences.new_chat_model.as_ref().map_or(
                    crate::session::ProviderKind::Codex, |model| model.provider,
                );
                let defaults = preferences.providers.get(&provider).cloned().unwrap_or_default();
                let mut draft = Draft {
                    model: preferences.new_chat_model.or(defaults.model),
                    effort: defaults.effort,
                    service_tier: defaults.service_tier,
                    ..Default::default()
                };
                if !previous.models.is_empty() {
                    let (model, effort, tier) = supported_settings(
                        draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref(),
                        Some(provider), &previous.models, !previous.model_errors.is_empty(),
                    );
                    let settings = (model.cloned(), effort.map(str::to_owned), tier.map(str::to_owned));
                    (draft.model, draft.effort, draft.service_tier) = settings;
                }
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

        Intent::SelectNewChatModel { scope, model } => {
            let mut defaults = previous.model_defaults(scope.clone());
            defaults.new_chat_model = model;
            set_model_defaults(&mut next, scope, defaults);
        }
        Intent::SelectDefaultModel { scope, provider, model } => {
            if model.as_ref().is_none_or(|model| model.provider == provider) {
                let mut defaults = previous.model_defaults(scope.clone());
                let settings = defaults.providers.entry(provider).or_default();
                if settings.model != model {
                    *settings = ProviderModelDefaults { model, ..Default::default() };
                }
                set_model_defaults(&mut next, scope, defaults);
            }
        }
        Intent::SelectDefaultEffort { scope, provider, effort } => {
            let mut defaults = previous.model_defaults(scope.clone());
            defaults.providers.entry(provider).or_default().effort = effort;
            set_model_defaults(&mut next, scope, defaults);
        }
        Intent::SelectDefaultServiceTier { scope, provider, service_tier } => {
            let mut defaults = previous.model_defaults(scope.clone());
            defaults.providers.entry(provider).or_default().service_tier = service_tier;
            set_model_defaults(&mut next, scope, defaults);
        }
        Intent::InheritModelDefaults { scope } => {
            Arc::make_mut(&mut next.scoped_model_defaults).remove(&scope);
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
                return prepare(previous, next, op::LoadComposerCatalog { cwd });
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
                        if matches!(&thread_id, DraftKey::Local { .. })
                            && draft.model.as_ref().is_none_or(|selected| selected.provider != model.provider)
                        {
                            let defaults = previous.model_defaults_for_cwd(&previous.navigation.cwd);
                            let settings = defaults.providers.get(&model.provider);
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
                let (model, effort, tier) = supported_settings(draft.model.as_ref(), draft.effort.as_deref(), draft.service_tier.as_deref(), None, &previous.models, !previous.model_errors.is_empty());
                let settings = (
                    model.cloned(),
                    effort.map(str::to_owned),
                    tier.map(str::to_owned),
                );
                (draft.model, draft.effort, draft.service_tier) = settings;
            }
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
fn set_draft_text(snapshot: &mut Snapshot, thread_id: DraftKey, text: String) {
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
    let previous_id = snapshot.navigation.thread_id.clone();
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
    if snapshot.navigation.cwd != navigation.cwd || navigation.draft_key.is_empty() {
        clear_workspace_location(Arc::make_mut(&mut snapshot.workspace));
    }
    if let Some(id) = &navigation.thread_id
        && snapshot.activity.unread.contains(id)
    {
        Arc::make_mut(&mut snapshot.activity).unread.remove(id);
    }
    snapshot.navigation = Arc::new(navigation);
}

fn prepare<O: operations::Operation>(
    previous: &Snapshot,
    mut next: Snapshot,
    mut operation: O,
) -> (Snapshot, Vec<Effect>) {
    if operation.invalidates() {
        next.epoch += 1;
    }
    if let Err(error) = operation.prepare(&mut next) {
        return reduce(previous, Event::Failed(error));
    }
    (next, vec![Effect::execute(operation)])
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

        Event::StorageScope(scope) => {
            if next.storage_scope == scope {
                return (next, Vec::new());
            }
            next.terminals = Arc::default();
            if !next.storage_scope.is_empty() {
                let archived = ScopedData {
                    drafts: std::mem::take(&mut next.drafts),
                    pending_submissions: std::mem::take(&mut next.pending_submissions),
                    file_drafts: std::mem::take(&mut next.file_drafts),
                    navigation: std::mem::take(&mut next.navigation),
                    activity: std::mem::take(&mut next.activity),
                };
                Arc::make_mut(&mut next.archived_scopes)
                    .insert(next.storage_scope.clone(), Arc::new(archived));
                if let Some(saved) = Arc::make_mut(&mut next.archived_scopes).remove(&scope) {
                    next.drafts = saved.drafts.clone();
                    next.pending_submissions = saved.pending_submissions.clone();
                    next.file_drafts = saved.file_drafts.clone();
                    next.navigation = saved.navigation.clone();
                    next.activity = saved.activity.clone();
                }
                next.conversations = Arc::default();
                next.threads = None;
                next.workspace = Arc::default();
                next.account = Arc::default();
                next.models = Arc::default();
                next.model_errors = Arc::default();
                reset_session(&mut next);
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
        Event::Connected => {
            next.operations = Arc::default();
            reset_session(&mut next);
            next.connected = true;
            next.error = None;
            next.list_query = Arc::new((*next.list_query).clone().for_connection());
            let mut effects = vec![
                Effect::execute(op::ListSessions::new((*next.list_query).clone())),
                Effect::execute(op::LoadModels {}),
                Effect::execute(op::ListAccounts {}),
            ];
            if let Some(thread_id) = &next.navigation.thread_id {
                effects.push(Effect::execute(op::ReadThread::open(thread_id.clone())));
            } else {
                effects.extend(op::review_workspace(&mut next));
            }
            for (handle, terminal) in next.terminals.iter() {
                if terminal.phase == TerminalPhase::Suspended {
                    effects.push(Effect::execute(op::StartTerminal {
                        handle: handle.clone(),
                        cwd: terminal.cwd.clone(),
                        size: terminal.size,
                    }));
                }
            }
            return (next, effects);
        }
        Event::Disconnected(reason) => {
            next.operations = Arc::default();
            reset_session(&mut next);
            next.connected = false;
            for terminal in Arc::make_mut(&mut next.terminals).values_mut() {
                if matches!(
                    terminal.phase,
                    TerminalPhase::Starting | TerminalPhase::Running
                ) {
                    Arc::make_mut(terminal).phase = TerminalPhase::Suspended;
                }
            }
            next.error = Some(reason);
        }
        Event::Failed(error) => next.error = Some(error),
        Event::Intent(_) | Event::Notification(_) | Event::SessionUpdate(_) => {
            unreachable!("handled by the reducer router")
        }
    }
    (next, Vec::new())
}

/// Cached history and drafts survive; IDs and activity belong to one connection.
/// Pending submissions are persisted only to recover dictation after a crash.
fn reset_session(snapshot: &mut Snapshot) {
    snapshot.composer_catalog = None;
    snapshot.permission_settings = None;
    snapshot.subscriptions = Arc::default();
    for thread in Arc::make_mut(&mut snapshot.conversations).values_mut() {
        if !thread.requests.is_empty() || !thread.submissions.is_empty() {
            let thread = Arc::make_mut(thread);
            thread.requests.clear();
            thread.submissions.clear();
        }
    }
    if !snapshot.activity.active.is_empty() {
        Arc::make_mut(&mut snapshot.activity).active.clear();
    }
    snapshot.epoch += 1;
    for pending in Arc::make_mut(&mut snapshot.pending_submissions).values_mut() {
        Arc::make_mut(pending).delivery_unknown = true;
    }
    if snapshot
        .conversations
        .values()
        .any(|thread| has_session_status(thread))
    {
        for thread in Arc::make_mut(&mut snapshot.conversations).values_mut() {
            if has_session_status(thread) {
                clear_session_status(Arc::make_mut(thread));
            }
        }
    }
    if let Some(list) = &mut snapshot.threads
        && list.data.iter().any(has_session_status)
    {
        for thread in &mut Arc::make_mut(list).data {
            clear_session_status(thread);
        }
    }
}
fn has_session_status(thread: &Thread) -> bool {
    thread.status == crate::models::SessionStatus::Running
        || thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Running)
}
fn clear_session_status(thread: &mut Thread) {
    if thread.status == crate::models::SessionStatus::Running {
        thread.status = crate::models::SessionStatus::Unknown;
    }
    for turn in thread.turns.iter_mut().flatten() {
        if turn.status == agent_protocol::execution::TurnStatus::Running {
            Arc::make_mut(turn).status = crate::models::TurnStatus::Unknown;
        }
    }
}

pub(crate) fn supported_settings<'a>(
    selected_model: Option<&'a crate::models::ModelRef>,
    selected_effort: Option<&'a str>,
    selected_tier: Option<&'a str>,
    default_provider: Option<crate::session::ProviderKind>,
    models: &'a [Model],
    catalog_incomplete: bool,
) -> (
    Option<&'a crate::models::ModelRef>,
    Option<&'a str>,
    Option<&'a str>,
) {
    // Absence in an incomplete catalog is not evidence that a saved choice was removed.
    if catalog_incomplete
        && selected_model.is_some()
        && !models
            .iter()
            .any(|model| Some(&model.model) == selected_model)
    {
        return (selected_model, selected_effort, selected_tier);
    }
    let provider = selected_model
        .filter(|model| !model.id.is_empty())
        .map(|model| model.provider)
        .or(default_provider);
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
        return if models.is_empty() {
            (selected_model, None, None)
        } else {
            // The other provider's catalog cannot validate this draft's options.
            (selected_model, selected_effort, selected_tier)
        };
    };
    let changed = selected_model.is_some() && selected_model != Some(&model.model);
    let effort = model
        .supported_reasoning_efforts
        .iter()
        .find(|effort| !changed && Some(effort.reasoning_effort.as_str()) == selected_effort)
        .or_else(|| {
            model
                .supported_reasoning_efforts
                .iter()
                .find(|effort| effort.reasoning_effort == model.default_reasoning_effort)
        })
        .or_else(|| model.supported_reasoning_efforts.first());
    let supported_tier = |id: &str| {
        id == "default"
            || model
                .service_tiers
                .as_ref()
                .is_some_and(|tiers| tiers.iter().any(|tier| tier.id == id))
    };
    let tier = selected_tier
        .filter(|tier| !changed && supported_tier(tier))
        .or_else(|| {
            model
                .default_service_tier
                .as_deref()
                .filter(|tier| supported_tier(tier))
        })
        .unwrap_or("default");
    (
        Some(&model.model),
        effort.map(|effort| effort.reasoning_effort.as_str()),
        Some(tier),
    )
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

fn submission(
    previous: &Snapshot,
    thread_id: Option<crate::session::SessionRef>,
    draft_key: DraftKey,
    draft: Arc<Draft>,
    client_user_message_id: agent_protocol::ids::ClientInputId,
    clear_draft: Option<Arc<Draft>>,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    next.error = None;
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
            provider: crate::presentation::model_settings::draft_provider(
                &draft_key,
                draft.model.as_ref(),
            ),
            draft_key,
            cwd: (!previous.navigation.cwd.trim().is_empty())
                .then(|| previous.navigation.cwd.clone()),
            client_user_message_id,
            draft,
        }),
    };
    (next, vec![effect])
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

fn set_model_defaults(
    snapshot: &mut Snapshot,
    scope: ModelDefaultsScope,
    mut defaults: ModelDefaults,
) {
    defaults
        .providers
        .retain(|_, settings| *settings != ProviderModelDefaults::default());
    if scope == ModelDefaultsScope::Global {
        snapshot.model_defaults = defaults;
    } else if snapshot.scoped_model_defaults.get(&scope) != Some(&defaults) {
        Arc::make_mut(&mut snapshot.scoped_model_defaults).insert(scope, defaults);
    }
}

impl Snapshot {
    /// Storage scope is Host node ID plus its session namespace. Model presets
    /// belong to the Host even when that namespace changes.
    pub(crate) fn model_environment_id(&self) -> &str {
        self.storage_scope.split(':').next().unwrap_or_default()
    }

    pub(crate) fn model_defaults_for_cwd(&self, cwd: &str) -> ModelDefaults {
        let project = self.threads.as_ref().and_then(|list| {
            list.projects
                .iter()
                .flat_map(|project| {
                    project.roots.iter().filter_map(move |root| {
                        let trimmed = root.path.trim_end_matches(['/', '\\']);
                        let path = if trimmed.is_empty() {
                            root.path.as_str()
                        } else {
                            trimmed
                        };
                        (!path.is_empty()
                            && (cwd == path
                                || cwd.strip_prefix(path).is_some_and(|rest| {
                                    path.ends_with(['/', '\\']) || rest.starts_with(['/', '\\'])
                                })))
                        .then_some((path.len(), &project.id))
                    })
                })
                .max_by_key(|(length, _)| *length)
                .map(|(_, id)| id.clone())
        });
        self.model_defaults(match project {
            Some(project) => ModelDefaultsScope::Project {
                environment: self.model_environment_id().to_owned(),
                project,
            },
            None => ModelDefaultsScope::Environment {
                id: self.model_environment_id().to_owned(),
            },
        })
    }
}

#[cfg(test)]
#[path = "state/model_defaults_tests.rs"]
mod model_defaults_tests;
