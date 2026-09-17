//! Immutable client state and pure conversation transitions.
use crate::{
    client::{Answer, ServerRequest},
    models::{
        FileContent, FileList, HostStatus, Invitation, Item, ListQuery, Model, RemoteHost, Thread,
        ThreadList, WorkspaceReview, WorktreeSettings,
    },
};
use operations as op;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    pub directory: Option<Arc<FileList>>,
    pub file: Option<Arc<FileContent>>,
    pub review_cwd: Option<String>,
    pub review: Option<Arc<WorkspaceReview>>,
    pub settings: Option<Arc<WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<crate::models::Worktree>>>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Navigation {
    pub thread_id: Option<String>,
    pub cwd: String,
    pub draft_key: String,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    #[serde(skip)]
    pub active: BTreeMap<String, bool>,
    pub unread: BTreeSet<String>,
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
    pub accounts: Option<Arc<crate::client::Accounts>>,
    #[serde(skip)]
    pub login: Option<Arc<crate::client::AccountLogin>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSubmission {
    pub sequence: u64,
    pub draft_key: String,
    pub draft: Arc<Draft>,
    pub turn_id: Option<String>,
    pub after_item_id: Option<String>,
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
    pub reset_size: Option<crate::client::TerminalSize>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    pub cwd: String,
    pub size: crate::client::TerminalSize,
    pub phase: TerminalPhase,
    pub output: VecDeque<Arc<TerminalOutput>>,
    pub sequence: u64,
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalView {
    pub status: String,
    pub accepts_input: bool,
    pub output: Vec<TerminalOutput>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    pub storage_scope: String,
    pub archived_scopes: Arc<BTreeMap<String, Arc<ScopedData>>>,
    #[serde(default)]
    pub account: Arc<AccountState>,
    #[serde(skip)]
    pub terminals: Arc<BTreeMap<String, Arc<Terminal>>>,
    #[serde(default)]
    pub conversations: Arc<BTreeMap<String, Arc<Thread>>>,
    #[serde(default)]
    pub threads: Option<Arc<ThreadList>>,
    #[serde(default)]
    pub models: Arc<Vec<Model>>,
    #[serde(default)]
    pub model_errors: Arc<Map<String, Value>>,
    #[serde(skip)]
    pub requests: Arc<BTreeMap<String, Arc<ServerRequest>>>,
    pub drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    pub pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, FileDraft>>,
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
    pub subscriptions: Arc<BTreeMap<String, uuid::Uuid>>,
    #[serde(skip)]
    pub error: Option<String>,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn account_models(&self, id: String) -> Vec<Model> {
        let Some(account) = self.account.accounts.as_ref().and_then(|accounts| {
            accounts
                .accounts
                .iter()
                .find(|account| account.id == id && accounts.is_selected(account))
        }) else {
            return Vec::new();
        };
        self.models
            .iter()
            .filter(|model| crate::models::model_provider(&model.model) == account.provider)
            .cloned()
            .collect()
    }

    pub fn account_is_active_for_draft(&self, id: String, thread_id: String) -> bool {
        let selected = self
            .drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_deref());
        self.account.accounts.as_ref().is_some_and(|accounts| {
            accounts.accounts.iter().any(|account| {
                account.id == id
                    && accounts.is_selected(account)
                    && self.models.iter().any(|model| {
                        Some(model.model.as_str()) == selected
                            && crate::models::model_provider(&model.model) == account.provider
                    })
            })
        })
    }

    pub fn terminal_view(&self, handle: String) -> Option<TerminalView> {
        self.terminals.get(&handle).map(|terminal| TerminalView {
            status: terminal.phase.label(),
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

    pub fn model_error_messages(&self) -> Vec<String> {
        self.model_errors
            .iter()
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
    pub drafts: Arc<BTreeMap<String, Arc<Draft>>>,
    pub pending_submissions: Arc<BTreeMap<String, Arc<PendingSubmission>>>,
    pub file_drafts: Arc<BTreeMap<String, FileDraft>>,
    pub navigation: Arc<Navigation>,
    pub activity: Arc<Activity>,
}

/// Durable client-owned data. Host results and connection state stay in memory.
#[derive(Debug, Serialize)]
pub struct LocalState<'a> {
    storage_scope: &'a str,
    archived_scopes: &'a BTreeMap<String, Arc<ScopedData>>,
    drafts: &'a BTreeMap<String, Arc<Draft>>,
    pending_submissions: &'a BTreeMap<String, Arc<PendingSubmission>>,
    file_drafts: &'a BTreeMap<String, FileDraft>,
    navigation: &'a Navigation,
    activity: &'a Activity,
}

impl Snapshot {
    pub fn local_state(&self) -> LocalState<'_> {
        LocalState {
            storage_scope: &self.storage_scope,
            archived_scopes: &self.archived_scopes,
            drafts: &self.drafts,
            pending_submissions: &self.pending_submissions,
            file_drafts: &self.file_drafts,
            navigation: &self.navigation,
            activity: &self.activity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    StorageScope(String),
    TerminalFailed { handle: String, reason: String },
    Intent(Intent),
    SubmissionFailed(String),
    SubmissionUnknown(String),

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
    match event {
        Event::Intent(intent) => reduce_intent(previous, intent),

        Event::Notification(message) => notification(previous, message),
        Event::SessionUpdate(update) => notifications::session_update(previous, *update),
        event => reduce_event(previous, event),
    }
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
        ListAccounts, SelectAccount, SelectAccountForDraft, LogoutAccount, StartAccountLogin,
        ReadAccountLogin, CancelAccountLogin, SubmitAccountLogin, ForkThread,
        StartTerminal, DetachTerminal, KillTerminal, CreateInvitation, RemoveRemoteHost,
        RevokeDevice, ListFiles, ReadFile,
        SaveFile, ReviewWorkspace, ReadWorktreeSettings,
        UpdateWorktreeSettings, ListWorktrees, RemoveWorktree, ListThreads, AddProject, StartThread,
        ReadThread, OpenRequest, ReadItem, ResizeTerminal,
        Interrupt,
        WriteTerminal, DownloadFile, LoadSessionImages, LoadVisualization,
        LoadHostManagement, LoadModels,
        Respond, Transcribe, UploadAttachment, PairRemoteHost,
    ], {
        Intent::ReadOlder { thread_id } => {
            let limit = u32::try_from(previous.conversations.get(&thread_id).map_or(5, |thread| {
                thread.history_limit
                    .unwrap_or_else(|| thread.turns.as_ref().map_or(5, |turns| turns.len() as u64))
            })).unwrap_or(u32::MAX).saturating_add(5);
            return prepare(previous, next, op::ReadThread { limit, ..op::ReadThread::new(thread_id) });
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
            return prepare(previous, next, operations::ListThreads { query });
        }

        Intent::ShowThreadList => {
            next.epoch += 1;
            navigate(&mut next, Navigation::default());
            return (next, Vec::new());
        }
        Intent::NewChat { cwd } => {
            next.epoch += 1;
            let key = format!("new:{cwd}");
            if !previous.drafts.contains_key(&key) {
                let draft = Draft::default();
                let (model, effort, tier) = supported_settings(&draft, &previous.models, &previous.model_errors);
                let draft = Draft {
                    model: model.map(str::to_owned),
                    effort: effort.map(str::to_owned),
                    service_tier: tier.map(str::to_owned),
                    ..draft
                };
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
                    FileDraft {
                        revision: revision.into(),
                        text,
                    },
                );
            } else {
                next.error = Some("file has not been loaded".into());
            }
        }

        Intent::SetDraft { thread_id, draft } => {
            Arc::make_mut(&mut next.drafts).insert(thread_id, Arc::new(draft));
        }
        Intent::SetDraftText { thread_id, text } => {
            if previous
                .drafts
                .get(&thread_id)
                .is_none_or(|draft| draft.text != text)
            {
                let draft = Arc::make_mut(&mut next.drafts)
                    .entry(thread_id)
                    .or_default();
                Arc::make_mut(draft).text = text;
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
                    if draft.model.as_deref() != Some(&model) {
                        draft.effort = None;
                        draft.service_tier = None;
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
                let (model, effort, tier) = supported_settings(&draft, &previous.models, &previous.model_errors);
                let settings = (
                    model.map(str::to_owned),
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
    if operation.invalidates(previous) {
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
            if let Some(pending) = shared_mut(&mut next.pending_submissions, &id) {
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
            reset_session(&mut next);
            next.connected = true;
            next.error = None;
            let query = Arc::make_mut(&mut next.list_query);
            if query.project_limit == 0 {
                query.project_limit = 5;
            }
            if query.chat_limit == 0 {
                query.chat_limit = 5;
            }
            let mut effects = vec![
                Effect::execute(op::ListThreads::new(query.clone())),
                Effect::execute(op::LoadModels {}),
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
            reset_session(&mut next);
            next.connected = false;
            next.requests = Arc::default();
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
    snapshot.subscriptions = Arc::default();
    for thread in Arc::make_mut(&mut snapshot.conversations).values_mut() {
        if !thread.requests.is_empty() {
            Arc::make_mut(thread).requests.clear();
        }
    }
    if !snapshot.requests.is_empty() {
        snapshot.requests = Arc::default();
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
    thread
        .status
        .as_ref()
        .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::Active)
        || thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.status.as_deref() == Some("inProgress"))
}
fn clear_session_status(thread: &mut Thread) {
    if thread
        .status
        .as_ref()
        .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::Active)
    {
        thread.status = None;
    }
    for turn in thread.turns.iter_mut().flatten() {
        if turn.status.as_deref() == Some("inProgress") {
            Arc::make_mut(turn).status = None;
        }
    }
}

fn supported_settings<'a>(
    draft: &'a Draft,
    models: &'a [Model],
    errors: &Map<String, Value>,
) -> (Option<&'a str>, Option<&'a str>, Option<&'a str>) {
    // Absence in an incomplete catalog is not evidence that a saved choice was removed.
    if !errors.is_empty()
        && draft.model.is_some()
        && !models
            .iter()
            .any(|model| Some(model.model.as_str()) == draft.model.as_deref())
    {
        return (
            draft.model.as_deref(),
            draft.effort.as_deref(),
            draft.service_tier.as_deref(),
        );
    }
    let provider = draft
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .map(crate::models::model_provider);
    let mut available = models.iter().filter(|model| {
        provider.is_none_or(|provider| crate::models::model_provider(&model.model) == provider)
    });
    let model = available
        .clone()
        .find(|model| Some(model.model.as_str()) == draft.model.as_deref())
        .or_else(|| {
            available
                .clone()
                .find(|model| model.is_default == Some(true))
        })
        .or_else(|| available.next());
    let Some(model) = model else {
        return (draft.model.as_deref(), None, None);
    };
    let changed = draft.model.as_deref() != Some(&model.model);
    let effort = model
        .supported_reasoning_efforts
        .iter()
        .find(|effort| {
            !changed && Some(effort.reasoning_effort.as_str()) == draft.effort.as_deref()
        })
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
    let tier = draft
        .service_tier
        .as_deref()
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

/// Immutable lookup projection for native panels. Session.requests owns the
/// state; this index only shares its request Arcs and is never persisted.
fn project_requests(snapshot: &mut Snapshot) {
    let requests: BTreeMap<_, _> = snapshot
        .conversations
        .values()
        .flat_map(|thread| {
            thread
                .requests
                .iter()
                .map(|(id, request)| (id.clone(), request.clone()))
        })
        .collect();
    if *snapshot.requests != requests {
        snapshot.requests = Arc::new(requests);
    }
}

fn shared_mut<'a, T: Clone>(
    values: &'a mut Arc<BTreeMap<String, Arc<T>>>,
    key: &str,
) -> Option<&'a mut T> {
    if !values.contains_key(key) {
        return None;
    }
    Arc::make_mut(values).get_mut(key).map(Arc::make_mut)
}

fn upsert_item(previous: &Snapshot, thread_id: &str, turn_id: &str, item: Item) -> Snapshot {
    let mut next = previous.clone();
    if let Some(thread) = previous.conversations.get(thread_id) {
        match (crate::session::SessionChange::Item {
            turn_id: turn_id.into(),
            item: item.into(),
        })
        .apply(thread)
        {
            Ok(thread) => {
                Arc::make_mut(&mut next.conversations).insert(thread_id.into(), Arc::new(thread));
            }
            Err(error) => next.error = Some(error.into()),
        }
    }
    next
}

fn submission(
    previous: &Snapshot,
    thread_id: Option<String>,
    draft_key: String,
    draft: Arc<Draft>,
    client_user_message_id: String,
    clear_draft: Option<Arc<Draft>>,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    next.error = None;
    if let Some(reason) = thread_id
        .as_ref()
        .and_then(|id| previous.conversations.get(id))
        .and_then(|thread| crate::session::input_unavailable_reason(thread))
    {
        next.error = Some(reason);
        return (next, Vec::new());
    }
    let cleared = clear_draft.as_ref().unwrap_or(&draft);
    if let Some(current) = shared_mut(&mut next.drafts, &draft_key) {
        if current.text == cleared.text {
            current.text.clear();
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

fn reconcile_pending(snapshot: &mut Snapshot, thread_id: &str) {
    let Some(thread) = snapshot.conversations.get(thread_id) else {
        return;
    };
    let echoed = |id: &String, pending: &Arc<PendingSubmission>| {
        (pending.accepted || pending.delivery_unknown)
            && pending.draft_key == thread_id
            && thread
                .turns
                .iter()
                .flatten()
                .flat_map(|turn| turn.items.iter().flatten())
                .any(|item| {
                    item.kind.as_deref() == Some("userMessage")
                        && item.client_id.as_ref() == Some(id)
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
