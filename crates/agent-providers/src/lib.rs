//! Native protocol translation. Application identities belong to agent-domain.
mod claude;
mod claude_control;
mod codex;
mod stdio;
use agent_domain::*;
pub use claude::*;
pub use claude_control::*;
pub use codex::*;
use serde_json::Value;
pub use stdio::*;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Translation {
    pub events: Vec<ProviderEvent>,
    pub outbound: Vec<Value>,
}
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid native protocol frame: {0}")]
    Invalid(String),
    #[error("native provider rejected {operation}: {message}")]
    Remote {
        operation: String,
        message: String,
        turn_completed: bool,
    },
    #[error("native conversation boundary is missing: {0}")]
    MissingBoundary(String),
}
fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn required(value: &Value, key: &str) -> Result<String, ProtocolError> {
    let s = string(value, key);
    if s.is_empty() {
        Err(ProtocolError::Invalid(format!("missing {key}")))
    } else {
        Ok(s)
    }
}
fn optional(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn terminal(value: &str) -> RunStatus {
    match value {
        "completed" | "success" => RunStatus::Completed,
        "interrupted" => RunStatus::Interrupted,
        "cancelled" => RunStatus::Cancelled,
        _ => RunStatus::Failed,
    }
}
fn item_status(value: &str) -> ItemStatus {
    match value {
        "failed" | "error" => ItemStatus::Failed,
        "interrupted" => ItemStatus::Interrupted,
        "cancelled" => ItemStatus::Cancelled,
        _ => ItemStatus::Completed,
    }
}
fn questions(value: &Value) -> Vec<Question> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| Question {
            id: optional(q, "id").unwrap_or_else(|| string(q, "question")),
            header: string(q, "header"),
            question: string(q, "question"),
            multiple: q
                .get("multiSelect")
                .or_else(|| q.get("multi_select"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| QuestionOption {
                    label: string(o, "label"),
                    description: optional(o, "description"),
                })
                .collect(),
        })
        .collect()
}
#[cfg(test)]
mod replay;
#[cfg(test)]
mod tests;
