use super::{
    Durability, EffectError, EffectHandler, EffectHandlers, EffectJob, EffectStatus, OutboxQueue,
    OutboxRow,
};
use crate::{ActorRegistry, Clock, RuntimeError, Settlement, StoreError};
use agent_domain::EffectResult;
use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

#[derive(Debug, Clone, thiserror::Error)]
pub enum OutboxError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("{operation} of {effect}: the worker no longer owns the effect lease")]
    LostLease {
        operation: &'static str,
        effect: String,
    },
}
impl From<StoreError> for OutboxError {
    fn from(error: StoreError) -> Self {
        Self::Runtime(error.into())
    }
}

#[derive(Debug, Clone)]
pub struct WorkerOptions {
    pub worker_id: String,
    pub lease: Duration,
    pub max_attempts: u32,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            worker_id: format!("runtime:{}", std::process::id()),
            lease: Duration::from_secs(30),
            max_attempts: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub concurrency: usize,
    /// Upper bound on idle sleeps; commits and settlements wake workers sooner.
    pub liveness: Duration,
}
impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            concurrency: 4,
            liveness: Duration::from_secs(30),
        }
    }
}

/// `min(30 s, 100 ms * 2^(attempt - 1))`.
pub fn retry_delay(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(16);
    Duration::from_millis((100u64 << exponent).min(30_000))
}

/// Claims and runs outbox rows one at a time; `spawn` runs several in parallel.
pub struct EffectWorker {
    queue: Arc<dyn OutboxQueue>,
    handlers: EffectHandlers,
    threads: Arc<ActorRegistry>,
    clock: Arc<dyn Clock>,
    options: WorkerOptions,
}

enum Prepared {
    Cancelled,
    Missing,
    Skipped,
    Run(Arc<dyn EffectHandler>),
    Failed(Arc<dyn EffectHandler>, EffectError),
}

impl EffectWorker {
    pub fn new(
        queue: Arc<dyn OutboxQueue>,
        handlers: EffectHandlers,
        threads: Arc<ActorRegistry>,
        clock: Arc<dyn Clock>,
        options: WorkerOptions,
    ) -> Self {
        Self {
            queue,
            handlers,
            threads,
            clock,
            options,
        }
    }

    /// Runs rows until nothing is claimable or `max` rows were handled.
    pub async fn drain(&self, max: usize) -> Result<usize, OutboxError> {
        let mut handled = 0;
        while handled < max && self.run_once().await? {
            handled += 1;
        }
        Ok(handled)
    }

    /// Claims and settles one row. False when nothing was claimable.
    pub async fn run_once(&self) -> Result<bool, OutboxError> {
        let Some(row) = self
            .queue
            .claim(&self.options.worker_id, self.options.lease)
            .await?
        else {
            return Ok(false);
        };
        let id = row.effect.id.clone();
        // Armed before the durable re-read, so a cancellation that commits during
        // the read still wins against execution.
        let cancellation = self.queue.cancellation(&id);
        let prepared = match self.prepare(&row).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.queue.clear_cancellation(&id);
                self.requeue(&row, &error).await;
                return Err(error);
            }
        };
        let handler = match prepared {
            Prepared::Run(handler) => handler,
            other => {
                self.queue.clear_cancellation(&id);
                return match other {
                    Prepared::Failed(handler, error) => {
                        self.reschedule(&row, &*handler, error).await
                    }
                    Prepared::Missing => {
                        let error = format!("no effect handler is registered for {}", row.kind);
                        tracing::error!(effect = %id, kind = %row.kind, "{error}");
                        self.finish(&row, Settlement::Failed(error)).await
                    }
                    Prepared::Skipped => {
                        let reason = "Skipped because the effect no longer applies to its thread.";
                        self.finish(&row, Settlement::Cancelled(reason.into()))
                            .await
                    }
                    Prepared::Cancelled | Prepared::Run(_) => Ok(true),
                };
            }
        };
        let job = EffectJob {
            effect: row.effect.clone(),
            thread: row.thread.clone(),
            attempt: row.attempts,
            will_retry: row.attempts < self.options.max_attempts,
        };
        let execution = AssertUnwindSafe(handler.run(job)).catch_unwind();
        let outcome = tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            outcome = execution => Some(outcome),
        };
        self.queue.clear_cancellation(&id);
        let Some(outcome) = outcome else {
            return Ok(true);
        };
        match outcome.unwrap_or_else(|panic| Err(panicked(panic.as_ref()))) {
            Ok(result) => self.complete(&row, result).await,
            Err(error) => self.reschedule(&row, &*handler, error).await,
        }
    }

    async fn prepare(&self, row: &OutboxRow) -> Result<Prepared, OutboxError> {
        if self.cancelled(&row.effect.id).await? {
            return Ok(Prepared::Cancelled);
        }
        let Some(handler) = self.handlers.get(&row.kind) else {
            return Ok(Prepared::Missing);
        };
        if handler.durability() == Durability::ProcessBound {
            let state = self.threads.state(&row.thread).await?;
            match std::panic::catch_unwind(AssertUnwindSafe(|| {
                handler.should_run(&state, &row.effect)
            })) {
                Ok(true) => {}
                Ok(false) => return Ok(Prepared::Skipped),
                Err(panic) => {
                    return Ok(Prepared::Failed(handler.clone(), panicked(panic.as_ref())));
                }
            }
        }
        Ok(Prepared::Run(handler.clone()))
    }

    async fn cancelled(&self, effect_id: &str) -> Result<bool, OutboxError> {
        Ok(self
            .queue
            .get(effect_id)
            .await?
            .is_some_and(|row| row.status == EffectStatus::Cancelled))
    }

    /// Settles a row whose handler never started.
    async fn finish(&self, row: &OutboxRow, settlement: Settlement) -> Result<bool, OutboxError> {
        let outcome = async {
            let settled = self
                .queue
                .settle(&row.effect.id, &self.options.worker_id, settlement)
                .await?;
            self.settled(row, settled, "settle").await
        }
        .await;
        if let Err(error) = &outcome {
            self.requeue(row, error).await;
        }
        outcome
    }

    async fn complete(
        &self,
        row: &OutboxRow,
        result: Option<EffectResult>,
    ) -> Result<bool, OutboxError> {
        let outcome = async {
            let settled = match result {
                None => {
                    self.queue
                        .settle(
                            &row.effect.id,
                            &self.options.worker_id,
                            Settlement::Succeeded,
                        )
                        .await?
                }
                Some(result) => {
                    self.threads
                        .settle_effect(
                            &row.thread,
                            row.effect.id.clone(),
                            result,
                            Settlement::Succeeded,
                        )
                        .await?;
                    true
                }
            };
            self.settled(row, settled, "complete").await
        }
        .await;
        if let Err(error) = &outcome {
            if self.handlers.durability(&row.kind) == Some(Durability::ReplaySafe) {
                self.requeue(row, error).await;
            } else {
                self.terminalize(row, error).await;
            }
        }
        outcome
    }

    async fn reschedule(
        &self,
        row: &OutboxRow,
        handler: &dyn EffectHandler,
        error: EffectError,
    ) -> Result<bool, OutboxError> {
        let message = error.to_string();
        let last =
            matches!(error, EffectError::Permanent(_)) || row.attempts >= self.options.max_attempts;
        tracing::warn!(
            effect = %row.effect.id,
            kind = %row.kind,
            attempts = row.attempts,
            last,
            error = %message,
            "effect execution failed"
        );
        let settled = if last {
            let mapped = std::panic::catch_unwind(AssertUnwindSafe(|| {
                handler.failure(&row.effect, &message)
            }))
            .unwrap_or(None);
            let settlement = Settlement::Failed(message);
            let settled = match mapped {
                Some(result) => self
                    .threads
                    .settle_effect(&row.thread, row.effect.id.clone(), result, settlement)
                    .await
                    .map(|_| true)
                    .map_err(OutboxError::from),
                None => self
                    .queue
                    .settle(&row.effect.id, &self.options.worker_id, settlement)
                    .await
                    .map_err(OutboxError::from),
            };
            match settled {
                Ok(settled) => settled,
                Err(error) => {
                    self.terminalize(row, &error).await;
                    return Err(error);
                }
            }
        } else {
            match self
                .queue
                .retry(
                    &row.effect.id,
                    &self.options.worker_id,
                    &message,
                    retry_delay(row.attempts),
                )
                .await
            {
                Ok(settled) => settled,
                Err(error) => {
                    let error = OutboxError::from(error);
                    self.requeue(row, &error).await;
                    return Err(error);
                }
            }
        };
        self.settled(row, settled, "reschedule").await
    }

    /// A row this worker could not settle is fine only if someone cancelled it.
    async fn settled(
        &self,
        row: &OutboxRow,
        settled: bool,
        operation: &'static str,
    ) -> Result<bool, OutboxError> {
        if settled || self.cancelled(&row.effect.id).await? {
            Ok(true)
        } else {
            Err(OutboxError::LostLease {
                operation,
                effect: row.effect.id.clone(),
            })
        }
    }

    async fn requeue(&self, row: &OutboxRow, cause: &OutboxError) {
        let error = format!("Worker failed before settling the claimed effect: {cause}");
        match self
            .queue
            .retry(
                &row.effect.id,
                &self.options.worker_id,
                &error,
                Duration::ZERO,
            )
            .await
        {
            Ok(true) => tracing::warn!(effect = %row.effect.id, kind = %row.kind, %cause,
                "requeued an effect after an unexpected worker failure"),
            Ok(false) => tracing::warn!(effect = %row.effect.id, kind = %row.kind,
                "could not requeue an effect whose lease the worker lost"),
            Err(requeue) => tracing::error!(effect = %row.effect.id, kind = %row.kind, %requeue,
                "failed to requeue an effect after an unexpected worker failure"),
        }
    }

    async fn terminalize(&self, row: &OutboxRow, cause: &OutboxError) {
        let error = format!(
            "Worker failed to settle a process-bound effect after execution started: {cause}"
        );
        match self
            .queue
            .settle(
                &row.effect.id,
                &self.options.worker_id,
                Settlement::Failed(error),
            )
            .await
        {
            Ok(true) => tracing::error!(effect = %row.effect.id, kind = %row.kind, %cause,
                "failed an effect whose settlement failed after execution started"),
            Ok(false) => tracing::warn!(effect = %row.effect.id, kind = %row.kind,
                "could not fail an effect whose lease the worker lost"),
            Err(fail) => tracing::error!(effect = %row.effect.id, kind = %row.kind, %fail,
                "failed to fail an effect after its settlement failed"),
        }
    }

    /// Runs `concurrency` claim loops until the returned daemon is dropped.
    pub fn spawn(self: Arc<Self>, options: DaemonOptions) -> EffectDaemon {
        let mut slots = JoinSet::new();
        for _ in 0..options.concurrency.max(1) {
            slots.spawn(
                self.clone()
                    .slot(options.liveness.max(Duration::from_millis(1))),
            );
        }
        EffectDaemon { _slots: slots }
    }

    /// Commit notifications are the fast path; `available_at` schedules retries,
    /// and the liveness poll only covers a missed notification.
    async fn slot(self: Arc<Self>, liveness: Duration) {
        let mut wakes = self.queue.wakes();
        loop {
            wakes.borrow_and_update();
            match AssertUnwindSafe(self.run_once()).catch_unwind().await {
                Ok(Ok(true)) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Ok(false)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "effect worker failed");
                    tokio::time::sleep(liveness.min(Duration::from_secs(1))).await;
                    continue;
                }
                Err(panic) => {
                    tracing::error!(panic = %panic_text(panic.as_ref()), "effect worker panicked");
                    tokio::time::sleep(liveness.min(Duration::from_secs(1))).await;
                    continue;
                }
            }
            let next = match self.queue.next_claimable_at().await {
                Ok(next) => next,
                Err(error) => {
                    tracing::warn!(%error, "cannot read the next effect deadline");
                    None
                }
            };
            let delay = match next {
                None => liveness,
                Some(at) => {
                    let until = at - self.clock.now().millis();
                    liveness.min(Duration::from_millis(if until > 0 {
                        until as u64
                    } else {
                        25
                    }))
                }
            };
            tokio::select! {
                Ok(()) = wakes.changed() => {}
                () = tokio::time::sleep(delay) => {}
            }
        }
    }
}

/// Stops the claim loops when dropped.
pub struct EffectDaemon {
    _slots: JoinSet<()>,
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-text panic".into())
}

fn panicked(panic: &(dyn std::any::Any + Send)) -> EffectError {
    EffectError::Retryable(format!(
        "the effect handler panicked: {}",
        panic_text(panic)
    ))
}
