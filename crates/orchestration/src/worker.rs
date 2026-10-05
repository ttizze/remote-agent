//! Provider I/O is owned here, never in the command planner or projector.
use crate::{
    contracts::*,
    store::{ClaimedEffect, Store, StoreError},
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{sync::watch, task::JoinSet};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AdapterError {
    pub message: String,
    pub retryable: bool,
}
#[async_trait::async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Start effects return once provider ownership is established. Long-lived
    /// notification pumps ingest full records using Store::ingest's run guard.
    async fn execute(
        &self,
        effect: &Effect,
        projection: ThreadProjection,
    ) -> std::result::Result<Vec<DomainEvent>, AdapterError>;
}
pub struct EffectWorker {
    pub store: Arc<Store>,
    pub adapter: Arc<dyn ProviderAdapter>,
    pub owner: String,
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
impl EffectWorker {
    pub async fn run(
        self,
        mut shutdown: watch::Receiver<bool>,
    ) -> std::result::Result<(), StoreError> {
        let mut jobs = JoinSet::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() {break;} }
                result = jobs.join_next(), if !jobs.is_empty() => { if let Some(result) = result { result.map_err(|e| StoreError::InvalidEvent(e.to_string()))??; } }
                _ = tick.tick() => {
                    while jobs.len() < 4 {
                        let Some(claim) = self.store.claim_effect(&self.owner, now_ms())? else {break;};
                        let store = self.store.clone(); let adapter = self.adapter.clone();
                        jobs.spawn(async move { execute(store, adapter, claim).await });
                    }
                }
            }
        }
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
        Ok(())
    }
}
async fn execute(
    store: Arc<Store>,
    adapter: Arc<dyn ProviderAdapter>,
    claim: ClaimedEffect,
) -> std::result::Result<(), StoreError> {
    let projection = store.projection(&claim.effect.thread_id)?;
    let expected = claim
        .effect
        .body
        .run_id()
        .and_then(|id| projection.runs.iter().find(|run| run.id == *id))
        .map(|run| (run.id.clone(), run.active_attempt_id.clone()));
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;
    let operation = adapter.execute(&claim.effect, projection);
    tokio::pin!(operation);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            _ = heartbeat.tick() => {match store.renew_effect(&claim, now_ms()) { Ok(()) => {}, Err(StoreError::LeaseLost) => return Ok(()), Err(error) => return Err(error) }}
        }
    };
    let timestamp =
        Timestamp::from_millis(now_ms()).map_err(|e| StoreError::InvalidEvent(e.to_string()))?;
    match result {
        Ok(events) => {
            match store.renew_effect(&claim, now_ms()) {
                Ok(()) => {}
                Err(StoreError::LeaseLost) => return Ok(()),
                Err(error) => return Err(error),
            }
            if !events.is_empty() {
                let ingest = if matches!(claim.effect.body, EffectBody::CaptureCheckpoint { .. }) {
                    Store::ingest_checkpoint
                } else {
                    Store::ingest
                };
                ingest(
                    &store,
                    events,
                    expected
                        .as_ref()
                        .map(|(run, attempt)| (run, attempt.as_ref())),
                    &timestamp,
                )?;
            }
            match store.finish_effect(&claim, None, now_ms()) {
                Ok(_) | Err(StoreError::LeaseLost) => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) => {
            let mut failed_claim = claim.clone();
            if !error.retryable {
                failed_claim.attempt = 5;
            }
            let retry = match store.finish_effect(&failed_claim, Some(&error.message), now_ms()) {
                Ok(retry) => retry,
                Err(StoreError::LeaseLost) => return Ok(()),
                Err(error) => return Err(error),
            };
            if !retry && let Some((run_id, attempt_id)) = expected {
                let projection = store.projection(&claim.effect.thread_id)?;
                if let Some(run) = projection.runs.iter().find(|run| {
                    run.id == run_id
                        && run.status.is_blocking()
                        && run.active_attempt_id == attempt_id
                }) {
                    let events = crate::decider::failed_effect(
                        &projection,
                        run,
                        &claim.effect.id,
                        &error.message,
                        &timestamp,
                    )
                    .events;
                    store.ingest(events, Some((&run_id, attempt_id.as_ref())), &timestamp)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    struct Completion;
    #[async_trait::async_trait]
    impl ProviderAdapter for Completion {
        async fn execute(
            &self,
            effect: &Effect,
            projection: ThreadProjection,
        ) -> std::result::Result<Vec<DomainEvent>, AdapterError> {
            let mut run = projection
                .runs
                .iter()
                .find(|run| Some(&run.id) == effect.body.run_id())
                .unwrap()
                .clone();
            run.status = RunStatus::Completed;
            run.completed_at = Some(now());
            Ok(vec![DomainEvent {
                id: EventId::new("adapter-done").unwrap(),
                thread_id: projection.thread.id,
                occurred_at: now(),
                payload: EventPayload::RunUpdated(run),
            }])
        }
    }
    struct Failure;
    #[async_trait::async_trait]
    impl ProviderAdapter for Failure {
        async fn execute(
            &self,
            _: &Effect,
            _: ThreadProjection,
        ) -> std::result::Result<Vec<DomainEvent>, AdapterError> {
            Err(AdapterError {
                message: "provider unavailable".into(),
                retryable: false,
            })
        }
    }
    fn setup() -> Arc<Store> {
        let store = Arc::new(Store::memory().unwrap());
        store
            .dispatch(&create(), &now(), &turns(), Driver::Codex)
            .unwrap();
        store
            .dispatch(
                &send("one", DispatchMode::StartImmediately),
                &now(),
                &turns(),
                Driver::Codex,
            )
            .unwrap();
        store
    }
    #[tokio::test]
    async fn worker_commits_provider_records_and_completes_the_lease() {
        let store = setup();
        let claim = store.claim_effect("worker", now_ms()).unwrap().unwrap();
        execute(store.clone(), Arc::new(Completion), claim)
            .await
            .unwrap();
        assert_eq!(
            store.projection(&create().thread_id).unwrap().runs[0].status,
            RunStatus::Completed
        );
        assert!(store.claim_effect("worker", now_ms()).unwrap().is_none());
    }
    #[tokio::test]
    async fn permanent_provider_failure_terminalizes_the_run() {
        let store = setup();
        let claim = store.claim_effect("worker", now_ms()).unwrap().unwrap();
        execute(store.clone(), Arc::new(Failure), claim)
            .await
            .unwrap();
        let projection = store.projection(&create().thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Failed);
        assert_eq!(projection.attempts[0].status, AttemptStatus::Failed);
        assert_eq!(projection.nodes[0].status, NodeStatus::Failed);
        assert!(
            projection
                .turn_items
                .iter()
                .any(|item| matches!(item.body, TurnItemBody::Error { .. }))
        );
        assert!(store.claim_effect("worker", now_ms()).unwrap().is_none());
    }
}
