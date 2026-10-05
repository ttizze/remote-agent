use agent_domain::{FoldError, ThreadId};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("command id is already used by thread {0}")]
    CommandConflict(ThreadId),
    #[error("native session {session} is already bound to thread {owner}")]
    NativeSessionOwned { session: String, owner: ThreadId },
    #[error("thread {thread} expected sequence {expected} but the store has {stored}")]
    Stale {
        thread: ThreadId,
        expected: u64,
        stored: u64,
    },
    #[error("unsupported schema version {0}")]
    Schema(String),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
    #[error("effect {0} is not running under this worker's lease")]
    NotLeased(String),
    #[error("the writer has stopped")]
    WriterStopped,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Store(Arc<StoreError>),
    #[error("fold: {0}")]
    Fold(#[from] FoldError),
    #[error("the thread actor has stopped")]
    ActorStopped,
    #[error("{0} is not accepted through this entry point")]
    InvalidInput(&'static str),
}
impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::Store(Arc::new(error))
    }
}
