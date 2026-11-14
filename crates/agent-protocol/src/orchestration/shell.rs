//! T3 1a12c4af's compact shell contract. Transcript bodies belong to the thread
//! stream and full-item queries, independently of sidebar hydration.
use super::*;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    #[serde(flatten)]
    pub thread: Thread,
    pub latest_run_id: Option<String>,
    pub latest_run_requested_at: Option<String>,
    pub latest_run_started_at: Option<String>,
    pub latest_run_completed_at: Option<String>,
    #[serde(default)]
    pub latest_run_stop_requested: bool,
    pub active_run_id: Option<String>,
    pub activity_run_started_at: Option<String>,
    pub activity_run_status: Option<RunStatus>,
    #[serde(default)]
    pub blocked_on_thread_wait: bool,
    pub status: ShellStatus,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub last_error_code: Option<String>,
    pub usage_limit_reset_at: Option<String>,
    pub pending_runtime_request: Option<PendingRequestSummary>,
    pub latest_visible_message: Option<MessageSummary>,
    pub latest_user_message_at: Option<String>,
    pub latest_user_authored_message_at: Option<String>,
    pub has_actionable_proposed_plan: bool,
    #[serde(default)]
    pub pending_background_tasks: Vec<PendingBackgroundTask>,
    #[serde(default)]
    pub provider_instance_history: Vec<String>,
    pub item_count: u64,
    pub visible_item_count: u64,
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
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    pub archived_threads: Vec<ThreadShell>,
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
        project: ProjectShell,
    },
    #[serde(rename = "project.removed")]
    ProjectRemoved { sequence: u64, project_id: String },
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        sequence: u64,
        location: ShellLocation,
        thread: ThreadShell,
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
