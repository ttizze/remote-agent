//! Native protocol translation. Application identities belong to agent-domain.
mod attachments;
mod claude;
mod claude_control;
mod claude_fork;
mod claude_models;
mod codex;
mod codex_tools;
mod elicitation;
mod skills;
mod stdio;
use agent_domain::*;
pub use attachments::*;
pub use claude::*;
pub use claude_control::*;
pub use claude_fork::*;
pub use claude_models::*;
pub use codex::*;
use codex_tools::*;
pub use elicitation::*;
use serde_json::Value;
pub use skills::*;
pub use stdio::*;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Translation {
    pub events: Vec<ProviderEvent>,
    pub outbound: Vec<Value>,
    pub process: Option<ProcessDirective>,
    pub replies: Vec<NativeReply>,
    /// A native operation that finished without a turn event.
    pub completion: Option<Completion>,
    /// The route (app thread) of a shared process the translation belongs to.
    pub route: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Completion {
    RolledBack { native_thread: String },
    Forked { native_thread: String },
}
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessDirective {
    Reset {
        native_thread: String,
    },
    Resume {
        native_thread: String,
        absolute_head: Option<String>,
    },
    Fork {
        native_thread: String,
        through_head: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub struct NativeReply {
    pub request: String,
    pub operation: String,
    pub result: Json,
}
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid native protocol frame: {0}")]
    Invalid(String),
    #[error("native provider rejected {operation}: {message}")]
    Remote {
        /// The rejected native request id; `None` when it was never sent.
        request: Option<String>,
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
fn native_path<'a>(
    route: &'a str,
    parents: &'a std::collections::BTreeMap<String, String>,
) -> Result<Vec<&'a str>, ProtocolError> {
    let mut path = vec![];
    let mut key = route;
    while !key.is_empty() {
        if path.contains(&key) {
            return Err(ProtocolError::Invalid("cyclic native child routing".into()));
        }
        path.push(key);
        key = parents.get(key).map(String::as_str).unwrap_or("");
    }
    Ok(path)
}
fn child_events(
    events: Vec<ProviderEvent>,
    route: &str,
    parents: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<ProviderEvent>, ProtocolError> {
    let path = native_path(route, parents)?;
    Ok(events
        .into_iter()
        .map(|mut event| {
            for key in &path {
                event = ProviderEvent::Child {
                    key: (*key).into(),
                    event: Box::new(event),
                };
            }
            event
        })
        .collect())
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
        "cancelled" | "declined" => ItemStatus::Cancelled,
        "inProgress" => ItemStatus::Running,
        _ => ItemStatus::Completed,
    }
}
fn questions(value: &Value) -> Vec<Question> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| Question {
            required: q.get("required").and_then(Value::as_bool).unwrap_or(true),
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
#[cfg(any(test, feature = "test-support"))]
pub mod replay_support;
#[cfg(test)]
mod tests;
