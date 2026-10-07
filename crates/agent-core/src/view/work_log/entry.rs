//! The work-log entry every timeline layout presents: one tool call, thought,
//! notice or diagnostic, with the item it came from.
use agent_domain::{
    Answer, Attachment, Item, ItemKind, ItemStatus, RunId, RuntimeRequestId, Timestamp,
};
use std::sync::Arc;

/// The item kinds as the work log names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    Fork,
    UserMessage,
    AssistantMessage,
    Reasoning,
    RunInterruptRequest,
    RunInterruptResult,
    CommandExecution,
    FileChange,
    DynamicTool,
    WebSearch,
    ProposedPlan,
    TodoList,
    ApprovalRequest,
    UserInputRequest,
    Subagent,
    Compaction,
    Error,
    SystemNotice,
    Notification,
    ThreadCreated,
}

impl ItemType {
    pub fn of(kind: &ItemKind) -> Self {
        match kind {
            ItemKind::Fork { .. } => Self::Fork,
            ItemKind::UserMessage { .. } => Self::UserMessage,
            ItemKind::AssistantMessage { .. } => Self::AssistantMessage,
            ItemKind::Reasoning => Self::Reasoning,
            ItemKind::RunInterruptRequest => Self::RunInterruptRequest,
            ItemKind::RunInterruptResult { .. } => Self::RunInterruptResult,
            ItemKind::CommandExecution { .. } => Self::CommandExecution,
            ItemKind::FileChange { .. } => Self::FileChange,
            ItemKind::DynamicTool { .. } => Self::DynamicTool,
            ItemKind::WebSearch { .. } => Self::WebSearch,
            ItemKind::ProposedPlan { .. } => Self::ProposedPlan,
            ItemKind::TodoList { .. } => Self::TodoList,
            ItemKind::ApprovalRequest { .. } => Self::ApprovalRequest,
            ItemKind::UserInputRequest { .. } => Self::UserInputRequest,
            ItemKind::Subagent { .. } => Self::Subagent,
            ItemKind::Compaction { .. } => Self::Compaction,
            ItemKind::Error { .. } => Self::Error,
            ItemKind::SystemNotice { .. } => Self::SystemNotice,
            ItemKind::Notification { .. } => Self::Notification,
            ItemKind::ThreadCreated { .. } => Self::ThreadCreated,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fork => "fork",
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::Reasoning => "reasoning",
            Self::RunInterruptRequest => "run_interrupt_request",
            Self::RunInterruptResult => "run_interrupt_result",
            Self::CommandExecution => "command_execution",
            Self::FileChange => "file_change",
            Self::DynamicTool => "dynamic_tool",
            Self::WebSearch => "web_search",
            Self::ProposedPlan => "proposed_plan",
            Self::TodoList => "todo_list",
            Self::ApprovalRequest => "approval_request",
            Self::UserInputRequest => "user_input_request",
            Self::Subagent => "subagent",
            Self::Compaction => "compaction",
            Self::Error => "error",
            Self::SystemNotice => "system_notice",
            Self::Notification => "notification",
            Self::ThreadCreated => "thread_created",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ToolLifecycleStatus {
    Idle,
    InProgress,
    Completed,
    Failed,
    Declined,
    Stopped,
}

impl From<ItemStatus> for ToolLifecycleStatus {
    fn from(status: ItemStatus) -> Self {
        match status {
            ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting => Self::InProgress,
            ItemStatus::Completed => Self::Completed,
            ItemStatus::Failed => Self::Failed,
            ItemStatus::Interrupted | ItemStatus::Cancelled => Self::Stopped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkTone {
    Thinking,
    Tool,
    Info,
    Error,
}

/// What produced an entry that is not a plain tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceActivity {
    ContextCompaction,
    RuntimeWarning,
    RuntimeError,
    TaskProgress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ToolSurface {
    Browser,
    Computer,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum NativeApp {
    AppId(String),
    DisplayName(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ToolIcon {
    Website {
        page_url: String,
        favicon_url: Option<String>,
        favicon_url_dark: Option<String>,
    },
    NativeApp(NativeApp),
    ThemedLogo {
        logo_url: String,
        logo_url_dark: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ToolSourceKind {
    Browser,
    Computer,
    Integration,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ToolSource {
    pub key: String,
    pub name: String,
    pub kind: ToolSourceKind,
    pub icon: Option<ToolIcon>,
}

/// Answers given to a question request, in question order.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionAnswer {
    pub request: RuntimeRequestId,
    pub answers: Vec<(String, Answer)>,
    pub attachments: Vec<(String, Vec<Attachment>)>,
    pub question_text: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkLogEntry {
    pub id: String,
    pub created_at: Timestamp,
    pub run: Option<RunId>,
    pub label: String,
    pub detail: Option<String>,
    pub command: Option<String>,
    pub raw_command: Option<String>,
    pub changed_files: Option<Vec<String>>,
    pub tone: WorkTone,
    pub tool_title: Option<String>,
    pub viewed_image_path: Option<String>,
    pub tool_surface: Option<ToolSurface>,
    pub tool_icon: Option<ToolIcon>,
    pub tool_source: Option<ToolSource>,
    pub source_activity: Option<SourceActivity>,
    pub request_kind: Option<String>,
    pub item_type: Option<ItemType>,
    pub tool_lifecycle_status: Option<ToolLifecycleStatus>,
    /// The item the entry presents.
    pub item: Option<Arc<Item>>,
    pub question_answer: Option<QuestionAnswer>,
}

impl WorkLogEntry {
    pub fn new(
        id: impl Into<String>,
        created_at: Timestamp,
        label: impl Into<String>,
        tone: WorkTone,
    ) -> Self {
        Self {
            id: id.into(),
            created_at,
            run: None,
            label: label.into(),
            detail: None,
            command: None,
            raw_command: None,
            changed_files: None,
            tone,
            tool_title: None,
            viewed_image_path: None,
            tool_surface: None,
            tool_icon: None,
            tool_source: None,
            source_activity: None,
            request_kind: None,
            item_type: None,
            tool_lifecycle_status: None,
            item: None,
            question_answer: None,
        }
    }
}
