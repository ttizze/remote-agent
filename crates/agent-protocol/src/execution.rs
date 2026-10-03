//! Provider-neutral observations of execution and failure.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Running,
    Completed,
    Failed,
    Declined,
    Interrupted,
    #[default]
    Unknown,
}
impl TurnStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
}
impl ItemStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Declined => "declined",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCategory {
    RateLimited,
    UsageLimit,
    Overloaded,
    ContextLimit,
    SessionLimit,
    Auth,
    Network,
    Policy,
    InvalidInput,
    Sandbox,
    Rollback,
    InputUnavailable,
    Internal,
    Provider(#[serde(with = "crate::protocol::json")] serde_json::Value),
    #[default]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionError {
    pub category: ErrorCategory,
    pub message: String,
    pub details: Option<String>,
    pub provider_code: Option<String>,
    pub http_status: Option<u16>,
    pub resets_at_seconds: Option<u64>,
    pub retry: Option<RetryEvidence>,
    pub retry_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryEvidence {
    pub retrying: bool,
    pub overloaded: bool,
    pub attempt: Option<u32>,
    pub max_attempts: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Running,
    Idle,
    Unavailable,
    #[default]
    Unknown,
}
