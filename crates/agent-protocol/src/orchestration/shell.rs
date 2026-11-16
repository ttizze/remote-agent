//! T3 1a12c4af's compact shell contract. Transcript bodies belong to the thread
//! stream and full-item queries, independently of sidebar hydration.
use super::*;
use std::sync::Arc;

/// Native imports retain their public provider-qualified addresses. The
/// canonical ID round trip proves that a prefix really names such an import.
pub fn public_session_ref(id: &str) -> Result<crate::session::SessionRef, String> {
    use crate::session::{ProviderKind, SessionRef};
    static PREFIXES: std::sync::LazyLock<Vec<(ProviderKind, String)>> =
        std::sync::LazyLock::new(|| {
            [
                ProviderKind::Codex,
                ProviderKind::Claude,
                ProviderKind::DeepSeek,
            ]
            .into_iter()
            .map(|provider| {
                let prefix = format!(
                    "{}:",
                    derived_id(
                        "thread",
                        &[
                            "provider",
                            provider.driver(),
                            "provider-instance",
                            provider.key(),
                            "native-thread"
                        ]
                    )
                );
                (provider, prefix)
            })
            .collect()
        });
    for (provider, prefix) in PREFIXES.iter() {
        if let Some(native) = id.strip_prefix(prefix)
            && let Ok(candidate) =
                SessionRef::new(*provider, crate::paths::decode_link_path(native))
            && candidate.application_id() == id
        {
            return Ok(candidate);
        }
    }
    SessionRef::new(None, id.to_owned()).map_err(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellStatus {
    Idle,
    Queued,
    Preparing,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
}

impl From<RunStatus> for ShellStatus {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Queued => Self::Queued,
            RunStatus::Preparing => Self::Preparing,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Completed => Self::Completed,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::RolledBack => Self::RolledBack,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestSummary {
    pub id: String,
    pub kind: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSummary {
    pub id: String,
    pub role: MessageRole,
    pub text: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Bex also observes real native turns without an application run. Their
/// receipt remains a provider-turn ID, never a fabricated application-run ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTurnSummary {
    pub id: String,
    pub status: ProviderTurnStatus,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    #[serde(flatten)]
    pub thread: Thread,
    pub latest_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_native_turn: Option<NativeTurnSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_requested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub latest_run_stop_requested: bool,
    pub active_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_run_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_run_status: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked_on_thread_wait: bool,
    pub status: ShellStatus,
    /// Bex observes native work created outside its application runs. This is
    /// the selected canonical provider thread's status, never a fabricated run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_status: Option<ProviderThreadStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_reset_at: Option<String>,
    pub pending_runtime_request: Option<PendingRequestSummary>,
    pub latest_visible_message: Option<MessageSummary>,
    pub latest_user_message_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_user_authored_message_at: Option<String>,
    pub has_actionable_proposed_plan: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_instance_history: Vec<String>,
    pub item_count: u64,
    pub visible_item_count: u64,
}

impl ThreadShell {
    pub fn task_state(&self) -> crate::live_activity::TaskState {
        use crate::execution::TurnStatus;
        let status = task_status(self.activity_run_status, self.native_status);
        let latest = self
            .newer_native_turn()
            .map(|turn| match turn.status {
                ProviderTurnStatus::Completed => Some(TurnStatus::Completed),
                ProviderTurnStatus::Failed => Some(TurnStatus::Failed),
                ProviderTurnStatus::Interrupted | ProviderTurnStatus::Cancelled => {
                    Some(TurnStatus::Interrupted)
                }
                _ => None,
            })
            .unwrap_or_else(|| match self.status {
                ShellStatus::Completed => Some(TurnStatus::Completed),
                ShellStatus::Failed => Some(TurnStatus::Failed),
                ShellStatus::Interrupted | ShellStatus::Cancelled => Some(TurnStatus::Interrupted),
                _ => None,
            });
        crate::live_activity::TaskState {
            status,
            waiting: self
                .pending_runtime_request
                .as_ref()
                .is_some_and(|request| request.blocking != Some(false)),
            approval: self
                .pending_runtime_request
                .as_ref()
                .is_some_and(|request| request.kind != "user_input"),
            latest,
            background: (!self.pending_background_tasks.is_empty()).then(|| {
                self.pending_background_tasks.iter().any(|task| {
                    task.awaited == Some(true)
                        || matches!(task.kind.as_str(), "subagent" | "background_task")
                })
            }),
        }
    }

    fn newer_native_turn(&self) -> Option<&NativeTurnSummary> {
        let native = self.latest_native_turn.as_ref()?;
        let Some(requested) = self.latest_run_requested_at.as_deref() else {
            return Some(native);
        };
        let observed = native
            .started_at
            .as_deref()
            .or(native.completed_at.as_deref())?;
        let observed = chrono::DateTime::parse_from_rfc3339(observed).ok()?;
        let requested = chrono::DateTime::parse_from_rfc3339(requested).ok()?;
        (observed > requested).then_some(native)
    }

    pub fn completion_receipt(&self) -> Option<(&str, &str)> {
        if let Some(native) = self.newer_native_turn() {
            return (native.status == ProviderTurnStatus::Completed)
                .then_some((native.id.as_str(), native.completed_at.as_deref()?));
        }
        (self.status == ShellStatus::Completed).then_some((
            self.latest_run_id.as_deref()?,
            self.latest_run_completed_at.as_deref()?,
        ))
    }

    pub fn unread(&self) -> bool {
        if self.task_state().session_status() != crate::execution::SessionStatus::Idle {
            return false;
        }
        let Some((_, completed)) = self.completion_receipt() else {
            return false;
        };
        let Some(completed) = chrono::DateTime::parse_from_rfc3339(completed).ok() else {
            return false;
        };
        self.thread
            .last_visited_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .is_none_or(|visited| visited < completed)
    }
}

/// A user fork is an independent conversation. Only delegated and side-chat
/// lineage belongs beneath another conversation in the shell.
pub fn is_listed_lineage(relationship: Option<&str>, side_chat: Option<bool>) -> bool {
    relationship != Some("subagent") && side_chat != Some(true)
}

pub fn activity<'a>(
    threads: impl IntoIterator<Item = &'a ThreadShell>,
) -> Result<crate::live_activity::TaskActivityState, String> {
    use crate::live_activity::{TaskActivityState, TaskActivitySummary};
    let mut state = TaskActivityState {
        display: TaskActivitySummary::default().display(),
        statuses: Vec::new(),
        waiting: Vec::new(),
        background: Vec::new(),
        unread: Vec::new(),
        attention: Vec::new(),
    };
    let mut phases = Vec::new();
    for shell in threads {
        if !is_listed_lineage(
            shell.thread.lineage.relationship_to_parent.as_deref(),
            shell.thread.lineage.side_chat,
        ) {
            continue;
        }
        let id = public_session_ref(&shell.thread.id)?;
        let facts = shell.task_state();
        phases.push(facts.phase().0);
        state.statuses.push((id.clone(), facts.session_status()));
        if facts.waiting {
            state.waiting.push(id.clone());
        }
        if let Some(background) = facts.background {
            state.background.push((id.clone(), background));
        }
        if let Some(attention) = facts.attention() {
            state.attention.push((id.clone(), attention));
        }
        if shell.unread()
            && let Some((receipt, _)) = shell.completion_receipt()
        {
            state.unread.push((id, receipt.to_owned().into()));
        }
    }
    let mut summary = TaskActivitySummary::from_statuses(phases);
    summary.finished = state.unread.len().try_into().unwrap_or(u32::MAX);
    state.display = summary.display();
    Ok(state)
}

pub fn task_status(
    activity: Option<RunStatus>,
    native: Option<ProviderThreadStatus>,
) -> crate::execution::SessionStatus {
    use crate::execution::SessionStatus;
    match activity {
        Some(RunStatus::Preparing | RunStatus::Starting) => SessionStatus::Preparing,
        Some(RunStatus::Running | RunStatus::Waiting) => SessionStatus::Running,
        _ => match native {
            Some(ProviderThreadStatus::Active) => SessionStatus::Running,
            Some(ProviderThreadStatus::Unknown) => SessionStatus::Unknown,
            _ => SessionStatus::Idle,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub has_custom_title: bool,
    pub workspace_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<Value>,
    pub default_model_selection: Option<ModelSelection>,
    pub default_thread_env_mode: Option<String>,
    #[serde(default)]
    pub auto_pull: bool,
    pub favicon_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_icon: Option<Value>,
    pub scripts: Vec<ProjectScript>,
    pub pinned_at: Option<String>,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellSnapshot {
    pub schema_version: u32,
    pub snapshot_sequence: u64,
    pub projects: Vec<Arc<ProjectShell>>,
    pub threads: Vec<Arc<ThreadShell>>,
    pub archived_threads: Vec<Arc<ThreadShell>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellLocation {
    Active,
    Archive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ShellStreamItem {
    #[serde(rename = "snapshot")]
    Snapshot {
        snapshot: ShellSnapshot,
        #[serde(skip_serializing_if = "Option::is_none")]
        resolved_repository_identity_roots: Option<Vec<String>>,
    },
    #[serde(rename = "project.updated")]
    ProjectUpdated {
        sequence: u64,
        project: Box<ProjectShell>,
    },
    #[serde(rename = "project.removed")]
    ProjectRemoved { sequence: u64, project_id: String },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: u64,
        location: ShellLocation,
        thread: Box<ThreadShell>,
    },
    #[serde(rename = "thread.removed")]
    ThreadRemoved {
        sequence: u64,
        location: ShellLocation,
        thread_id: String,
    },
    #[serde(rename = "synchronized")]
    Synchronized,
    #[serde(rename = "snapshot-required")]
    SnapshotRequired,
}

/// Validate independently of transport, storage and client state.
pub fn validate_snapshot(snapshot: &ShellSnapshot) -> Result<(), &'static str> {
    if snapshot.schema_version != 2 {
        return Err("unsupported shell schema version");
    }
    let mut threads = std::collections::BTreeSet::new();
    if snapshot
        .threads
        .iter()
        .chain(&snapshot.archived_threads)
        .any(|thread| thread.thread.id.is_empty() || !threads.insert(&thread.thread.id))
    {
        return Err("shell thread identity is empty or repeated");
    }
    let mut projects = std::collections::BTreeSet::new();
    if snapshot
        .projects
        .iter()
        .any(|project| project.id.is_empty() || !projects.insert(&project.id))
    {
        return Err("shell project identity is empty or repeated");
    }
    Ok(())
}

/// The same wire identity rules apply to bootstrap and live updates.
pub fn validate_item(item: &ShellStreamItem) -> Result<(), &'static str> {
    let id = match item {
        ShellStreamItem::Snapshot { snapshot, .. } => return validate_snapshot(snapshot),
        ShellStreamItem::ProjectUpdated { project, .. } => Some(project.id.as_str()),
        ShellStreamItem::ProjectRemoved { project_id, .. } => Some(project_id.as_str()),
        ShellStreamItem::ThreadUpdated { thread, .. } => Some(thread.thread.id.as_str()),
        ShellStreamItem::ThreadRemoved { thread_id, .. } => Some(thread_id.as_str()),
        _ => None,
    };
    if id.is_some_and(|id| id.trim().is_empty()) {
        return Err("shell update identity is empty");
    }
    Ok(())
}

mod display;
pub use display::{background_tasks, summary};
