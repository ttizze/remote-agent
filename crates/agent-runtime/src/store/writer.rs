use super::{Store, StoredFact, ThreadHead, head, native_session_owner};
use crate::{EffectStatus, SearchChanges, ShellRow, StoreError};
use agent_domain::{Effect, EffectBody, Fact, FactBody, Receipt, ThreadId, Timestamp};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tokio::sync::oneshot;

pub(super) type Job = Box<dyn FnOnce(&mut Connection, &[Arc<dyn CommitListener>]) + Send>;

/// Everything one actor step persists, written in one `BEGIN IMMEDIATE` transaction.
#[derive(Debug, Clone)]
pub struct CommitBatch {
    pub thread: ThreadId,
    pub at: Timestamp,
    pub base_thread_seq: u64,
    pub input_seq: u64,
    pub receipt: Option<Receipt>,
    pub facts: Vec<Fact>,
    pub effects: Vec<Effect>,
    /// The claimed outbox row whose result this step consumed, and how it ends.
    pub settle: Option<EffectSettlement>,
    pub shell: Option<ShellRow>,
    pub needs_recovery: bool,
    pub search: SearchChanges,
    /// The projection after this batch, when a snapshot is due.
    pub snapshot: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct CommitOutcome {
    pub head: ThreadHead,
    pub facts: Arc<[StoredFact]>,
}

/// What a commit made durable, delivered to listeners in global order.
#[derive(Debug, Clone)]
pub struct CommitNotice {
    pub thread: ThreadId,
    pub head: ThreadHead,
    pub facts: Arc<[StoredFact]>,
    pub effects: Arc<[Effect]>,
    pub shell: Option<ShellRow>,
    pub settled: Option<String>,
}

/// Settles a claimed outbox row with the step that consumes its result. The commit
/// fails unless the row is still running under `worker`'s lease, so a cancelled or
/// reclaimed effect never contributes facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSettlement {
    pub effect_id: String,
    pub worker: String,
    pub settlement: Settlement,
}

/// How a claimed outbox row ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    Succeeded,
    Failed(String),
    Cancelled(String),
}
impl Settlement {
    pub fn status(&self) -> EffectStatus {
        match self {
            Self::Succeeded => EffectStatus::Succeeded,
            Self::Failed(_) => EffectStatus::Failed,
            Self::Cancelled(_) => EffectStatus::Cancelled,
        }
    }
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Succeeded => None,
            Self::Failed(error) | Self::Cancelled(error) => Some(error),
        }
    }
}

/// Runs on the writer thread after COMMIT and before the next transaction.
/// Implementations must not block; hand work to a channel instead.
pub trait CommitListener: Send + Sync {
    fn committed(&self, notice: &CommitNotice);
}

pub(super) fn spawn(
    mut connection: Connection,
    listeners: Arc<RwLock<Vec<Arc<dyn CommitListener>>>>,
) -> Result<std::sync::mpsc::Sender<Job>, StoreError> {
    let (sender, jobs) = std::sync::mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("runtime-store-writer".into())
        .spawn(move || {
            for job in jobs {
                let listeners = listeners.read().expect("listener list").clone();
                job(&mut connection, &listeners);
            }
        })
        .map_err(|error| StoreError::Corrupt(format!("cannot start the writer: {error}")))?;
    Ok(sender)
}

impl Store {
    async fn submit<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection, &[Arc<dyn CommitListener>]) -> Result<T, StoreError>
        + Send
        + 'static,
    ) -> Result<T, StoreError> {
        let (reply, result) = oneshot::channel();
        self.inner
            .writer
            .send(Box::new(move |connection, listeners| {
                let _ = reply.send(work(connection, listeners));
            }))
            .map_err(|_| StoreError::WriterStopped)?;
        result.await.map_err(|_| StoreError::WriterStopped)?
    }

    /// Commits one actor step, then notifies listeners before the writer takes the next job.
    pub async fn commit(&self, batch: CommitBatch) -> Result<CommitOutcome, StoreError> {
        self.submit(move |connection, listeners| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (notice, outcome) = write_batch(&transaction, batch)?;
            transaction.commit()?;
            for listener in listeners {
                listener.committed(&notice);
            }
            Ok(outcome)
        })
        .await
    }

    /// Host-owned writes outside actor steps (outbox claims, launches, workspaces).
    pub async fn write<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Transaction<'_>) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        self.submit(move |connection, _| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = work(&transaction)?;
            transaction.commit()?;
            Ok(value)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn on_writer(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<(), StoreError> {
        self.submit(move |connection, _| work(connection)).await
    }
}

fn variant(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(name) => name.clone(),
        serde_json::Value::Object(fields) => fields.keys().next().cloned().unwrap_or_default(),
        _ => String::new(),
    }
}
fn variant_of(value: &impl Serialize) -> Result<String, StoreError> {
    Ok(variant(&serde_json::to_value(value)?))
}
/// The outbox `kind` of an effect: the variant name, or `Provider.<command>`.
pub fn effect_kind(body: &EffectBody) -> Result<String, StoreError> {
    Ok(match body {
        EffectBody::Provider(command) => format!("Provider.{}", variant_of(command)?),
        other => variant_of(other)?,
    })
}

fn write_batch(
    tx: &Transaction<'_>,
    batch: CommitBatch,
) -> Result<(CommitNotice, CommitOutcome), StoreError> {
    let thread = batch.thread.as_str();
    let stored = head(tx, &batch.thread)?;
    if stored.thread_seq != batch.base_thread_seq {
        return Err(StoreError::Stale {
            thread: batch.thread,
            expected: batch.base_thread_seq,
            stored: stored.thread_seq,
        });
    }
    if let Some(receipt) = &batch.receipt {
        let owner: Option<String> = tx
            .query_row(
                "SELECT thread_id FROM receipts WHERE command_id = ?1",
                [receipt.command.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            return Err(StoreError::CommandConflict(
                ThreadId::new(owner).map_err(|error| StoreError::Corrupt(error.to_string()))?,
            ));
        }
    }
    // An import adopts a session that already exists outside the Host, so it must not
    // take one any thread has bound. The single writer makes this check atomic.
    if batch
        .facts
        .iter()
        .any(|fact| matches!(fact.body, FactBody::ThreadImported))
    {
        for fact in &batch.facts {
            if let FactBody::NativeSessionBound { native_thread, .. } = &fact.body
                && let Some(owner) = native_session_owner(tx, native_thread, &batch.thread)?
            {
                return Err(StoreError::NativeSessionOwned {
                    session: native_thread.clone(),
                    owner,
                });
            }
        }
    }
    let at_millis = batch.at.millis();
    let mut thread_seq = stored.thread_seq;
    let mut global_seq = stored.global_seq;
    let mut facts = Vec::with_capacity(batch.facts.len());
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO facts (thread_id, thread_seq, input_seq, kind, at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for fact in batch.facts {
            let body = serde_json::to_value(&fact.body)?;
            thread_seq += 1;
            insert.execute(params![
                thread,
                thread_seq as i64,
                batch.input_seq as i64,
                variant(&body),
                fact.at.as_str(),
                body.to_string(),
            ])?;
            global_seq = tx.last_insert_rowid() as u64;
            facts.push(StoredFact {
                global_seq,
                thread_seq,
                fact,
            });
        }
    }
    if let Some(receipt) = &batch.receipt {
        tx.execute(
            "INSERT INTO receipts
             (command_id, thread_id, fingerprint, reply, thread_seq, global_seq, accepted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                receipt.command.as_str(),
                thread,
                receipt.fingerprint,
                serde_json::to_string(&receipt.reply)?,
                thread_seq as i64,
                global_seq as i64,
                batch.at.as_str(),
            ],
        )?;
    }
    let head = ThreadHead {
        thread_seq,
        input_seq: batch.input_seq,
        global_seq,
    };
    tx.execute(
        "INSERT INTO threads (thread_id, thread_seq, input_seq, last_global_seq, needs_recovery)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (thread_id) DO UPDATE SET thread_seq = excluded.thread_seq,
             input_seq = excluded.input_seq, last_global_seq = excluded.last_global_seq,
             needs_recovery = excluded.needs_recovery",
        params![
            thread,
            head.thread_seq as i64,
            head.input_seq as i64,
            head.global_seq as i64,
            batch.needs_recovery,
        ],
    )?;
    {
        let mut insert = tx.prepare_cached(
            "INSERT INTO outbox (effect_id, thread_id, lane, kind, attempt_id, payload, status,
                 attempts, available_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', 0, ?7, ?7, ?7)",
        )?;
        for effect in &batch.effects {
            let lane = match effect.body {
                EffectBody::GenerateTitle { .. } => "title",
                _ => "main",
            };
            insert.execute(params![
                effect.id,
                thread,
                lane,
                effect_kind(&effect.body)?,
                effect.attempt.as_ref().map(|attempt| attempt.as_str()),
                serde_json::to_string(effect)?,
                at_millis,
            ])?;
        }
    }
    if let Some(settle) = &batch.settle {
        let changed = tx.execute(
            "UPDATE outbox SET status = ?4, last_error = ?5, completed_at = ?2, updated_at = ?2,
                 lease_owner = NULL, lease_expires_at = NULL
             WHERE effect_id = ?1 AND thread_id = ?3 AND status = 'running' AND lease_owner = ?6",
            params![
                settle.effect_id,
                at_millis,
                thread,
                settle.settlement.status().as_str(),
                settle.settlement.error(),
                settle.worker,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::NotLeased(settle.effect_id.clone()));
        }
    }
    if let Some(shell) = &batch.shell {
        tx.execute(
            "INSERT INTO thread_shells
                 (thread_id, global_seq, project, archived, deleted, needs_recovery, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (thread_id) DO UPDATE SET global_seq = excluded.global_seq,
                 project = excluded.project, archived = excluded.archived,
                 deleted = excluded.deleted, needs_recovery = excluded.needs_recovery,
                 payload = excluded.payload",
            params![
                thread,
                global_seq as i64,
                shell.project,
                shell.archived,
                shell.deleted,
                shell.needs_recovery,
                shell.payload.to_string(),
            ],
        )?;
    }
    if batch.search.cleared {
        tx.execute("DELETE FROM search_messages WHERE thread_id = ?1", [thread])?;
    }
    {
        let mut upsert = tx.prepare_cached(
            "INSERT INTO search_messages (thread_id, message_id, role, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (thread_id, message_id) DO UPDATE SET role = excluded.role,
                 text = excluded.text, created_at = excluded.created_at",
        )?;
        for row in &batch.search.upserts {
            upsert.execute(params![
                thread,
                row.message.as_str(),
                row.role,
                row.text,
                row.created_at
            ])?;
        }
    }
    if let Some(blob) = &batch.snapshot {
        tx.execute(
            "INSERT INTO thread_snapshots (thread_id, thread_seq, format, blob)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (thread_id) DO UPDATE SET thread_seq = excluded.thread_seq,
                 format = excluded.format, blob = excluded.blob",
            params![thread, thread_seq as i64, super::SNAPSHOT_FORMAT, blob],
        )?;
    }
    let facts: Arc<[StoredFact]> = facts.into();
    Ok((
        CommitNotice {
            thread: batch.thread,
            head,
            facts: facts.clone(),
            effects: batch.effects.into(),
            shell: batch.shell,
            settled: batch.settle.map(|settle| settle.effect_id),
        },
        CommitOutcome { head, facts },
    ))
}
