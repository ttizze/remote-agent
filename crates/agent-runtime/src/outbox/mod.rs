//! Durable effect execution: rows committed by actor steps are claimed per thread
//! lane, run by Host handlers, and settled through the owning actor.
mod queue;
mod worker;

pub use queue::*;
pub use worker::*;

use crate::StoreError;
use agent_domain::{Effect, EffectResult, State, ThreadId};
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}
impl EffectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// One outbox row. Times are Unix milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub effect: Effect,
    pub thread: ThreadId,
    pub lane: String,
    pub kind: String,
    pub status: EffectStatus,
    pub attempts: u32,
    pub available_at: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub last_error: Option<String>,
}

pub(crate) const OUTBOX_COLUMNS: &str = "payload, thread_id, lane, kind, status, attempts, \
     available_at, lease_owner, lease_expires_at, created_at, updated_at, completed_at, last_error";

pub(crate) struct RawOutboxRow {
    payload: String,
    thread: String,
    lane: String,
    kind: String,
    status: String,
    attempts: i64,
    available_at: i64,
    lease_owner: Option<String>,
    lease_expires_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
    last_error: Option<String>,
}
impl RawOutboxRow {
    /// Reads the columns of `OUTBOX_COLUMNS`, in order.
    pub(crate) fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            payload: row.get(0)?,
            thread: row.get(1)?,
            lane: row.get(2)?,
            kind: row.get(3)?,
            status: row.get(4)?,
            attempts: row.get(5)?,
            available_at: row.get(6)?,
            lease_owner: row.get(7)?,
            lease_expires_at: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
            completed_at: row.get(11)?,
            last_error: row.get(12)?,
        })
    }
    pub(crate) fn decode(self) -> Result<OutboxRow, StoreError> {
        Ok(OutboxRow {
            effect: serde_json::from_str(&self.payload)?,
            thread: crate::store::thread_id(self.thread)?,
            lane: self.lane,
            kind: self.kind,
            status: EffectStatus::parse(&self.status).ok_or_else(|| {
                StoreError::Corrupt(format!("unknown outbox status {}", self.status))
            })?,
            attempts: self.attempts as u32,
            available_at: self.available_at,
            lease_owner: self.lease_owner,
            lease_expires_at: self.lease_expires_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
            completed_at: self.completed_at,
            last_error: self.last_error,
        })
    }
}

/// What a Host restart does with an unsettled row of a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Tied to a provider process that no longer exists after a restart: cancelled.
    ProcessBound,
    /// Safe to run again: a row left running is requeued.
    ReplaySafe,
}

/// One claimed execution of an effect.
#[derive(Debug, Clone)]
pub struct EffectJob {
    pub effect: Effect,
    pub thread: ThreadId,
    /// 1 for the first execution.
    pub attempt: u32,
    /// False on the last attempt the worker will make.
    pub will_retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectError {
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    Permanent(String),
}

/// Host execution of one effect kind.
pub trait EffectHandler: Send + Sync {
    fn durability(&self) -> Durability;

    /// Asked before a process-bound effect runs; false settles the row cancelled.
    fn should_run(&self, _state: &State, _effect: &Effect) -> bool {
        true
    }

    /// `Ok(Some(result))` is fed to the thread, which settles the row in the same commit.
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>>;

    /// The result fed to the thread when the effect fails for good.
    fn failure(&self, _effect: &Effect, _error: &str) -> Option<EffectResult> {
        None
    }
}

/// Handlers by outbox kind (see `effect_kind`). Rows of an unregistered kind fail.
#[derive(Default, Clone)]
pub struct EffectHandlers {
    handlers: HashMap<String, Arc<dyn EffectHandler>>,
}
impl EffectHandlers {
    pub fn with(mut self, kind: impl Into<String>, handler: Arc<dyn EffectHandler>) -> Self {
        self.handlers.insert(kind.into(), handler);
        self
    }
    pub fn get(&self, kind: &str) -> Option<&Arc<dyn EffectHandler>> {
        self.handlers.get(kind)
    }
    pub fn durability(&self, kind: &str) -> Option<Durability> {
        self.get(kind).map(|handler| handler.durability())
    }
}

#[cfg(test)]
mod tests;
