//! Device state published to native views. The Host owns conversation
//! decisions; this holds what the device folded, sent and is editing.
use crate::commands::build::FollowUpBehavior;
use crate::commands::outbox::Outbox;
use crate::sync::{ShellCache, ShellStatus, ThreadSync};
use agent_domain::{
    Attachment, AttachmentKind, CheckpointId, Driver, InteractionMode, MessageContext,
    ModelSelection, RunId, RuntimeMode, State, ThreadId, ThreadShell, WorktreeSetupSnapshot,
};
use agent_protocol::conversation::{SearchMatch, ShellSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

mod device;
mod sources;
pub use device::*;
pub use sources::*;

/// The project a new thread uses when none is chosen.
pub const CHATS_PROJECT: &str = "chats";

/// Copy-on-write storage shared between published snapshots.
#[derive(Debug, PartialEq)]
pub struct Shared<T>(Arc<T>);
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}
impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}
impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}
impl<T> Shared<T> {
    pub fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelOption {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    pub attachments: Vec<DraftAttachment>,
    pub text: String,
    pub instance_id: String,
    pub driver: Driver,
    pub model: String,
    pub options: Vec<ModelOption>,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    /// Payloads behind the text's context links.
    pub context: Option<MessageContext>,
    /// Where a new thread's first run works; only new-thread drafts set it.
    pub workspace: Option<DraftWorkspace>,
}

/// The new-thread composer's workspace choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftWorkspace {
    pub mode: crate::view::projects::selection::ThreadWorkspaceMode,
    /// The branch to work on locally, or the base of a new worktree.
    pub branch: Option<String>,
    /// An existing worktree that has the branch checked out.
    pub worktree_path: Option<String>,
    pub start_from_origin: bool,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            attachments: vec![],
            text: String::new(),
            instance_id: String::new(),
            driver: Driver::Codex,
            model: String::new(),
            options: vec![],
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            context: None,
            workspace: None,
        }
    }
}
fn attachment_error(code: &str) -> String {
    match code {
        "too-many-attachments" => "You can attach up to 100 files per message.",
        "duplicate-attachment-id" => "Duplicate attachment ids are not allowed.",
        "image-too-large" => "Images must be 10 MB or smaller.",
        _ => "This attachment cannot be sent.",
    }
    .into()
}
impl Draft {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.attachments.is_empty()
    }
    pub fn attachment_refs(&self) -> Result<Vec<Attachment>, String> {
        let attachments = self
            .attachments
            .iter()
            .map(DraftAttachment::reference)
            .collect::<Result<Vec<_>, _>>()?;
        agent_domain::validate_attachments(&attachments).map_err(attachment_error)?;
        Ok(attachments)
    }
    pub fn selection(&self) -> Result<ModelSelection, String> {
        if self.model.trim().is_empty() || self.instance_id.trim().is_empty() {
            return Err("Select a model".into());
        }
        Ok(ModelSelection {
            instance: self.instance_id.clone(),
            driver: self.driver,
            model: self.model.clone(),
            options: self
                .options
                .iter()
                .map(|option| (option.key.clone(), option.value.clone()))
                .collect(),
        })
    }
    pub fn with_selection(mut self, selection: &ModelSelection) -> Self {
        self.instance_id = selection.instance.clone();
        self.driver = selection.driver;
        self.model = selection.model.clone();
        self.options = selection
            .options
            .iter()
            .map(|(key, value)| ModelOption {
                key: key.clone(),
                value: value.clone(),
            })
            .collect();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftAttachment {
    pub id: String,
    pub remote_id: Option<String>,
    pub name: String,
    pub mime_type: String,
    pub kind: String,
    pub size_bytes: u64,
    pub local_path: String,
    pub status: String,
    pub error: Option<String>,
}
pub fn native_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}
impl DraftAttachment {
    pub fn metadata(&self) -> Attachment {
        Attachment {
            kind: if self.kind == "image" {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            source: None,
            id: self.remote_id.clone().unwrap_or_else(|| self.id.clone()),
            name: self.name.clone(),
            mime_type: self.mime_type.clone(),
            path: String::new(),
            size: self.size_bytes,
        }
    }
    pub fn reference(&self) -> Result<Attachment, String> {
        if self.status != "ready" {
            return Err("Wait for attachments to upload, or retry failed uploads.".into());
        }
        self.remote_id
            .as_ref()
            .ok_or("Attachment has not uploaded")?;
        Ok(self.metadata())
    }
    pub fn from_remote(attachment: &Attachment) -> Self {
        Self {
            id: attachment.id.clone(),
            remote_id: Some(attachment.id.clone()),
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            kind: match attachment.kind {
                AttachmentKind::Image => "image",
                AttachmentKind::File => "file",
            }
            .into(),
            size_bytes: attachment.size,
            local_path: String::new(),
            status: "ready".into(),
            error: None,
        }
    }
}

/// Keeps text appended to the draft (by dictation) after the base the native
/// edit started from.
pub fn merge_draft_text(base: String, edited: String, current: String) -> String {
    if current != base && current.starts_with(&base) {
        format!("{}{}", edited, &current[base.len()..])
    } else {
        edited
    }
}

/// Restored content joins the draft after a blank line, once.
pub fn merge_restored_text(existing: &str, incoming: &str) -> String {
    if incoming.is_empty() {
        return existing.into();
    }
    if existing.is_empty() {
        return incoming.into();
    }
    if existing == incoming || existing.ends_with(&format!("\n\n{incoming}")) {
        return existing.into();
    }
    format!("{existing}\n\n{incoming}")
}

/// A rollback whose rolled-back message returns to the composer once it succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRollback {
    pub thread: ThreadId,
    pub checkpoint: CheckpointId,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    pub store_id: String,
    pub revision: u64,
    pub connected: bool,
    pub host_name: Option<String>,
    pub error: Option<String>,
    pub shell: Arc<ShellCache>,
    /// Open only while the archive is shown.
    pub archived: Option<Arc<ShellCache>>,
    pub threads: BTreeMap<ThreadId, Arc<ThreadSync>>,
    pub setups: BTreeMap<ThreadId, WorktreeSetupSnapshot>,
    pub outbox: Arc<Outbox>,
    pub rollbacks: BTreeMap<agent_domain::CommandId, PendingRollback>,
    pub drafts: Shared<BTreeMap<String, Draft>>,
    pub default_draft: Draft,
    pub follow_up: FollowUpBehavior,
    pub selected_thread: Option<ThreadId>,
    pub selected_project: Option<String>,
    pub editing_run: Option<RunId>,
    pub search: String,
    pub search_matches: Vec<SearchMatch>,
    /// The Host's provider instances and their models; `None` until listed.
    pub providers: Option<Vec<crate::models::ProviderInstance>>,
    pub workspace: Workspace,
    pub terminals: BTreeMap<String, Terminal>,
    /// Provider commands, path search, Git status, refs and diff previews.
    pub sources: WorkspaceSources,
    /// By project id.
    pub project_icons: BTreeMap<String, ProjectIconEntry>,
    /// What the Host's terminal metadata stream reports for every thread's
    /// terminals, by thread and terminal id.
    pub terminal_metadata:
        BTreeMap<(ThreadId, String), agent_protocol::operations::TerminalSummary>,
    pub accounts: Option<agent_protocol::operations::Accounts>,
    pub account_login: Option<agent_protocol::operations::AccountLogin>,
    pub host_status: Option<crate::models::HostStatus>,
    pub remote_hosts: Vec<crate::models::RemoteHost>,
    pub invitation: Option<crate::models::Invitation>,
    pub preferences: Preferences,
    pub inbox_returns: crate::view::inbox::InboxReturns,
    pub thread_order: Option<ThreadOrderHold>,
    /// The Host's conversation settings once read.
    pub conversation_settings: Option<crate::models::ConversationSettings>,
    pub session_import: SessionImport,
    /// Answer drafts by question request id.
    pub question_drafts: BTreeMap<String, QuestionDrafts>,
    pub diff_panels: BTreeMap<ThreadId, crate::view::checkpoints::DiffPanelSelection>,
    pub stash: Shared<crate::view::composer::stash::PromptStash>,
    /// The last setup a closed stream reported, kept for the card.
    pub held_setups: BTreeMap<ThreadId, WorktreeSetupSnapshot>,
    pub error_dismissals: crate::view::timeline::banners::ThreadErrorDismissals,
    /// Timeline rows already built; every snapshot of the store shares them.
    pub timelines: Arc<std::sync::Mutex<crate::view::timeline::rows::TimelineCache>>,
}

impl Snapshot {
    pub fn accepts_after(&self, previous: &Snapshot) -> bool {
        self.store_id != previous.store_id || self.revision >= previous.revision
    }
    pub fn terminal_available(&self) -> bool {
        self.connected && !self.cwd().is_empty()
    }
    /// The active shell with pending lifecycle previews applied.
    pub fn shell_view(&self) -> Option<Cow<'_, ShellSnapshot>> {
        self.shell
            .snapshot
            .as_ref()
            .map(|shell| self.outbox.overlay_shell(shell))
    }
    pub fn shell_status(&self) -> ShellStatus {
        self.shell.status
    }
    pub fn thread_row(&self, id: &ThreadId) -> Option<&ThreadShell> {
        [Some(&self.shell), self.archived.as_ref()]
            .into_iter()
            .flatten()
            .filter_map(|cache| cache.snapshot.as_ref())
            .flat_map(|shell| &shell.threads)
            .find(|row| &row.id == id)
    }
    pub fn thread(&self, id: &ThreadId) -> Option<&ThreadSync> {
        self.threads.get(id).map(Arc::as_ref)
    }
    pub fn thread_state(&self, id: &ThreadId) -> Option<&State> {
        self.thread(id)?.state.as_deref()
    }
    pub fn selected_state(&self) -> Option<&State> {
        self.thread_state(self.selected_thread.as_ref()?)
    }
    pub fn shell_projects(&self) -> &[crate::models::Project] {
        self.shell
            .snapshot
            .as_ref()
            .map_or(&[], |shell| shell.projects.as_slice())
    }
    pub fn draft_key(&self) -> String {
        if let (Some(thread), Some(run)) = (&self.selected_thread, &self.editing_run) {
            return format!("queue:{thread}:{run}");
        }
        self.selected_thread
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| self.new_thread_draft_key())
    }
    pub fn new_thread_draft_key(&self) -> String {
        format!(
            "new:{}",
            self.selected_project.as_deref().unwrap_or(CHATS_PROJECT)
        )
    }
    pub fn current_draft(&self) -> Draft {
        self.drafts
            .get(&self.draft_key())
            .cloned()
            .unwrap_or_else(|| {
                self.selected_thread.as_ref().map_or_else(
                    || self.default_draft.clone(),
                    |id| self.draft_for_thread(id),
                )
            })
    }
    /// A thread's draft, or one with the thread's model and modes.
    pub fn draft_for_thread(&self, id: &ThreadId) -> Draft {
        if let Some(draft) = self.drafts.get(id.as_str()) {
            return draft.clone();
        }
        let thread = self
            .thread_state(id)
            .and_then(|state| state.thread.as_ref())
            .map(|t| (&t.selection, t.runtime_mode, t.interaction_mode))
            .or_else(|| {
                self.thread_row(id)
                    .map(|row| (&row.selection, row.runtime_mode, row.interaction_mode))
            });
        match thread {
            Some((selection, runtime_mode, interaction_mode)) => Draft {
                runtime_mode,
                interaction_mode,
                ..Draft::default().with_selection(selection)
            },
            None => self.default_draft.clone(),
        }
    }
    /// A fork or merge back from this thread is waiting for the Host.
    pub fn context_pending(&self, thread: &ThreadId) -> bool {
        self.outbox.entries.iter().any(|entry| {
            &entry.thread == thread
                && matches!(
                    &entry.request,
                    crate::commands::outbox::Request::Dispatch(dispatch)
                        if matches!(dispatch.command, agent_domain::Command::Fork { .. } | agent_domain::Command::MergeBack { .. })
                )
        })
    }
    pub fn cwd(&self) -> String {
        self.directory_of(self.selected_thread.as_ref())
    }
    /// Where the composer's provider commands and `@` paths come from: the
    /// open thread's checkout, else the new-thread draft's worktree or its
    /// project's root.
    pub fn composer_cwd(&self) -> String {
        if self.selected_thread.is_some() {
            return self.cwd();
        }
        let workspace = self.new_thread_workspace();
        match workspace.mode {
            crate::view::projects::selection::ThreadWorkspaceMode::Local => workspace.worktree_path,
            crate::view::projects::selection::ThreadWorkspaceMode::Worktree => None,
        }
        .or_else(|| self.new_thread_project_root())
        .unwrap_or_default()
    }
    /// The root of the project a new thread would use; none for chats.
    pub fn new_thread_project_root(&self) -> Option<String> {
        let project = self.selected_project.as_deref()?;
        self.shell_projects()
            .iter()
            .find(|candidate| candidate.id == project)
            .and_then(|project| project.roots.first())
            .map(|root| root.path.clone())
    }
    /// The checked-out branch of the project's root and the worktree it is
    /// checked out in when that differs from the root.
    pub fn new_thread_local_selection(&self) -> (Option<String>, Option<String>) {
        let Some(root) = self.new_thread_project_root() else {
            return (None, None);
        };
        let current = self
            .sources
            .refs(&root, RefScope::All)
            .and_then(|entry| entry.list.as_ref())
            .and_then(|list| list.refs.iter().find(|branch| branch.current));
        match current {
            Some(branch) => (
                Some(branch.name.clone()),
                crate::view::new_thread::branch_worktree_path(
                    crate::view::projects::selection::ThreadWorkspaceMode::Local,
                    &root,
                    branch.worktree_path.as_deref(),
                ),
            ),
            None => (None, None),
        }
    }
    /// The new-thread draft's workspace: its choice, else the Host's default
    /// mode on the project's checkout.
    pub fn new_thread_workspace(&self) -> DraftWorkspace {
        use crate::view::projects::selection::ThreadWorkspaceMode;
        if let Some(workspace) = self
            .drafts
            .get(&self.new_thread_draft_key())
            .and_then(|draft| draft.workspace.clone())
        {
            return workspace;
        }
        let worktree = self.new_thread_project_root().is_some()
            && self
                .workspace
                .worktree_settings
                .as_ref()
                .is_some_and(|settings| settings.create_on_new_session);
        let mode = if worktree {
            ThreadWorkspaceMode::Worktree
        } else {
            ThreadWorkspaceMode::Local
        };
        let (branch, worktree_path) = match mode {
            ThreadWorkspaceMode::Local => self.new_thread_local_selection(),
            ThreadWorkspaceMode::Worktree => (None, None),
        };
        DraftWorkspace {
            mode,
            branch,
            worktree_path,
            start_from_origin: false,
        }
    }
    /// Where a thread's files and terminals open: its worktree or checkout,
    /// else its project's root.
    pub fn thread_cwd(&self, thread: &ThreadId) -> String {
        self.directory_of(Some(thread))
    }
    /// The project a thread belongs to.
    pub fn thread_project(&self, thread: &ThreadId) -> Option<&str> {
        self.thread_state(thread)
            .and_then(|state| state.thread.as_ref())
            .map(|thread| thread.project.as_str())
            .or_else(|| self.thread_row(thread).map(|row| row.project.as_str()))
    }
    fn directory_of(&self, thread: Option<&ThreadId>) -> String {
        let workspace = thread.and_then(|id| {
            self.thread_state(id)
                .and_then(|state| state.thread.as_ref())
                .map(|thread| (thread.workspace.clone(), thread.project.clone()))
                .or_else(|| {
                    self.thread_row(id)
                        .map(|row| (row.workspace.clone(), row.project.clone()))
                })
        });
        if let Some((Some(workspace), _)) = &workspace {
            return workspace
                .worktree_path
                .clone()
                .unwrap_or_else(|| workspace.cwd.clone());
        }
        let project = workspace
            .map(|(_, project)| project)
            .or_else(|| self.selected_project.clone())
            .unwrap_or_else(|| CHATS_PROJECT.into());
        self.shell_projects()
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| p.roots.first())
            .map(|root| root.path.clone())
            .unwrap_or_default()
    }
    /// A known terminal keeps the directory the Host reported; a new one opens
    /// in the thread's worktree, else its project's root, with the project
    /// script environment.
    pub fn terminal_location(&self, thread: &ThreadId, terminal_id: &str) -> TerminalLocation {
        let worktree_path = self
            .thread_state(thread)
            .and_then(|state| state.thread.as_ref())
            .map(|thread| thread.workspace.as_ref())
            .or_else(|| self.thread_row(thread).map(|row| row.workspace.as_ref()))
            .flatten()
            .and_then(|workspace| workspace.worktree_path.clone());
        let project_root = self
            .thread_project(thread)
            .and_then(|project| {
                self.shell_projects()
                    .iter()
                    .find(|candidate| candidate.id == project)
            })
            .and_then(|project| project.roots.first())
            .map(|root| root.path.clone())
            .unwrap_or_default();
        let known = self
            .terminal_metadata
            .get(&(thread.clone(), terminal_id.to_owned()));
        TerminalLocation {
            thread: thread.clone(),
            cwd: known.map_or_else(|| self.thread_cwd(thread), |summary| summary.cwd.clone()),
            worktree_path: known.map_or(worktree_path.clone(), |summary| {
                summary.worktree_path.clone()
            }),
            env: crate::view::projects::scripts::project_script_runtime_env(
                &project_root,
                worktree_path.as_deref(),
                &BTreeMap::new(),
            ),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspace {
    pub requested_directory: Option<String>,
    pub requested_file: Option<String>,
    pub directory: Option<crate::models::FileList>,
    pub file: Option<Arc<crate::models::FileContent>>,
    pub file_drafts: BTreeMap<String, Arc<FileDraft>>,
    pub review_generation: u64,
    pub review: Option<Arc<crate::models::WorkspaceReview>>,
    pub diff_request: Option<agent_protocol::conversation::GetTurnDiff>,
    pub worktree_settings: Option<crate::models::WorktreeSettings>,
    pub worktrees: Vec<crate::models::Worktree>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct FileDraft {
    pub text: String,
    pub revision: String,
}
/// One terminal of a thread, keyed by its handle in `Snapshot::terminals`.
#[derive(Debug, Clone, PartialEq)]
pub struct Terminal {
    pub thread: ThreadId,
    pub terminal_id: String,
    /// Terminals split side by side share the id of the first one.
    pub group: String,
    pub cwd: String,
    pub size: agent_protocol::operations::TerminalSize,
    pub phase: TerminalPhase,
    pub output: Shared<std::collections::VecDeque<Arc<TerminalOutput>>>,
    pub sequence: u64,
    pub output_bytes: usize,
    /// Written once the Host started the terminal (a project script's command).
    pub pending_input: Vec<u8>,
}
impl Terminal {
    pub fn clear_output(&mut self) {
        self.output.clear();
        self.output_bytes = 0;
    }
}

/// Where a thread's terminal opens and the environment its shell gets.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalLocation {
    pub thread: ThreadId,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TerminalPhase {
    Starting,
    Running,
    Suspended,
    Detached,
    Exited(i32),
    Failed(String),
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalOutput {
    pub sequence: u64,
    pub data: Vec<u8>,
    pub reset_size: Option<agent_protocol::operations::TerminalSize>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadAction {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Snooze {
        until: String,
    },
    Unsnooze,
    Rename {
        title: String,
    },
    RegenerateTitle,
    MarkUnread,
    AutoSettle {
        enabled: bool,
    },
    Archive,
    Unarchive,
    Delete,
    PinReorder {
        order_key: String,
    },
    ActiveReorder {
        order_key: String,
    },
    /// Records a visit at `at` (milliseconds), as dismissing a Woke mark
    /// does with the row's `woke_at`.
    Visit {
        at: i64,
    },
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueAction {
    Resume,
    Cancel {
        run_id: String,
    },
    Edit {
        run_id: String,
    },
    SaveEdit,
    CancelEdit,
    /// Moves one queued run before another, or to the end.
    Move {
        run_id: String,
        before_run_id: Option<String>,
    },
    Steer {
        run_id: String,
    },
}

/// A file on this device to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LocalFile {
    pub path: String,
    pub name: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AnswerEdit {
    ToggleOption { value: String },
    Custom { text: String },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Intent {
    // Navigation and filters.
    OpenThread {
        thread_id: String,
    },
    LeaveThread,
    NewThread {
        project_id: Option<String>,
    },
    ShowArchived {
        open: bool,
    },
    Search {
        query: String,
    },
    FilterProject {
        project_id: Option<String>,
    },
    Refresh,

    // Composer.
    EditDraft {
        text: String,
        base_text: Option<String>,
    },
    /// Chooses an item of the composer menu at `cursor` (UTF-16).
    SelectComposerItem {
        text: String,
        cursor: u32,
        item_id: String,
    },
    RemoveDraftContext {
        context_id: String,
    },
    /// Drops an unsent draft: a thread's (its id) or a new thread's (`new:<project>`).
    DiscardDraft {
        draft_key: String,
    },
    /// `draft_key` is the composer's (or an answer's) key when the files were picked.
    AttachFiles {
        draft_key: String,
        files: Vec<LocalFile>,
    },
    /// `draft_key` names an answer's files; `None` is the composer.
    RetryAttachment {
        draft_key: Option<String>,
        id: String,
    },
    RemoveAttachment {
        draft_key: Option<String>,
        id: String,
    },
    /// `alternate` is the second send gesture (Mod+Enter, long press).
    Send {
        alternate: bool,
    },
    Stop,
    StopSessions,
    /// Sends `/compact` to the open thread; the draft stays as it is.
    CompactContext,
    SetModel {
        instance_id: String,
        driver: Driver,
        model: String,
        options: Vec<ModelOption>,
    },
    SelectTrait {
        descriptor_id: String,
        choice: String,
    },
    ToggleTrait {
        descriptor_id: String,
        on: bool,
    },
    SetRuntimeMode {
        mode: RuntimeMode,
    },
    SetInteractionMode {
        mode: InteractionMode,
    },
    StashDraft,
    /// Images of a stash entry the client finished encoding.
    FinalizeStashImages {
        entry_id: String,
        images: crate::view::composer::stash::StashImages,
    },
    /// The entry's images return in `Outcome::StashRestored` for the client to attach.
    RestoreStash {
        entry_id: String,
    },
    DeleteStash {
        entry_id: String,
    },
    Transcribe {
        draft_key: String,
        preparation: Option<String>,
        audio: Vec<u8>,
    },

    // Queue, requests and plans.
    Queue {
        action: QueueAction,
    },
    RespondApproval {
        request_id: String,
        decision: String,
    },
    EditAnswer {
        request_id: String,
        question_id: String,
        edit: AnswerEdit,
    },
    ShowQuestion {
        request_id: String,
        index: u32,
    },
    SubmitAnswers {
        request_id: String,
    },
    DismissInput {
        request_id: String,
    },
    PlanFollowUp {
        new_thread: bool,
    },

    // Thread lifecycle.
    Thread {
        thread_id: String,
        action: ThreadAction,
    },
    ReorderPinned {
        thread_id: String,
        before_thread_id: Option<String>,
    },
    /// A move in the mobile list; the list holds the new order until it lands.
    MoveThread {
        thread_id: String,
        section: crate::view::thread_order::OrderSection,
        destination: crate::view::thread_order::MoveDestination,
    },
    /// A drop planned by `Snapshot::sidebar_drop`.
    DropThread {
        thread_id: String,
        plan: crate::view::sidebar::SidebarThreadDropPlan,
    },
    LimitRecovery {
        thread_id: String,
        action: crate::view::timeline::banners::RecoveryAction,
    },
    /// Hides a thread error banner by its `dismiss_key` for this app session.
    DismissThreadError {
        dismiss_key: String,
    },
    Fork {
        source_thread_id: String,
        run_id: String,
    },
    MergeBack,
    Rollback {
        checkpoint_id: String,
        restore_files: bool,
    },
    DiscardPending {
        command_id: String,
    },

    // Timeline.
    LoadEarlier,
    LoadItemDetail {
        item_id: String,
    },
    CancelSetup,

    // Diff panel of the open thread.
    SelectDiffScope {
        choice: crate::view::checkpoints::DiffScopeChoice,
    },
    SelectDiffTurn {
        run_id: String,
        file_path: Option<String>,
    },
    SelectDiffBaseRef {
        base_ref: Option<String>,
    },
    SetDiffIgnoreWhitespace {
        ignore: bool,
    },
    LoadDiff,
    ReviewWorkspace {
        cwd: String,
    },
    ReadTurnDiff {
        from_run_ordinal: u64,
        to_run_ordinal: u64,
        ignore_whitespace: bool,
    },

    // Thread terminals.
    OpenTerminal {
        thread_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    NewTerminal {
        thread_id: String,
        cols: u16,
        rows: u16,
    },
    SplitTerminal {
        thread_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    WriteTerminal {
        thread_id: String,
        terminal_id: String,
        data: Vec<u8>,
    },
    ResizeTerminal {
        thread_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    DetachTerminal {
        thread_id: String,
        terminal_id: String,
    },
    CloseTerminal {
        thread_id: String,
        terminal_id: String,
    },
    /// The composer's text or cursor (UTF-16) changed; loads what its `/`,
    /// `$` and `@` menu lists.
    UpdateComposerMenu {
        text: String,
        cursor: u32,
        layout: crate::view::timeline::rows::TimelineLayout,
    },
    /// Lists the branches the diff panel's base picker offers.
    SearchDiffBaseRefs {
        query: String,
    },
    /// Lists the branches the new-thread branch picker offers.
    SearchNewThreadBranches {
        query: String,
    },
    /// The next page of the new-thread branch picker.
    LoadMoreNewThreadBranches,
    SetNewThreadWorkspace {
        mode: crate::view::projects::selection::ThreadWorkspaceMode,
    },
    /// Chooses the branch to work on locally, or the base of a new worktree.
    SelectNewThreadBranch {
        branch: String,
        worktree_path: Option<String>,
    },
    SetNewThreadStartFromOrigin {
        on: bool,
    },
    /// A new thread in the project on a branch, from a thread's menu.
    NewThreadOnBranch {
        project_id: String,
        branch: String,
        worktree_path: Option<String>,
    },
    /// Saves a project's icon file; `None` finds it automatically.
    SetProjectIcon {
        project_id: String,
        path: Option<String>,
    },
    /// Prepares the workspace of a run whose preparation failed again.
    RetryPreparation {
        run_id: String,
    },
    /// Stops the open thread's worktree setup and sends its first message
    /// again as a new thread on the project's checkout.
    WorkLocally,
    /// Empties the terminal's history and screens; the shell keeps running.
    ClearTerminal {
        thread_id: String,
        terminal_id: String,
    },
    /// Starts a new shell with an empty history.
    RestartTerminal {
        thread_id: String,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    RunProjectScript {
        thread_id: String,
        script_id: String,
        cols: u16,
        rows: u16,
    },

    // Settings and projects.
    SetFollowUpBehavior {
        behavior: crate::commands::build::FollowUpBehavior,
    },
    SetTimestampFormat {
        format: crate::view::time::TimestampFormat,
    },
    SetWorkingSection {
        enabled: bool,
    },
    /// The model new threads start with; an open thread keeps its own.
    SetDefaultModel {
        instance_id: String,
        driver: Driver,
        model: String,
        options: Vec<ModelOption>,
    },
    /// The permissions new threads start with; an open thread keeps its own.
    SetDefaultRuntimeMode {
        mode: RuntimeMode,
    },
    ToggleFavoriteModel {
        instance_id: String,
        model: String,
    },
    SetModelOrder {
        instance_id: String,
        models: Vec<String>,
    },
    LoadConversationSettings,
    UpdateConversationSettings {
        scope: crate::view::settings::SettingsScope,
        change: crate::view::settings::ConversationSettingChange,
    },
    ResetProjectSettings {
        project_id: String,
    },
    AddProject {
        path: String,
    },
    UpdateProjectScripts {
        project_id: String,
        scripts: Vec<crate::models::ProjectScript>,
    },
    ScanSessions,
    SelectImportSessions {
        paths: Vec<String>,
        checked: bool,
    },
    ImportSessions,
    CloseImport,
    LoadWorktreeSettings,
    SaveWorktreeSettings {
        settings: crate::models::WorktreeSettings,
    },
    ListWorktrees,
    RemoveWorktree {
        path: String,
    },

    // Files.
    ListFiles {
        path: String,
    },
    ReadFile {
        path: String,
        discard_draft: bool,
    },
    EditFile {
        path: String,
        text: String,
    },
    SaveFile {
        path: String,
    },

    // Accounts and Hosts.
    LoadAccounts,
    SelectAccount {
        provider: crate::provider::ProviderKind,
        id: String,
    },
    StartLogin {
        provider: crate::provider::ProviderKind,
    },
    CompleteLogin {
        provider: crate::provider::ProviderKind,
        id: String,
        code: String,
    },
    CancelLogin {
        provider: crate::provider::ProviderKind,
        id: String,
    },
    DeleteAccount {
        provider: crate::provider::ProviderKind,
        id: String,
    },
    LoadHostStatus,
    LoadRemoteHosts,
    LoadHostManagement,
    PairRemoteHost {
        invitation: crate::models::Invitation,
        name: String,
    },
    RemoveRemoteHost {
        id: String,
    },
    CreateInvitation,
    RevokeDevice {
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_native_edit_keeps_dictation_appended_after_its_base() {
        assert_eq!(
            merge_draft_text(
                "hello".into(),
                "hello there".into(),
                "hello\ntranscript".into()
            ),
            "hello there\ntranscript"
        );
        assert_eq!(
            merge_draft_text("hello".into(), "".into(), "hello".into()),
            ""
        );
    }

    #[test]
    fn restored_text_joins_the_draft_once() {
        assert_eq!(merge_restored_text("", "sent"), "sent");
        assert_eq!(merge_restored_text("typed", ""), "typed");
        assert_eq!(merge_restored_text("typed", "sent"), "typed\n\nsent");
        assert_eq!(
            merge_restored_text("typed\n\nsent", "sent"),
            "typed\n\nsent"
        );
        assert_eq!(merge_restored_text("sent", "sent"), "sent");
    }

    #[test]
    fn a_draft_selection_needs_a_model_and_carries_its_options() {
        let mut draft = Draft::default();
        assert!(draft.selection().is_err());
        draft.instance_id = "codex".into();
        draft.model = "gpt".into();
        draft.options.push(ModelOption {
            key: "reasoningEffort".into(),
            value: "high".into(),
        });
        let selection = draft.selection().unwrap();
        assert_eq!(selection.options["reasoningEffort"], "high");
        assert_eq!(
            Draft::default().with_selection(&selection),
            Draft {
                attachments: vec![],
                text: String::new(),
                ..draft
            }
        );
    }

    #[test]
    fn attachments_send_only_after_their_upload() {
        let mut attachment = DraftAttachment {
            id: "local".into(),
            remote_id: None,
            name: "a.png".into(),
            mime_type: "image/png".into(),
            kind: "image".into(),
            size_bytes: 4,
            local_path: "/tmp/a.png".into(),
            status: "uploading".into(),
            error: None,
        };
        assert!(attachment.reference().is_err());
        attachment.status = "ready".into();
        attachment.remote_id = Some("pending:1".into());
        let reference = attachment.reference().unwrap();
        assert_eq!(reference.id, "pending:1");
        assert_eq!(reference.kind, AttachmentKind::Image);
        assert_eq!(
            DraftAttachment::from_remote(&reference)
                .remote_id
                .as_deref(),
            Some("pending:1")
        );
    }
}
