//! A work-log row as both layouts draw it: a provider failure, or a call
//! with its icon, label, tones and expanded detail.
use crate::view::work_log::ToolIcon;
use crate::view::work_log::item_detail::ToolCallLines;
use crate::view::work_log::tool_catalog::ToolLogo;
use agent_domain::{RunId, ThreadId, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkIcon {
    Agent,
    Alert,
    Browser,
    Computer,
    Check,
    Command,
    Edit,
    Eye,
    Globe,
    Search,
    Hammer,
    Lock,
    Message,
    Warning,
    Wrench,
    Zap,
}

/// A provider failure that ended a turn, drawn with its message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderFailureRow {
    pub summary: String,
    /// A usage limit, drawn as a warning without the message.
    pub warning: bool,
    pub message: String,
    pub reset_at: Option<Timestamp>,
    pub created_at: Timestamp,
    /// The run a Retry prepares again, while it still ends in this failure.
    pub retry_preparation: Option<RunId>,
    pub copy_text: String,
}

impl ProviderFailureRow {
    /// `reset_time` is the reset formatted for the reader's locale.
    pub fn label(&self, reset_time: Option<&str>) -> String {
        if !self.warning {
            return self.summary.clone();
        }
        match reset_time {
            Some(time) => format!("Usage limit reached. Retry after {time}."),
            None => "Usage limit reached.".into(),
        }
    }

    pub fn accessibility_label(&self, reset_time: Option<&str>) -> String {
        if self.warning {
            self.label(reset_time)
        } else {
            format!("{}: {}", self.summary, self.message)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkRowIcon {
    Brain,
    Logo(ToolLogo),
    Feed(WorkIcon),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkLabelTone {
    Default,
    Warning,
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkIconTone {
    Default,
    Warning,
    Destructive,
    /// A failed call's icon, muted.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkRowRole {
    None,
    Button,
    /// Opens the thread of the subagent a notification reports.
    Link,
}

/// One work-log call row and, when expanded, its detail.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkActivityRow {
    pub id: String,
    pub role: WorkRowRole,
    pub opens_thread: Option<ThreadId>,
    /// The desktop's trailing button that opens `opens_thread`.
    pub open_label: Option<String>,
    pub can_expand: bool,
    pub expanded: bool,
    /// Load the item's withheld detail; the row shows it once loaded.
    pub load_detail: bool,
    pub shimmer: bool,
    pub label: String,
    pub answer_preview: Option<String>,
    /// The answer preview reads as an answer, not as the question.
    pub answer_highlighted: bool,
    pub icon: WorkRowIcon,
    pub tool_icon: Option<ToolIcon>,
    pub icon_tone: WorkIconTone,
    pub label_tone: WorkLabelTone,
    pub failed: bool,
    /// A failure mark beside a branded tool icon.
    pub failure_mark: bool,
    pub accessibility_label: String,
    pub accessibility_hint: String,
    pub copy_text: String,
    pub detail: Option<WorkActivityDetail>,
}

/// The expanded panel of a call row.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkActivityDetail {
    pub reasoning: Option<String>,
    pub call: Option<ToolCallLines>,
    pub full_detail: Option<String>,
    pub output: Option<String>,
    pub failed_exit_code: Option<i64>,
    pub viewed_image_path: Option<String>,
    /// The answered questions with their answers and files.
    pub question_answer: Option<Vec<crate::view::work_log::user_input::AnswerHistoryQuestion>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkLogRow {
    ProviderFailure(ProviderFailureRow),
    Activity(Box<WorkActivityRow>),
}
