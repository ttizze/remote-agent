//! Device state. The Host owns conversation decisions and the domain log.
use orchestration::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Draft {
    pub text: String,
    pub instance_id: String,
    pub model: String,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub runtime_mode: String,
    pub interaction_mode: String,
}
impl Draft {
    pub fn selection(&self) -> Result<ModelSelection, String> {
        if self.model.trim().is_empty() {
            return Err("Select a model".into());
        }
        let mut options = BTreeMap::new();
        if let Some(effort) = &self.effort {
            options.insert(
                "reasoningEffort".into(),
                Json(serde_json::Value::String(effort.clone())),
            );
        }
        if let Some(tier) = &self.service_tier {
            options.insert(
                "serviceTier".into(),
                Json(serde_json::Value::String(tier.clone())),
            );
        }
        Ok(ModelSelection {
            instance_id: ProviderInstanceId::new(&self.instance_id).map_err(|e| e.to_string())?,
            model: self.model.clone(),
            options,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadCache {
    pub projection: Arc<ThreadProjection>,
    pub sequence: u64,
    pub history_cursor: Option<HistoryCursor>,
    pub has_more_history: bool,
    pub synchronized: bool,
    pub latest_local_turn_ordinal: Option<u64>,
    pub accessed_at: u64,
}

#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct Snapshot {
    pub revision: u64,
    pub connected: bool,
    pub host_name: Option<String>,
    pub error: Option<String>,
    pub shell: Option<Arc<ShellSnapshot>>,
    pub shell_synchronized: bool,
    pub threads: BTreeMap<ThreadId, ThreadCache>,
    pub selected_thread: Option<ThreadId>,
    pub selected_project: Option<String>,
    pub search: String,
    pub search_matches: Vec<SearchMatch>,
    pub observed_returns: BTreeMap<ThreadId, Timestamp>,
    pub drafts: BTreeMap<String, Draft>,
    pub default_draft: Draft,
    pub editing_run: Option<RunId>,
    pub models: Vec<crate::models::Model>,
    pub projects: Vec<crate::models::Project>,
    pub model_errors: BTreeMap<String, String>,
    pub pending_commands: Vec<Command>,
    pub pending_launches: Vec<agent_protocol::orchestration::LaunchThread>,
    pub workspace: Workspace,
    pub terminals: BTreeMap<String, Terminal>,
    pub accounts: Option<agent_protocol::operations::Accounts>,
    pub account_login: Option<agent_protocol::operations::AccountLogin>,
    pub host_status: Option<crate::models::HostStatus>,
    pub remote_hosts: Vec<crate::models::RemoteHost>,
    pub invitation: Option<crate::models::Invitation>,
}
impl Snapshot {
    pub fn draft_key(&self) -> String {
        self.selected_thread
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                format!(
                    "new:{}",
                    self.selected_project.as_deref().unwrap_or("bex:chats")
                )
            })
    }
    pub fn current_draft(&self) -> Draft {
        self.drafts
            .get(&self.draft_key())
            .cloned()
            .unwrap_or_else(|| {
                if let Some(thread) = self.selected_thread.as_ref().and_then(|id| {
                    self.shell
                        .as_ref()?
                        .threads
                        .iter()
                        .chain(&self.shell.as_ref()?.archived_threads)
                        .find(|s| &s.thread.id == id)
                }) {
                    Draft {
                        text: String::new(),
                        instance_id: thread.thread.provider_instance_id.to_string(),
                        model: thread.thread.model_selection.model.clone(),
                        effort: thread
                            .thread
                            .model_selection
                            .options
                            .get("reasoningEffort")
                            .and_then(|j| j.0.as_str())
                            .map(str::to_owned),
                        service_tier: thread
                            .thread
                            .model_selection
                            .options
                            .get("serviceTier")
                            .and_then(|j| j.0.as_str())
                            .map(str::to_owned),
                        runtime_mode: thread.thread.runtime_mode.as_str().into(),
                        interaction_mode: thread.thread.interaction_mode.as_str().into(),
                    }
                } else {
                    self.default_draft.clone()
                }
            })
    }
    pub fn projection(&self) -> Option<&ThreadProjection> {
        self.selected_thread
            .as_ref()
            .and_then(|id| self.threads.get(id))
            .map(|cache| cache.projection.as_ref())
    }
    pub fn cwd(&self) -> String {
        let thread = self.projection().map(|p| &p.thread).or_else(|| {
            self.selected_thread
                .as_ref()
                .and_then(|id| {
                    self.shell
                        .as_ref()?
                        .threads
                        .iter()
                        .chain(&self.shell.as_ref()?.archived_threads)
                        .find(|s| &s.thread.id == id)
                })
                .map(|s| &s.thread)
        });
        thread
            .and_then(|t| t.worktree_path.clone())
            .or_else(|| {
                let project = thread
                    .map(|t| t.project_id.as_str())
                    .or(self.selected_project.as_deref())
                    .unwrap_or("bex:chats");
                self.projects
                    .iter()
                    .find(|p| p.id == project)?
                    .roots
                    .first()
                    .map(|r| r.path.clone())
            })
            .unwrap_or_default()
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn turn_diff_options(&self) -> Vec<crate::presentation::diff::TurnDiffOption> {
        let Some(projection) = self.projection() else {
            return vec![];
        };
        let Some(scope) = projection
            .checkpoint_scopes
            .iter()
            .find(|scope| scope.kind == ScopeKind::RootRun)
        else {
            return vec![];
        };
        crate::presentation::diff::turn_diff_options(&projection.checkpoints, &scope.id)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workspace {
    pub directory: Option<crate::models::FileList>,
    pub file: Option<crate::models::FileContent>,
    pub file_drafts: BTreeMap<String, FileDraft>,
    pub review: Option<crate::models::WorkspaceReview>,
    pub diff_request: Option<agent_protocol::orchestration::GetTurnDiff>,
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
    pub output: Vec<TerminalOutput>,
    pub sequence: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SendBehavior {
    Default,
    Steer,
    Restart,
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
    Search {
        query: String,
    },
    FilterProject {
        project_id: Option<String>,
    },
    MovePinned {
        thread_id: String,
        up: bool,
    },
    ReorderPinned {
        thread_id: String,
        before_thread_id: Option<String>,
    },
    EditDraft {
        draft: Draft,
    },
    Send {
        behavior: SendBehavior,
    },
    Stop,
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
        model: String,
        effort: Option<String>,
        service_tier: Option<String>,
    },
    SetRuntimeMode {
        mode: String,
    },
    SetInteractionMode {
        mode: String,
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
    LoadHistory,
    LoadItem {
        item_id: String,
    },
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
        from_turn_count: u64,
        to_turn_count: u64,
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
