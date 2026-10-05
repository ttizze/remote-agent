//! Codex app-server and Claude stream-json adapters for orchestration-v2.
pub mod capabilities;
pub mod claude;
pub mod codex;
pub mod normalize;
use orchestration::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// The Host ingests these under the run/attempt ownership guard. Adapters do
/// not own the orchestration store or mutate client projections.
#[derive(Debug)]
pub struct ProviderBatch {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub attempt_id: RunAttemptId,
    pub events: Vec<DomainEvent>,
    pub occurred_at: Timestamp,
    pub acknowledged: Option<tokio::sync::oneshot::Sender<bool>>,
}
pub(crate) fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    Timestamp::from_millis(millis).expect("system time in RFC 3339 range")
}
pub(crate) fn error(message: impl std::fmt::Display) -> orchestration::worker::AdapterError {
    orchestration::worker::AdapterError {
        message: message.to_string(),
        retryable: false,
    }
}
