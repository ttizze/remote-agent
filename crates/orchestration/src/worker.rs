//! Provider I/O is owned here, never in the command planner or projector.
use crate::{
    ProviderAdapter,
    contracts::*,
    store::{ClaimedEffect, Store, StoreError},
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{sync::watch, task::JoinSet};

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
                result = jobs.join_next(), if !jobs.is_empty() => { if let Some(result) = result {
                    match result { Ok(Ok(())) => {}, Ok(Err(error)) => tracing::error!(operation="effect.job", message=%error), Err(error) => tracing::error!(operation="effect.job", message=%error) }
                } }
                _ = tick.tick() => {
                    while jobs.len() < 4 {
                        let claim = match self.store.claim_effect(&self.owner, now_ms()) {
                            Ok(Some(claim)) => claim,
                            Ok(None) => break,
                            Err(error) => { tracing::error!(operation="effect.claim", message=%error); break; }
                        };
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
    let mut commits = store.subscribe_commits();
    let projection = store.projection(&claim.effect.thread_id)?;
    let expected = claim
        .effect
        .body
        .run_id()
        .and_then(|id| projection.runs.iter().find(|run| run.id == *id))
        .map(|run| (run.id.clone(), run.active_attempt_id.clone()));
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(10));
    heartbeat.tick().await;
    let start = match &claim.effect.body {
        EffectBody::Start { run_id } => Some(run_id.clone()),
        _ => None,
    };
    let stop_requested = start.as_ref().is_some_and(|id| {
        projection.turn_items.iter().any(|item| {
            item.run_id.as_ref() == Some(id)
                && matches!(item.body, TurnItemBody::RunInterruptRequest { .. })
        })
    });
    if stop_requested {
        let run = start.as_ref().expect("start");
        adapter
            .cancel_start(run, &projection)
            .await
            .map_err(|error| StoreError::InvalidEvent(error.message))?;
        let decision = crate::decider::interrupted_start(
            &projection,
            run,
            &claim.effect.id,
            &Timestamp::from_millis(now_ms()).expect("current time"),
        );
        if !decision.events.is_empty() {
            store.ingest(
                decision.events,
                expected
                    .as_ref()
                    .map(|(id, attempt)| (id, attempt.as_ref())),
                &Timestamp::from_millis(now_ms()).expect("current time"),
            )?;
        }
    }
    let operation = adapter.execute(&claim.effect, projection.clone());
    let mut cancelled = stop_requested;
    tokio::pin!(operation);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            event = commits.recv(), if start.is_some() && !cancelled => {
                let requested = match event {
                    Ok(event) => event.event.thread_id == claim.effect.thread_id && match &event.event.payload {
                        EventPayload::TurnItemUpdated(item) => item.run_id.as_ref() == start.as_ref() && matches!(item.body, TurnItemBody::RunInterruptRequest { .. }),
                        EventPayload::RunUpdated(run) => Some(&run.id) == start.as_ref() && run.status.is_terminal(),
                        _ => false,
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let current = store.projection(&claim.effect.thread_id)?;
                        current.turn_items.iter().any(|item| item.run_id.as_ref() == start.as_ref() && matches!(item.body, TurnItemBody::RunInterruptRequest { .. })) || current.runs.iter().any(|run| Some(&run.id) == start.as_ref() && run.status.is_terminal())
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => false,
                };
                if requested {
                    let run = start.as_ref().expect("start");
                    adapter.cancel_start(run, &projection).await.map_err(|error| StoreError::InvalidEvent(error.message))?;
                    let current = store.projection(&claim.effect.thread_id)?;
                    let timestamp = Timestamp::from_millis(now_ms()).expect("current time");
                    let events = crate::decider::interrupted_start(&current, run, &claim.effect.id, &timestamp).events;
                    if !events.is_empty() { store.ingest(events, expected.as_ref().map(|(id, attempt)| (id, attempt.as_ref())), &timestamp)?; }
                    cancelled = true;
                }
            }
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
                if let EffectBody::Rollback { request_id, .. } = &claim.effect.body {
                    store.ingest_rollback(events, request_id, &timestamp)?;
                } else {
                    let ingest =
                        if matches!(claim.effect.body, EffectBody::CaptureCheckpoint { .. }) {
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
            }
            match store.finish_effect(&claim, None, now_ms()) {
                Ok(_) | Err(StoreError::LeaseLost) => {}
                Err(error) => return Err(error),
            }
        }
        Err(error) => {
            if cancelled {
                let _ = store.finish_effect(&claim, None, now_ms());
                return Ok(());
            }
            if error.turn_completed && matches!(claim.effect.body, EffectBody::Steer { .. }) {
                store.steer_follow_up(&claim.effect, &timestamp)?;
                store.finish_effect(&claim, None, now_ms())?;
                return Ok(());
            }
            let mut failed_claim = claim.clone();
            if !error.retryable {
                failed_claim.attempt = 5;
            }
            let retry = match store.finish_effect(&failed_claim, Some(&error.message), now_ms()) {
                Ok(retry) => retry,
                Err(StoreError::LeaseLost) => return Ok(()),
                Err(error) => return Err(error),
            };
            if !retry && let EffectBody::Rollback { request_id, .. } = &claim.effect.body {
                let mut thread = store.projection(&claim.effect.thread_id)?.thread;
                thread.rollback_request_id = None;
                thread.rollback_failure = Some(error.message.clone());
                thread.updated_at = timestamp.clone();
                store.ingest_rollback(
                    crate::events(
                        &claim.effect.thread_id,
                        &format!("rollback-failed:{request_id}"),
                        vec![EventPayload::ThreadMetadataUpdated(thread)],
                        &timestamp,
                    ),
                    request_id,
                    &timestamp,
                )?;
            }
            if !retry && let Some((run_id, attempt_id)) = expected {
                let projection = store.projection(&claim.effect.thread_id)?;
                if let Some(run) = projection.runs.iter().find(|run| {
                    run.id == run_id
                        && run.status.is_blocking()
                        && run.status != RunStatus::Waiting
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
    use crate::AdapterError;
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
                turn_completed: false,
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
    struct Initializing {
        started: tokio::sync::Notify,
        cancelled: std::sync::atomic::AtomicBool,
        changed: tokio::sync::Notify,
        inputs: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ProviderAdapter for Initializing {
        async fn execute(
            &self,
            _: &Effect,
            _: ThreadProjection,
        ) -> std::result::Result<Vec<DomainEvent>, AdapterError> {
            self.started.notify_one();
            self.changed.notified().await;
            if !self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                self.inputs
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(vec![])
        }
        async fn cancel_start(
            &self,
            _: &RunId,
            _: &ThreadProjection,
        ) -> std::result::Result<(), AdapterError> {
            self.cancelled
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.changed.notify_one();
            Ok(())
        }
    }
    #[tokio::test]
    async fn stopping_an_in_progress_start_cancels_provider_input_and_holds_the_queue() {
        let store = setup();
        store
            .dispatch(
                &send("queued", DispatchMode::QueueAfterActive),
                &now(),
                &turns(),
                Driver::Codex,
            )
            .unwrap();
        let adapter = Arc::new(Initializing {
            started: tokio::sync::Notify::new(),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
            inputs: std::sync::atomic::AtomicUsize::new(0),
        });
        let claim = store.claim_effect("test", now_ms()).unwrap().unwrap();
        let task = tokio::spawn(execute(store.clone(), adapter.clone(), claim));
        adapter.started.notified().await;
        let run = store.projection(&create().thread_id).unwrap().runs[0]
            .id
            .clone();
        store
            .dispatch(
                &command(
                    "stop-startup",
                    CommandBody::RunInterrupt {
                        run_id: run,
                        reason: None,
                        hold_queue: true,
                    },
                ),
                &now(),
                &turns(),
                Driver::Codex,
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(adapter.cancelled.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(adapter.inputs.load(std::sync::atomic::Ordering::SeqCst), 0);
        let projection = store.projection(&create().thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Interrupted);
        assert!(projection.runs[1].queue_held);
    }
    struct PanicOnce(std::sync::atomic::AtomicBool);
    #[async_trait::async_trait]
    impl ProviderAdapter for PanicOnce {
        async fn execute(
            &self,
            effect: &Effect,
            projection: ThreadProjection,
        ) -> std::result::Result<Vec<DomainEvent>, AdapterError> {
            if !self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
                panic!("isolated effect failure");
            }
            Completion.execute(effect, projection).await
        }
    }
    #[tokio::test]
    async fn worker_survives_a_panicking_job_and_processes_another_thread() {
        let store = setup();
        let mut other = create();
        other.thread_id = ThreadId::new("other").unwrap();
        other.command_id = CommandId::new("other-create").unwrap();
        store
            .dispatch(&other, &now(), &turns(), Driver::Codex)
            .unwrap();
        let mut input = send("other-send", DispatchMode::StartImmediately);
        input.thread_id = other.thread_id.clone();
        store
            .dispatch(&input, &now(), &turns(), Driver::Codex)
            .unwrap();
        let (stop, shutdown) = watch::channel(false);
        let worker = tokio::spawn(
            EffectWorker {
                store: store.clone(),
                adapter: Arc::new(PanicOnce(std::sync::atomic::AtomicBool::new(false))),
                owner: "test".into(),
            }
            .run(shutdown),
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if [create().thread_id, other.thread_id.clone()]
                    .iter()
                    .any(|id| store.projection(id).unwrap().runs[0].status == RunStatus::Completed)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!worker.is_finished());
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
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
