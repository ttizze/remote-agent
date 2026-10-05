//! The provider I/O boundary has no persistence dependency.
use crate::contracts::*;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AdapterError {
    pub message: String,
    pub retryable: bool,
    pub turn_completed: bool,
}
#[async_trait::async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Start effects return once provider ownership is established. Long-lived
    /// notification pumps ingest full records using Store::ingest's run guard.
    /// Cancel an in-progress Start, including a process still initializing.
    async fn cancel_start(
        &self,
        _run: &RunId,
        _projection: &ThreadProjection,
    ) -> std::result::Result<(), AdapterError> {
        Ok(())
    }
    async fn execute(
        &self,
        effect: &Effect,
        projection: ThreadProjection,
    ) -> std::result::Result<Vec<DomainEvent>, AdapterError>;
}
