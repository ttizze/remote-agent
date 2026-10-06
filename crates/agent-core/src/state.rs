//! Device state published to native views. The Host owns conversation
//! decisions; this holds what the device folded, sent and is editing.
use crate::commands::build::FollowUpBehavior;
use crate::commands::outbox::Outbox;
use crate::sync::{ShellCache, ShellStatus, ThreadSync};
use agent_domain::{
    Attachment, AttachmentKind, CheckpointId, Driver, InteractionMode, ModelSelection, RunId,
    RuntimeMode, State, ThreadId, ThreadShell, WorktreeSetupSnapshot,
};
use agent_protocol::conversation::{SearchMatch, ShellLocation, ShellSnapshot};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

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
    pub models: Vec<crate::models::Model>,
    pub model_errors: BTreeMap<String, String>,
    pub workspace: Workspace,
    pub terminals: BTreeMap<String, Terminal>,
    pub accounts: Option<agent_protocol::operations::Accounts>,
    pub account_login: Option<agent_protocol::operations::AccountLogin>,
    pub host_status: Option<crate::models::HostStatus>,
    pub remote_hosts: Vec<crate::models::RemoteHost>,
    pub invitation: Option<crate::models::Invitation>,
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
        let workspace = self.selected_thread.as_ref().and_then(|id| {
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
    pub fn shell_location(&self, location: ShellLocation) -> Option<&ShellCache> {
        match location {
            ShellLocation::Active => Some(&self.shell),
            ShellLocation::Archived => self.archived.as_deref(),
        }
    }
    pub fn terminal_view(&self, handle: &str, after: u64) -> TerminalView {
        let terminal = self.terminals.get(handle);
        TerminalView {
            status: terminal.map(|t| match &t.phase {
                TerminalPhase::Starting => "Starting".into(),
                TerminalPhase::Running => "Running".into(),
                TerminalPhase::Suspended => "Waiting for reconnect".into(),
                TerminalPhase::Detached => "Detached".into(),
                TerminalPhase::Exited(code) => format!("Exited · {code}"),
                TerminalPhase::Failed(message) => message.clone(),
            }),
            loading: terminal.is_some_and(|t| t.phase == TerminalPhase::Starting),
            accepts_input: self.connected
                && terminal.is_some_and(|t| t.phase == TerminalPhase::Running),
            output: terminal
                .map(|t| {
                    t.output
                        .iter()
                        .filter(|o| o.sequence > after)
                        .map(|output| output.as_ref().clone())
                        .collect()
                })
                .unwrap_or_default(),
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
#[derive(Debug, Clone, PartialEq)]
pub struct Terminal {
    pub cwd: String,
    pub size: agent_protocol::operations::TerminalSize,
    pub phase: TerminalPhase,
    pub output: Shared<std::collections::VecDeque<Arc<TerminalOutput>>>,
    pub sequence: u64,
    pub output_bytes: usize,
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
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalView {
    pub status: Option<String>,
    pub loading: bool,
    pub accepts_input: bool,
    pub output: Vec<TerminalOutput>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QuestionAnswer {
    pub question_id: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadAction {
    Pin,
    Unpin,
    Settle,
    Unsettle,
    Snooze { until: String },
    Unsnooze,
    Rename { title: String },
    RegenerateTitle,
    MarkUnread,
    AutoSettle { enabled: bool },
    Archive,
    Unarchive,
    Delete,
    PinReorder { order_key: String },
    ActiveReorder { order_key: String },
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueAction {
    Resume,
    Cancel { run_id: String },
    Edit { run_id: String },
    SaveEdit,
    CancelEdit,
    Reorder { run_ids: Vec<String> },
    Steer { run_id: String },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Intent {
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
    ReorderPinned {
        thread_id: String,
        before_thread_id: Option<String>,
    },
    EditDraft {
        text: String,
        base_text: Option<String>,
    },
    AttachFile {
        path: String,
        name: String,
        mime_type: String,
        draft_key: String,
    },
    RetryAttachment {
        id: String,
    },
    RemoveAttachment {
        id: String,
    },
    /// `alternate` is the second send gesture (Mod+Enter, long press).
    Send {
        alternate: bool,
    },
    Stop,
    StopSessions,
    DiscardPending {
        command_id: String,
    },
    Fork {
        source_thread_id: String,
        run_id: String,
    },
    MergeBack,
    PlanFollowUp {
        new_thread: bool,
    },
    Rollback {
        checkpoint_id: String,
        restore_files: bool,
    },
    Thread {
        thread_id: String,
        action: ThreadAction,
    },
    Queue {
        action: QueueAction,
    },
    SetModel {
        instance_id: String,
        driver: Driver,
        model: String,
        options: Vec<ModelOption>,
    },
    SetRuntimeMode {
        mode: RuntimeMode,
    },
    SetInteractionMode {
        mode: InteractionMode,
    },
    RespondApproval {
        request_id: String,
        decision: String,
    },
    RespondQuestions {
        request_id: String,
        answers: Vec<QuestionAnswer>,
    },
    DismissInput {
        request_id: String,
    },
    LoadEarlier,
    LoadItemDetail {
        item_id: String,
    },
    CancelSetup,
    Refresh,
    Transcribe {
        draft_key: String,
        preparation: Option<String>,
        audio: Vec<u8>,
    },
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
    ReviewWorkspace {
        cwd: String,
    },
    ReadTurnDiff {
        from_run_ordinal: u64,
        to_run_ordinal: u64,
        ignore_whitespace: bool,
    },
    LoadWorktreeSettings,
    SaveWorktreeSettings {
        settings: crate::models::WorktreeSettings,
    },
    ListWorktrees,
    RemoveWorktree {
        path: String,
    },
    StartTerminal {
        handle: String,
        cwd: String,
        cols: u16,
        rows: u16,
    },
    ResizeTerminal {
        handle: String,
        cols: u16,
        rows: u16,
    },
    WriteTerminal {
        handle: String,
        data: Vec<u8>,
    },
    DetachTerminal {
        handle: String,
    },
    KillTerminal {
        handle: String,
    },
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
    RegisterProject {
        path: String,
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
