use super::{Durability, EffectHandlers, OUTBOX_COLUMNS, OutboxRow, RawOutboxRow};
use crate::{Clock, CommitListener, CommitNotice, Settlement, Store, StoreError};
use agent_domain::ThreadId;
use futures_util::future::BoxFuture;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// The outbox operations a worker uses. Settling calls only change a row that is
/// still running under the caller's lease and return whether they did.
pub trait OutboxQueue: Send + Sync {
    /// Claims the earliest available row whose thread lane has no earlier open row.
    fn claim(
        &self,
        worker: &str,
        lease: Duration,
    ) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>>;
    fn get(&self, effect_id: &str) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>>;
    fn settle(
        &self,
        effect_id: &str,
        worker: &str,
        settlement: Settlement,
    ) -> BoxFuture<'_, Result<bool, StoreError>>;
    fn retry(
        &self,
        effect_id: &str,
        worker: &str,
        error: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<bool, StoreError>>;
    /// The earliest `available_at` among rows that only wait for time.
    fn next_claimable_at(&self) -> BoxFuture<'_, Result<Option<i64>, StoreError>>;
    /// The process-local signal that fires when the row is cancelled while claimed.
    fn cancellation(&self, effect_id: &str) -> CancellationToken;
    fn clear_cancellation(&self, effect_id: &str);
    /// Changes whenever new or unblocked work may be claimable.
    fn wakes(&self) -> watch::Receiver<u64>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub cancelled: usize,
    pub requeued: usize,
}

/// Pending rows with no running or earlier pending row in the same thread lane.
/// An earlier row waiting out a retry backoff therefore still blocks later ones.
const UNBLOCKED: &str = "candidate.status = 'pending' AND NOT EXISTS (
    SELECT 1 FROM outbox AS active
    WHERE active.thread_id = candidate.thread_id AND active.lane = candidate.lane
        AND (active.status = 'running'
            OR (active.status = 'pending' AND active.rowid < candidate.rowid)))";

/// The outbox table of a `Store`. Leases are never reclaimed while this process
/// lives; rows a previous process left behind are handled by
/// `reconcile_after_process_loss` before workers start.
pub struct SqliteOutbox {
    store: Store,
    clock: Arc<dyn Clock>,
    signals: Mutex<HashMap<String, CancellationToken>>,
    wake: Arc<Wake>,
}

struct Wake(watch::Sender<u64>);
impl Wake {
    fn notify(&self) {
        self.0
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}
impl CommitListener for Wake {
    fn committed(&self, notice: &CommitNotice) {
        if !notice.effects.is_empty() || notice.settled.is_some() {
            self.notify();
        }
    }
}

impl SqliteOutbox {
    /// Wakes claimers after every actor commit that adds or settles a row.
    pub fn new(store: Store, clock: Arc<dyn Clock>) -> Arc<Self> {
        let wake = Arc::new(Wake(watch::Sender::new(0)));
        store.add_listener(wake.clone());
        Arc::new(Self {
            store,
            clock,
            signals: Mutex::new(HashMap::new()),
            wake,
        })
    }

    pub fn notify(&self) {
        self.wake.notify();
    }

    fn now(&self) -> i64 {
        self.clock.now().millis()
    }

    fn released(&self, effect_id: &str) {
        self.signals.lock().expect("signals").remove(effect_id);
        self.notify();
    }

    /// Cancels the thread's unsettled rows of `kinds` and stops their running
    /// executions. Returns the cancelled effect ids.
    pub async fn cancel(
        &self,
        thread: &ThreadId,
        kinds: &[&str],
        reason: &str,
    ) -> Result<Vec<String>, StoreError> {
        let (thread, kinds, reason) = (
            thread.to_string(),
            serde_json::to_string(kinds)?,
            reason.to_string(),
        );
        let now = self.now();
        let cancelled = self
            .store
            .write(move |tx| {
                let mut statement = tx.prepare(
                    "UPDATE outbox SET status = 'cancelled', lease_owner = NULL,
                         lease_expires_at = NULL, completed_at = ?4, updated_at = ?4,
                         last_error = ?3
                     WHERE thread_id = ?1 AND status IN ('pending', 'running')
                         AND kind IN (SELECT value FROM json_each(?2))
                     RETURNING effect_id",
                )?;
                let ids = statement.query_map(params![thread, kinds, reason, now], |row| {
                    row.get::<_, String>(0)
                })?;
                Ok(ids.collect::<Result<Vec<_>, _>>()?)
            })
            .await?;
        if !cancelled.is_empty() {
            let signals = self.signals.lock().expect("signals");
            for id in &cancelled {
                if let Some(signal) = signals.get(id) {
                    signal.cancel();
                }
            }
            drop(signals);
            self.notify();
        }
        Ok(cancelled)
    }

    /// Run once at startup, before any worker claims: rows of process-bound or
    /// unregistered kinds are cancelled, and replay-safe rows left running are
    /// requeued without resetting their attempt count.
    pub async fn reconcile_after_process_loss(
        &self,
        handlers: &EffectHandlers,
    ) -> Result<Reconciled, StoreError> {
        let open: Vec<(String, String, String)> = self
            .store
            .blocking(|store| {
                store.read(|c| {
                    let mut statement = c.prepare(
                        "SELECT effect_id, kind, status FROM outbox
                         WHERE status IN ('pending', 'running')",
                    )?;
                    let rows = statement
                        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                    Ok(rows.collect::<Result<Vec<_>, _>>()?)
                })
            })
            .await?;
        let mut cancel = Vec::new();
        let mut requeue = Vec::new();
        for (id, kind, status) in open {
            match handlers.durability(&kind) {
                Some(Durability::ReplaySafe) if status == "running" => requeue.push(id),
                Some(Durability::ReplaySafe) => {}
                Some(Durability::ProcessBound) | None => cancel.push(id),
            }
        }
        let now = self.now();
        let reconciled = self
            .store
            .write(move |tx| {
                let mut reconciled = Reconciled::default();
                for id in cancel {
                    reconciled.cancelled += tx.execute(
                        "UPDATE outbox SET status = 'cancelled', lease_owner = NULL,
                             lease_expires_at = NULL, completed_at = ?2, updated_at = ?2,
                             last_error = 'Cancelled because the server process ended before the effect completed.'
                         WHERE effect_id = ?1 AND status IN ('pending', 'running')",
                        params![id, now],
                    )?;
                }
                for id in requeue {
                    reconciled.requeued += tx.execute(
                        "UPDATE outbox SET status = 'pending', lease_owner = NULL,
                             lease_expires_at = NULL, available_at = ?2, updated_at = ?2,
                             last_error = 'Requeued after the previous server process ended.'
                         WHERE effect_id = ?1 AND status = 'running'",
                        params![id, now],
                    )?;
                }
                Ok(reconciled)
            })
            .await?;
        if reconciled.requeued > 0 {
            self.notify();
        }
        Ok(reconciled)
    }
}

impl OutboxQueue for SqliteOutbox {
    fn claim(
        &self,
        worker: &str,
        lease: Duration,
    ) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>> {
        let worker = worker.to_string();
        Box::pin(async move {
            let now = self.now();
            let expires = now + (lease.as_millis() as i64).max(1);
            let claimed = self
                .store
                .write(move |tx| {
                    tx.prepare_cached(&format!(
                        "UPDATE outbox SET status = 'running', attempts = attempts + 1,
                             lease_owner = ?1, lease_expires_at = ?2, updated_at = ?3,
                             last_error = NULL
                         WHERE effect_id = (
                             SELECT candidate.effect_id FROM outbox AS candidate
                             WHERE candidate.available_at <= ?3 AND {UNBLOCKED}
                             ORDER BY candidate.available_at, candidate.rowid LIMIT 1)
                         RETURNING {OUTBOX_COLUMNS}"
                    ))?
                    .query_row(params![worker, expires, now], RawOutboxRow::read)
                    .optional()?
                    .map(RawOutboxRow::decode)
                    .transpose()
                })
                .await?;
            if let Some(row) = &claimed {
                self.signals
                    .lock()
                    .expect("signals")
                    .insert(row.effect.id.clone(), CancellationToken::new());
            }
            Ok(claimed)
        })
    }

    fn get(&self, effect_id: &str) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>> {
        let effect_id = effect_id.to_string();
        Box::pin(self.store.blocking(move |store| {
            store.read(|c| {
                c.prepare_cached(&format!(
                    "SELECT {OUTBOX_COLUMNS} FROM outbox WHERE effect_id = ?1"
                ))?
                .query_row([effect_id], RawOutboxRow::read)
                .optional()?
                .map(RawOutboxRow::decode)
                .transpose()
            })
        }))
    }

    fn settle(
        &self,
        effect_id: &str,
        worker: &str,
        settlement: Settlement,
    ) -> BoxFuture<'_, Result<bool, StoreError>> {
        let (effect_id, worker) = (effect_id.to_string(), worker.to_string());
        Box::pin(async move {
            let now = self.now();
            let id = effect_id.clone();
            let changed = self
                .store
                .write(move |tx| {
                    Ok(tx.execute(
                        "UPDATE outbox SET status = ?3, last_error = ?4, completed_at = ?5,
                             updated_at = ?5, lease_owner = NULL, lease_expires_at = NULL
                         WHERE effect_id = ?1 AND status = 'running' AND lease_owner = ?2",
                        params![
                            id,
                            worker,
                            settlement.status().as_str(),
                            settlement.error(),
                            now
                        ],
                    )?)
                })
                .await?;
            if changed == 1 {
                self.released(&effect_id);
            }
            Ok(changed == 1)
        })
    }

    fn retry(
        &self,
        effect_id: &str,
        worker: &str,
        error: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<bool, StoreError>> {
        let (effect_id, worker, error) =
            (effect_id.to_string(), worker.to_string(), error.to_string());
        Box::pin(async move {
            let now = self.now();
            let available_at = now + delay.as_millis() as i64;
            let id = effect_id.clone();
            let changed = self
                .store
                .write(move |tx| {
                    Ok(tx.execute(
                        "UPDATE outbox SET status = 'pending', available_at = ?3,
                             lease_owner = NULL, lease_expires_at = NULL, updated_at = ?4,
                             last_error = ?5
                         WHERE effect_id = ?1 AND status = 'running' AND lease_owner = ?2",
                        params![id, worker, available_at, now, error],
                    )?)
                })
                .await?;
            if changed == 1 {
                self.released(&effect_id);
            }
            Ok(changed == 1)
        })
    }

    fn next_claimable_at(&self) -> BoxFuture<'_, Result<Option<i64>, StoreError>> {
        Box::pin(self.store.blocking(|store| {
            store.read(|c| {
                Ok(c.prepare_cached(&format!(
                    "SELECT MIN(candidate.available_at) FROM outbox AS candidate WHERE {UNBLOCKED}"
                ))?
                .query_row([], |row| row.get::<_, Option<i64>>(0))?)
            })
        }))
    }

    fn cancellation(&self, effect_id: &str) -> CancellationToken {
        self.signals
            .lock()
            .expect("signals")
            .entry(effect_id.to_string())
            .or_default()
            .clone()
    }

    fn clear_cancellation(&self, effect_id: &str) {
        self.signals.lock().expect("signals").remove(effect_id);
    }

    fn wakes(&self) -> watch::Receiver<u64> {
        self.wake.0.subscribe()
    }
}
