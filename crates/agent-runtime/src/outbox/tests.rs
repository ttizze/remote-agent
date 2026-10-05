//! Ports of T3 EffectWorker.test.ts and the outbox cases of FoundationPersistence.test.ts.
//! T3 `terminal.cleanup` maps to `DeleteAttachments`, `provider-turn.start` to a
//! process-bound provider effect, `provider-runtime.continue` to `SendToThread`.
use super::*;
use crate::actor::tests::{command_id, create, send};
use crate::store::tests::{at, temp_store, thread};
use crate::{
    ActorContext, ActorRegistry, Clock, CommandOrigin, CommitBatch, ManualClock, SearchChanges,
    Settlement, Store, StoreError, effect_kind,
};
use agent_domain::{
    Command, Effect, EffectBody, EffectResult, ProviderCommand, ProviderOperation, Reply,
    RunStatus, State, ThreadId, Timestamp,
};
use futures_util::FutureExt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

type Outcome = Result<Option<EffectResult>, EffectError>;
type Run = Arc<dyn Fn(EffectJob) -> BoxFuture<'static, Outcome> + Send + Sync>;
type ShouldRun = Arc<dyn Fn(&State, &Effect) -> bool + Send + Sync>;
type Failure = Arc<dyn Fn(&Effect, &str) -> Option<EffectResult> + Send + Sync>;
type SettleHook = Box<dyn Fn(&Settlement, usize) -> Result<bool, StoreError> + Send + Sync>;
type Gate = (Arc<Notify>, Arc<Notify>);

struct Handler {
    durability: Durability,
    run: Run,
    should_run: Option<ShouldRun>,
    failure: Option<Failure>,
}
impl EffectHandler for Handler {
    fn durability(&self) -> Durability {
        self.durability
    }
    fn should_run(&self, state: &State, effect: &Effect) -> bool {
        self.should_run
            .as_ref()
            .is_none_or(|check| check(state, effect))
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Outcome> {
        (self.run)(job)
    }
    fn failure(&self, effect: &Effect, error: &str) -> Option<EffectResult> {
        self.failure.as_ref().and_then(|map| map(effect, error))
    }
}
fn run<F: Future<Output = Outcome> + Send + 'static>(
    body: impl Fn(EffectJob) -> F + Send + Sync + 'static,
) -> Run {
    Arc::new(move |job| body(job).boxed())
}
fn succeed() -> Run {
    run(|_| async { Ok(None) })
}
fn handler(durability: Durability, run: Run) -> Handler {
    Handler {
        durability,
        run,
        should_run: None,
        failure: None,
    }
}
const PROVIDER: &str = "Provider.Interrupt";
const START: &str = "Provider.Start";
const CLEANUP: &str = "DeleteAttachments";
const TITLE: &str = "GenerateTitle";
const FORWARD: &str = "SendToThread";
/// Every test kind runs `run`, with T3's durability for the kind it stands for.
fn handlers(run: Run) -> EffectHandlers {
    let mut handlers = EffectHandlers::default();
    for (kind, durability) in [
        (PROVIDER, Durability::ProcessBound),
        (START, Durability::ProcessBound),
        (CLEANUP, Durability::ReplaySafe),
        (TITLE, Durability::ReplaySafe),
        (FORWARD, Durability::ReplaySafe),
    ] {
        handlers = handlers.with(kind, Arc::new(handler(durability, run.clone())));
    }
    handlers
}

fn effect(id: &str, body: EffectBody) -> Effect {
    Effect {
        id: id.into(),
        attempt: None,
        body,
    }
}
fn provider(id: &str) -> Effect {
    effect(
        id,
        EffectBody::Provider(ProviderCommand::Interrupt {
            native_thread: None,
            native_turn: None,
        }),
    )
}
fn cleanup(id: &str) -> Effect {
    effect(id, EffectBody::DeleteAttachments { paths: vec![] })
}
fn title(id: &str) -> Effect {
    effect(id, EffectBody::GenerateTitle { text: id.into() })
}
fn forward(id: &str) -> Effect {
    effect(
        id,
        EffectBody::SendToThread {
            thread: thread("thread:forward-target"),
            command: Box::new(Command::Rename { title: id.into() }),
        },
    )
}

struct Db {
    _dir: tempfile::TempDir,
    store: Store,
    clock: Arc<ManualClock>,
    outbox: Arc<SqliteOutbox>,
    registry: Arc<ActorRegistry>,
}
fn db() -> Db {
    let (dir, store) = temp_store();
    let clock = Arc::new(ManualClock::new(&at()));
    let mut context = ActorContext::new(store.clone());
    context.clock = clock.clone();
    Db {
        _dir: dir,
        outbox: SqliteOutbox::new(store.clone(), clock.clone()),
        registry: ActorRegistry::new(context),
        store,
        clock,
    }
}
impl Db {
    fn now(&self) -> i64 {
        self.clock.now().millis()
    }
    async fn enqueue(&self, thread: &ThreadId, effects: Vec<Effect>) {
        self.enqueue_after(thread, effects, 0).await;
    }
    /// Commits `effects` as one step of `thread`, available `delay` ms from now.
    async fn enqueue_after(&self, thread: &ThreadId, effects: Vec<Effect>, delay: i64) {
        let head = self.store.thread_head(thread).unwrap();
        self.store
            .commit(CommitBatch {
                thread: thread.clone(),
                at: Timestamp::from_millis(self.now() + delay).unwrap(),
                base_thread_seq: head.thread_seq,
                input_seq: head.input_seq + 1,
                receipt: None,
                facts: vec![],
                effects,
                settle: None,
                shell: None,
                needs_recovery: false,
                search: SearchChanges::default(),
                attachment_paths: vec![],
                snapshot: None,
            })
            .await
            .unwrap();
    }
    async fn claim(&self, worker: &str) -> Option<OutboxRow> {
        self.outbox
            .claim(worker, Duration::from_secs(30))
            .await
            .unwrap()
    }
    async fn row(&self, id: &str) -> OutboxRow {
        self.outbox.get(id).await.unwrap().unwrap()
    }
    async fn succeed(&self, id: &str, worker: &str) {
        assert!(
            self.outbox
                .settle(id, worker, Settlement::Succeeded)
                .await
                .unwrap()
        );
    }
    fn worker(&self, handlers: EffectHandlers, options: WorkerOptions) -> Arc<EffectWorker> {
        Arc::new(EffectWorker::new(
            self.outbox.clone(),
            handlers,
            self.registry.clone(),
            self.clock.clone(),
            options,
        ))
    }
}
fn options(worker_id: &str) -> WorkerOptions {
    WorkerOptions {
        worker_id: worker_id.into(),
        ..WorkerOptions::default()
    }
}
async fn eventually(mut done: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("condition within 5 s");
}

/// Follows paused Tokio time, so `advance` moves deadlines and sleeps together.
struct TokioClock {
    base: i64,
    start: tokio::time::Instant,
}
impl TokioClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            base: at().millis(),
            start: tokio::time::Instant::now(),
        })
    }
}
impl Clock for TokioClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.base + self.start.elapsed().as_millis() as i64).unwrap()
    }
}

type Hook<T> = Box<dyn Fn(&FakeQueue, usize) -> Result<T, StoreError> + Send + Sync>;
/// A scripted queue for worker paths a real database cannot be made to take.
struct FakeQueue {
    row: Option<OutboxRow>,
    claim: Option<Hook<Option<OutboxRow>>>,
    get: Option<Hook<Option<OutboxRow>>>,
    settle: Option<SettleHook>,
    retry: Option<Hook<bool>>,
    next: Mutex<Option<i64>>,
    claims: AtomicUsize,
    gets: AtomicUsize,
    settles: Mutex<Vec<(String, Settlement)>>,
    retries: Mutex<Vec<(String, String, Duration)>>,
    armed: AtomicBool,
    token: CancellationToken,
    wake: watch::Sender<u64>,
}
impl FakeQueue {
    fn new(row: Option<OutboxRow>) -> Self {
        Self {
            row,
            claim: None,
            get: None,
            settle: None,
            retry: None,
            next: Mutex::new(None),
            claims: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            settles: Mutex::new(Vec::new()),
            retries: Mutex::new(Vec::new()),
            armed: AtomicBool::new(false),
            token: CancellationToken::new(),
            wake: watch::Sender::new(0),
        }
    }
    fn claims(&self) -> usize {
        self.claims.load(Ordering::SeqCst)
    }
    fn settled(&self, matching: impl Fn(&Settlement) -> bool) -> Vec<String> {
        let settles = self.settles.lock().unwrap();
        settles
            .iter()
            .filter(|(_, settlement)| matching(settlement))
            .map(|(_, settlement)| settlement.error().unwrap_or_default().to_string())
            .collect()
    }
}
impl OutboxQueue for FakeQueue {
    fn claim(&self, _: &str, _: Duration) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>> {
        let index = self.claims.fetch_add(1, Ordering::SeqCst);
        let result = match &self.claim {
            Some(hook) => hook(self, index),
            None => Ok(self.row.clone()),
        };
        async move { result }.boxed()
    }
    fn get(&self, _: &str) -> BoxFuture<'_, Result<Option<OutboxRow>, StoreError>> {
        let index = self.gets.fetch_add(1, Ordering::SeqCst);
        let result = match &self.get {
            Some(hook) => hook(self, index),
            None => Ok(self.row.clone()),
        };
        async move { result }.boxed()
    }
    fn settle(
        &self,
        _: &str,
        worker: &str,
        settlement: Settlement,
    ) -> BoxFuture<'_, Result<bool, StoreError>> {
        let index = self.settles.lock().unwrap().len();
        self.settles
            .lock()
            .unwrap()
            .push((worker.into(), settlement.clone()));
        let result = match &self.settle {
            Some(hook) => hook(&settlement, index),
            None => Ok(true),
        };
        async move { result }.boxed()
    }
    fn retry(
        &self,
        _: &str,
        worker: &str,
        error: &str,
        delay: Duration,
    ) -> BoxFuture<'_, Result<bool, StoreError>> {
        let index = {
            let mut retries = self.retries.lock().unwrap();
            retries.push((worker.into(), error.into(), delay));
            retries.len() - 1
        };
        let result = match &self.retry {
            Some(hook) => hook(self, index),
            None => Ok(true),
        };
        async move { result }.boxed()
    }
    fn next_claimable_at(&self) -> BoxFuture<'_, Result<Option<i64>, StoreError>> {
        let next = *self.next.lock().unwrap();
        async move { Ok(next) }.boxed()
    }
    fn cancellation(&self, _: &str) -> CancellationToken {
        self.armed.store(true, Ordering::SeqCst);
        self.token.clone()
    }
    fn clear_cancellation(&self, _: &str) {}
    fn wakes(&self) -> watch::Receiver<u64> {
        self.wake.subscribe()
    }
}
fn simulated(message: &str) -> StoreError {
    StoreError::Corrupt(message.into())
}
fn claimed(effect: Effect, thread_id: &str, worker: &str, attempts: u32) -> OutboxRow {
    let now = at().millis();
    OutboxRow {
        kind: effect_kind(&effect.body).unwrap(),
        effect,
        thread: thread(thread_id),
        lane: "main".into(),
        status: EffectStatus::Running,
        attempts,
        available_at: now,
        lease_owner: Some(worker.into()),
        lease_expires_at: Some(now),
        created_at: now,
        updated_at: now,
        completed_at: None,
        last_error: None,
    }
}
fn cancelled(mut row: OutboxRow) -> OutboxRow {
    row.status = EffectStatus::Cancelled;
    row.lease_owner = None;
    row.lease_expires_at = None;
    row.completed_at = Some(row.updated_at);
    row
}
struct Fake {
    _dir: tempfile::TempDir,
    queue: Arc<FakeQueue>,
    worker: Arc<EffectWorker>,
}
fn fake(queue: FakeQueue, handlers: EffectHandlers, options: WorkerOptions) -> Fake {
    fake_with_clock(queue, handlers, options, Arc::new(ManualClock::new(&at())))
}
fn fake_with_clock(
    queue: FakeQueue,
    handlers: EffectHandlers,
    options: WorkerOptions,
    clock: Arc<dyn Clock>,
) -> Fake {
    let (dir, store) = temp_store();
    let queue = Arc::new(queue);
    let worker = Arc::new(EffectWorker::new(
        queue.clone(),
        handlers,
        ActorRegistry::new(ActorContext::new(store)),
        clock,
        options,
    ));
    Fake {
        _dir: dir,
        queue,
        worker,
    }
}
fn counting(count: &Arc<AtomicUsize>) -> Run {
    let count = count.clone();
    run(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        async { Ok(None) }
    })
}

// EffectWorker.test.ts

#[tokio::test]
async fn requeues_a_claim_when_a_pre_execution_worker_check_fails() {
    let worker_id = "worker-pre-execution-failure";
    let mut queue = FakeQueue::new(Some(claimed(
        cleanup("effect:worker-pre-execution-failure"),
        "thread:worker-pre-execution-failure",
        worker_id,
        1,
    )));
    queue.get = Some(Box::new(|_, _| {
        Err(simulated("simulated cancellation-state read failure"))
    }));
    let executions = Arc::new(AtomicUsize::new(0));
    let f = fake(queue, handlers(counting(&executions)), options(worker_id));

    let error = f.worker.run_once().await.unwrap_err();

    assert!(
        error
            .to_string()
            .contains("simulated cancellation-state read failure")
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let retries = f.queue.retries.lock().unwrap().clone();
    let (worker, message, delay) = retries.first().unwrap();
    assert_eq!(worker, worker_id);
    assert_eq!(*delay, Duration::ZERO);
    assert!(message.contains("simulated cancellation-state read failure"));
}

#[tokio::test]
async fn arms_cancellation_before_the_durable_pre_execution_check() {
    let worker_id = "worker-cancellation-registration-race";
    let row = claimed(
        cleanup("effect:worker-cancellation-registration-race"),
        "thread:worker-cancellation-registration-race",
        worker_id,
        1,
    );
    let mut queue = FakeQueue::new(Some(row.clone()));
    // A cancellation commits right after this durable read took its snapshot. Its
    // process-local signal only arrives when the worker armed it before the read.
    queue.get = Some(Box::new(move |queue, _| {
        if queue.armed.load(Ordering::SeqCst) {
            queue.token.cancel();
        }
        Ok(Some(row.clone()))
    }));
    let executions = Arc::new(AtomicUsize::new(0));
    let count = executions.clone();
    let execute = run(move |_| {
        let count = count.clone();
        async move {
            tokio::task::yield_now().await;
            count.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    });
    let f = fake(queue, handlers(execute), options(worker_id));

    assert!(f.worker.run_once().await.unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(f.queue.settles.lock().unwrap().is_empty());
}

#[tokio::test]
async fn terminalizes_a_process_bound_claim_when_success_settlement_fails() {
    let worker_id = "worker-process-bound-settlement-failure";
    let mut queue = FakeQueue::new(Some(claimed(
        provider("effect:worker-process-bound-settlement-failure"),
        "thread:worker-process-bound-settlement-failure",
        worker_id,
        1,
    )));
    queue.settle = Some(Box::new(|settlement, _| match settlement {
        Settlement::Succeeded => Err(simulated("simulated success settlement failure")),
        _ => Ok(true),
    }));
    let executions = Arc::new(AtomicUsize::new(0));
    let f = fake(queue, handlers(counting(&executions)), options(worker_id));

    assert!(f.worker.run_once().await.is_err());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(f.queue.retries.lock().unwrap().is_empty());
    let failed = f
        .queue
        .settled(|settlement| matches!(settlement, Settlement::Failed(_)));
    let terminal = failed.first().unwrap();
    assert!(terminal.contains("after execution started"));
    assert!(terminal.contains("simulated success settlement failure"));
}

#[tokio::test]
async fn requeues_a_replay_safe_claim_when_success_settlement_fails() {
    let worker_id = "worker-replay-safe-settlement-failure";
    let mut queue = FakeQueue::new(Some(claimed(
        cleanup("effect:worker-replay-safe-settlement-failure"),
        "thread:worker-replay-safe-settlement-failure",
        worker_id,
        1,
    )));
    queue.settle = Some(Box::new(|settlement, _| match settlement {
        Settlement::Succeeded => Err(simulated("simulated replay-safe settlement failure")),
        _ => Ok(true),
    }));
    let f = fake(queue, handlers(succeed()), options(worker_id));

    assert!(f.worker.run_once().await.is_err());
    assert_eq!(f.queue.retries.lock().unwrap().len(), 1);
    assert!(
        f.queue
            .settled(|settlement| matches!(settlement, Settlement::Failed(_)))
            .is_empty()
    );
}

#[tokio::test]
async fn keeps_a_process_bound_executor_failure_retryable_when_retry_settlement_fails() {
    let worker_id = "worker-process-bound-retry-settlement-failure";
    let mut queue = FakeQueue::new(Some(claimed(
        provider("effect:worker-process-bound-retry-settlement-failure"),
        "thread:worker-process-bound-retry-settlement-failure",
        worker_id,
        1,
    )));
    queue.retry = Some(Box::new(|_, index| {
        if index == 0 {
            Err(simulated("simulated retry settlement failure"))
        } else {
            Ok(true)
        }
    }));
    let execute = run(|_| async {
        Err(EffectError::Retryable(
            "simulated provider execution failure".into(),
        ))
    });
    let f = fake(queue, handlers(execute), options(worker_id));

    assert!(f.worker.run_once().await.is_err());
    assert_eq!(f.queue.retries.lock().unwrap().len(), 2);
    assert!(
        f.queue
            .settled(|settlement| matches!(settlement, Settlement::Failed(_)))
            .is_empty()
    );
}

#[tokio::test]
async fn keeps_a_max_attempt_replay_safe_failure_terminal_when_fail_settlement_fails() {
    let worker_id = "worker-replay-safe-terminal-settlement-failure";
    let mut queue = FakeQueue::new(Some(claimed(
        cleanup("effect:worker-replay-safe-terminal-settlement-failure"),
        "thread:worker-replay-safe-terminal-settlement-failure",
        worker_id,
        5,
    )));
    queue.settle = Some(Box::new(|settlement, index| match settlement {
        Settlement::Failed(_) if index == 0 => {
            Err(simulated("simulated terminal settlement failure"))
        }
        _ => Ok(true),
    }));
    let execute = run(|_| async {
        Err(EffectError::Retryable(
            "simulated terminal cleanup failure".into(),
        ))
    });
    let f = fake(
        queue,
        handlers(execute),
        WorkerOptions {
            max_attempts: 5,
            ..options(worker_id)
        },
    );

    assert!(f.worker.run_once().await.is_err());
    assert_eq!(
        f.queue
            .settled(|settlement| matches!(settlement, Settlement::Failed(_)))
            .len(),
        2
    );
    assert!(f.queue.retries.lock().unwrap().is_empty());
}

/// Paused time only moves through `advance`, so this yields instead of sleeping.
async fn await_claims(queue: &FakeQueue, expected: usize) {
    for _ in 0..10_000 {
        if queue.claims() >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("expected {expected} claims, saw {}", queue.claims());
}
async fn settle_tasks() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
fn idle_daemon(queue: FakeQueue, clock: Arc<TokioClock>) -> (Fake, EffectDaemon) {
    let f = fake_with_clock(
        queue,
        EffectHandlers::default(),
        WorkerOptions::default(),
        clock,
    );
    let daemon = f.worker.clone().spawn(DaemonOptions {
        concurrency: 1,
        liveness: Duration::from_millis(1_000),
    });
    (f, daemon)
}

#[tokio::test(start_paused = true)]
async fn uses_durable_deadlines_notifications_and_a_slow_liveness_poll() {
    let clock = TokioClock::new();
    let now = clock.now().millis();
    let mut queue = FakeQueue::new(None);
    *queue.next.get_mut().unwrap() = Some(now + 100);
    queue.claim = Some(Box::new(move |queue, index| {
        match index + 1 {
            2 => *queue.next.lock().unwrap() = Some(now + 5_000),
            3 => *queue.next.lock().unwrap() = None,
            _ => {}
        }
        Ok(None)
    }));
    let (f, _daemon) = idle_daemon(queue, clock);

    await_claims(&f.queue, 1).await;
    tokio::time::advance(Duration::from_millis(99)).await;
    settle_tasks().await;
    assert_eq!(f.queue.claims(), 1);

    tokio::time::advance(Duration::from_millis(1)).await;
    await_claims(&f.queue, 2).await;
    tokio::time::advance(Duration::from_millis(999)).await;
    settle_tasks().await;
    assert_eq!(f.queue.claims(), 2);

    f.queue.wake.send_modify(|version| *version += 1);
    await_claims(&f.queue, 3).await;
    tokio::time::advance(Duration::from_millis(999)).await;
    settle_tasks().await;
    assert_eq!(f.queue.claims(), 3);

    tokio::time::advance(Duration::from_millis(1)).await;
    await_claims(&f.queue, 4).await;
}

#[tokio::test(start_paused = true)]
async fn does_not_hot_loop_when_a_claim_fails() {
    let clock = TokioClock::new();
    let mut queue = FakeQueue::new(None);
    *queue.next.get_mut().unwrap() = Some(clock.now().millis());
    queue.claim = Some(Box::new(|_, _| {
        Err(simulated("simulated database failure"))
    }));
    let (f, _daemon) = idle_daemon(queue, clock);

    await_claims(&f.queue, 1).await;
    tokio::time::advance(Duration::from_millis(999)).await;
    settle_tasks().await;
    assert_eq!(f.queue.claims(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    await_claims(&f.queue, 2).await;
}

#[tokio::test(start_paused = true)]
async fn backs_off_briefly_when_a_due_deadline_loses_a_claim_race() {
    let clock = TokioClock::new();
    let mut queue = FakeQueue::new(None);
    *queue.next.get_mut().unwrap() = Some(clock.now().millis());
    queue.claim = Some(Box::new(|_, _| Ok(None)));
    let (f, _daemon) = idle_daemon(queue, clock);

    await_claims(&f.queue, 1).await;
    tokio::time::advance(Duration::from_millis(24)).await;
    settle_tasks().await;
    assert_eq!(f.queue.claims(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    await_claims(&f.queue, 2).await;
}

#[tokio::test]
async fn safely_retries_after_replacement_cleanup_succeeds_and_start_fails() {
    let db = db();
    let id = thread("thread:effect-worker-restart");
    db.enqueue(&id, vec![provider("effect:restart:replace")])
        .await;
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let fail_first_start = Arc::new(AtomicBool::new(true));
    let record = events.clone();
    let execute = run(move |_| {
        let (events, fail_first_start) = (record.clone(), fail_first_start.clone());
        async move {
            for step in ["interrupt:replacement", "detach", "start"] {
                events.lock().unwrap().push(step.into());
            }
            if fail_first_start.swap(false, Ordering::SeqCst) {
                return Err(EffectError::Retryable(
                    "simulated first start failure".into(),
                ));
            }
            Ok(None)
        }
    });
    let worker = db.worker(handlers(execute), options("restart-worker"));

    assert!(worker.run_once().await.unwrap());
    assert_eq!(
        db.row("effect:restart:replace").await.status,
        EffectStatus::Pending
    );
    db.clock.advance(100);
    assert!(worker.run_once().await.unwrap());

    assert_eq!(
        *events.lock().unwrap(),
        [
            "interrupt:replacement",
            "detach",
            "start",
            "interrupt:replacement",
            "detach",
            "start"
        ]
    );
    assert_eq!(
        db.row("effect:restart:replace").await.status,
        EffectStatus::Succeeded
    );
}

#[tokio::test]
async fn settles_a_delegated_child_once_its_restart_continuation_fails_for_good() {
    let db = db();
    let id = thread("thread:effect-worker-restart");
    let effect_id = "effect:restart-continuation:run";
    db.enqueue(&id, vec![forward(effect_id)]).await;
    let recovered = Arc::new(Mutex::new(Vec::<ThreadId>::new()));
    let record = recovered.clone();
    // A continuation that will never run still owes a delegated parent a result.
    let execute = run(move |job| {
        let recovered = record.clone();
        async move {
            if !job.will_retry {
                recovered.lock().unwrap().push(job.thread);
            }
            Err(EffectError::Retryable("provider instance removed".into()))
        }
    });
    let worker = db.worker(
        handlers(execute),
        WorkerOptions {
            max_attempts: 2,
            ..options("continuation-worker")
        },
    );

    assert!(worker.run_once().await.unwrap());
    assert!(recovered.lock().unwrap().is_empty());
    db.clock.advance(100);
    assert!(worker.run_once().await.unwrap());
    assert_eq!(*recovered.lock().unwrap(), [id]);
    let row = db.row(effect_id).await;
    assert_eq!(row.status, EffectStatus::Failed);
    assert_eq!(row.last_error.as_deref(), Some("provider instance removed"));
}

// FoundationPersistence.test.ts

#[tokio::test]
async fn keeps_one_durable_effect_across_command_retries_and_executes_it_after_recovery() {
    let db = db();
    let id = thread("thread:foundation-effect-recovery");
    let handle = db.registry.get_or_load(&id).await.unwrap();
    handle
        .dispatch(command_id("create"), create(&id), CommandOrigin::Client)
        .await
        .unwrap();
    let command = command_id("command:foundation-effect-recovery");
    let first = handle
        .dispatch(command.clone(), send("m"), CommandOrigin::Client)
        .await
        .unwrap();
    let retry = handle
        .dispatch(command, send("m"), CommandOrigin::Client)
        .await
        .unwrap();
    assert!(!first.replayed);
    assert!(retry.replayed);
    assert_eq!(retry.global_seq, first.global_seq);
    let starts: Vec<_> = db
        .store
        .outbox(&id)
        .unwrap()
        .into_iter()
        .filter(|row| row.kind == START)
        .collect();
    assert_eq!(starts.len(), 1);

    let executions = Arc::new(AtomicUsize::new(0));
    let count = executions.clone();
    let execute = run(move |job| {
        if effect_kind(&job.effect.body).unwrap() == START {
            count.fetch_add(1, Ordering::SeqCst);
        }
        async { Ok(None) }
    });
    let worker = db.worker(handlers(execute), options("recovery-worker"));
    assert!(worker.run_once().await.unwrap());
    worker.drain(usize::MAX).await.unwrap();
    assert!(!worker.run_once().await.unwrap());

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        db.row(&starts[0].effect.id).await.status,
        EffectStatus::Succeeded
    );
}

#[tokio::test]
async fn does_not_wake_claimers_for_effects_from_an_idempotent_command_retry() {
    let db = db();
    let id = thread("thread:foundation-idempotent-wakeup");
    let handle = db.registry.get_or_load(&id).await.unwrap();
    handle
        .dispatch(command_id("create"), create(&id), CommandOrigin::Client)
        .await
        .unwrap();
    let mut wakes = db.outbox.wakes();
    wakes.borrow_and_update();
    let command = command_id("command:foundation-idempotent-wakeup");

    handle
        .dispatch(command.clone(), send("m"), CommandOrigin::Client)
        .await
        .unwrap();
    assert!(wakes.has_changed().unwrap());
    wakes.borrow_and_update();
    assert!(
        handle
            .dispatch(command, send("m"), CommandOrigin::Client)
            .await
            .unwrap()
            .replayed
    );

    assert!(!wakes.has_changed().unwrap());
}

#[tokio::test]
async fn interrupts_a_running_process_bound_effect_when_it_is_cancelled() {
    let db = db();
    let id = thread("thread:foundation-cancel-running-effect");
    let effect_id = "effect:foundation-cancel-running-effect";
    db.enqueue(&id, vec![provider(effect_id)]).await;
    let started = Arc::new(Notify::new());
    let (interrupted, on_interrupt) = tokio::sync::oneshot::channel::<()>();
    let interrupted = Arc::new(Mutex::new(Some(interrupted)));
    let signal = started.clone();
    let execute = run(move |_| {
        let (started, interrupted) = (signal.clone(), interrupted.clone());
        async move {
            struct OnDrop(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for OnDrop {
                fn drop(&mut self) {
                    if let Some(sender) = self.0.take() {
                        let _ = sender.send(());
                    }
                }
            }
            let _guard = OnDrop(interrupted.lock().unwrap().take());
            started.notify_one();
            std::future::pending::<()>().await;
            Ok(None)
        }
    });
    let worker = db.worker(handlers(execute), options("cancellation-worker"));
    let running = tokio::spawn({
        let worker = worker.clone();
        async move { worker.run_once().await }
    });
    started.notified().await;

    let cancelled = db
        .outbox
        .cancel(&id, &[PROVIDER], "The owning run was interrupted.")
        .await
        .unwrap();
    assert_eq!(cancelled, [effect_id]);
    assert!(running.await.unwrap().unwrap());
    on_interrupt.await.unwrap();

    assert_eq!(db.row(effect_id).await.status, EffectStatus::Cancelled);
}

#[tokio::test]
async fn treats_cancellation_between_execution_and_settlement_as_a_normal_outcome() {
    let worker_id = "settlement-race-worker";
    let row = claimed(
        cleanup("effect:foundation-cancel-before-settlement"),
        "thread:foundation-cancel-before-settlement",
        worker_id,
        1,
    );
    let mut queue = FakeQueue::new(Some(row.clone()));
    queue.settle = Some(Box::new(|_, _| Ok(false)));
    // The pre-execution read still sees the claim; the cancellation lands while it runs.
    queue.get = Some(Box::new(move |_, index| {
        Ok(Some(if index == 0 {
            row.clone()
        } else {
            cancelled(row.clone())
        }))
    }));
    let executions = Arc::new(AtomicUsize::new(0));
    let f = fake(queue, handlers(counting(&executions)), options(worker_id));

    assert!(f.worker.run_once().await.unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn does_not_start_an_effect_that_was_cancelled_during_claim_registration() {
    let worker_id = "claim-cancellation-worker";
    let row = claimed(
        cleanup("effect:foundation-cancelled-during-claim"),
        "thread:foundation-cancelled-during-claim",
        worker_id,
        1,
    );
    let mut queue = FakeQueue::new(Some(row.clone()));
    queue.get = Some(Box::new(move |_, _| Ok(Some(cancelled(row.clone())))));
    let executions = Arc::new(AtomicUsize::new(0));
    let f = fake(queue, handlers(counting(&executions)), options(worker_id));

    assert!(f.worker.run_once().await.unwrap());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn allows_only_one_worker_to_claim_an_available_effect() {
    let db = db();
    let id = thread("thread:foundation-exclusive-claim");
    db.enqueue(&id, vec![provider("effect:foundation-exclusive-claim")])
        .await;

    let (a, b) = tokio::join!(db.claim("worker-a"), db.claim("worker-b"));

    assert_eq!(a.is_some() as u8 + b.is_some() as u8, 1);
    if let Some(row) = a {
        db.succeed(&row.effect.id, "worker-a").await;
    }
    if let Some(row) = b {
        db.succeed(&row.effect.id, "worker-b").await;
    }
}

#[tokio::test]
async fn runs_title_generation_beside_critical_work_while_serializing_each_lane() {
    let db = db();
    let id = thread("thread:foundation-title-effect-lane");
    let title_id = "effect:foundation-title-effect-lane:a-title";
    let provider_id = "effect:foundation-title-effect-lane:b-provider";
    let next_title_id = "effect:foundation-title-effect-lane:c-title";
    let next_critical_id = "effect:foundation-title-effect-lane:d-critical";
    db.enqueue(
        &id,
        vec![
            title(title_id),
            provider(provider_id),
            title(next_title_id),
            cleanup(next_critical_id),
        ],
    )
    .await;

    let first_title = db.claim("title-lane-worker").await.unwrap();
    assert_eq!(first_title.effect.id, title_id);
    let critical = db.claim("critical-lane-worker").await.unwrap();
    assert_eq!(critical.effect.id, provider_id);
    assert!(db.claim("blocked-lanes-worker").await.is_none());

    db.succeed(provider_id, "critical-lane-worker").await;
    let next_critical = db.claim("critical-lane-worker").await.unwrap();
    assert_eq!(next_critical.effect.id, next_critical_id);
    db.succeed(next_critical_id, "critical-lane-worker").await;

    db.succeed(title_id, "title-lane-worker").await;
    let next_title = db.claim("title-lane-worker").await.unwrap();
    assert_eq!(next_title.effect.id, next_title_id);
    db.succeed(next_title_id, "title-lane-worker").await;
}

#[tokio::test]
async fn ignores_deadlines_blocked_by_a_running_effect_on_the_same_thread() {
    let db = db();
    let now = db.now();
    let future = now + 10_000;
    let blocked = thread("thread:foundation-next-claimable:blocked");
    let later = thread("thread:foundation-next-claimable:future");
    db.enqueue(
        &blocked,
        vec![
            cleanup("effect:foundation-next-claimable:a1"),
            cleanup("effect:foundation-next-claimable:a2"),
        ],
    )
    .await;
    db.enqueue_after(
        &later,
        vec![cleanup("effect:foundation-next-claimable:b1")],
        10_000,
    )
    .await;

    let claimed = db.claim("next-claimable-worker").await.unwrap();
    assert_eq!(claimed.effect.id, "effect:foundation-next-claimable:a1");
    assert_eq!(db.outbox.next_claimable_at().await.unwrap(), Some(future));

    db.succeed(&claimed.effect.id, "next-claimable-worker")
        .await;
    assert!(db.outbox.next_claimable_at().await.unwrap().unwrap() <= now);

    let unblocked = db.claim("next-claimable-worker").await.unwrap();
    assert_eq!(unblocked.effect.id, "effect:foundation-next-claimable:a2");
    db.succeed(&unblocked.effect.id, "next-claimable-worker")
        .await;
}

#[tokio::test]
async fn wakes_claimers_when_cancellation_unblocks_same_thread_work() {
    let db = db();
    let id = thread("thread:foundation-cancellation-wakeup");
    db.enqueue(
        &id,
        vec![
            provider("effect:foundation-cancellation-wakeup:a-running"),
            cleanup("effect:foundation-cancellation-wakeup:b-pending"),
        ],
    )
    .await;
    let running = db.claim("cancellation-wakeup-worker").await.unwrap();
    assert_eq!(running.kind, PROVIDER);
    let mut wakes = db.outbox.wakes();
    wakes.borrow_and_update();

    db.outbox
        .cancel(&id, &[PROVIDER], "Test cancellation wakeup.")
        .await
        .unwrap();

    assert!(wakes.has_changed().unwrap());
    let unblocked = db.claim("cancellation-wakeup-worker").await.unwrap();
    assert_eq!(unblocked.kind, CLEANUP);
    db.succeed(&unblocked.effect.id, "cancellation-wakeup-worker")
        .await;
}

#[tokio::test]
async fn keeps_later_thread_effects_behind_an_earlier_effect_waiting_to_retry() {
    let db = db();
    let worker = "retry-order-worker";
    let id = thread("thread:foundation-retry-order");
    let rollback_id = "effect:foundation-retry-order:z-rollback";
    db.enqueue(&id, vec![cleanup(rollback_id)]).await;
    let rollback = db.claim(worker).await.unwrap();
    assert!(
        db.outbox
            .retry(
                &rollback.effect.id,
                worker,
                "rollback failed once",
                Duration::from_secs(60)
            )
            .await
            .unwrap()
    );

    // A turn the user starts during the rollback's backoff must not run first,
    // even when its timestamp ties and its id sorts first.
    db.enqueue(
        &id,
        vec![
            provider("effect:foundation-retry-order:a-start"),
            title("effect:foundation-retry-order:b-title"),
        ],
    )
    .await;
    let title = db.claim(worker).await.unwrap();
    assert_eq!(title.effect.id, "effect:foundation-retry-order:b-title");
    assert!(db.claim(worker).await.is_none());
    assert_eq!(
        db.outbox.next_claimable_at().await.unwrap(),
        Some(db.row(rollback_id).await.available_at)
    );

    db.outbox
        .cancel(&id, &[CLEANUP], "Test cleanup.")
        .await
        .unwrap();
    let unblocked = db.claim(worker).await.unwrap();
    assert_eq!(unblocked.effect.id, "effect:foundation-retry-order:a-start");
    db.succeed("effect:foundation-retry-order:a-start", worker)
        .await;
    db.succeed("effect:foundation-retry-order:b-title", worker)
        .await;
}

#[tokio::test]
async fn executes_a_retry_at_its_durable_deadline_instead_of_the_liveness_interval() {
    let db = db();
    let effect_id = "effect:foundation-durable-retry-deadline";
    let executions = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(Notify::new());
    let (count, done) = (executions.clone(), completed.clone());
    let execute = run(move |_| {
        let attempt = count.fetch_add(1, Ordering::SeqCst) + 1;
        let done = done.clone();
        async move {
            if attempt == 1 {
                return Err(EffectError::Retryable("simulated retry".into()));
            }
            done.notify_one();
            Ok(None)
        }
    });
    let _daemon = db
        .worker(handlers(execute), options("durable-retry-deadline-worker"))
        .spawn(DaemonOptions {
            concurrency: 1,
            liveness: Duration::from_secs(30),
        });
    db.enqueue(
        &thread("thread:foundation-durable-retry-deadline"),
        vec![cleanup(effect_id)],
    )
    .await;
    eventually(async || {
        let row = db.row(effect_id).await;
        row.status == EffectStatus::Pending && row.attempts == 1
    })
    .await;

    db.clock.advance(99);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    db.clock.advance(1);
    tokio::time::timeout(Duration::from_secs(5), completed.notified())
        .await
        .unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn runs_distinct_threads_concurrently_while_serializing_effects_within_a_thread() {
    let db = db();
    let (a1, a2, b1) = (
        "effect:foundation-concurrent-effects:a1",
        "effect:foundation-concurrent-effects:a2",
        "effect:foundation-concurrent-effects:b1",
    );
    let gates: Arc<HashMap<&str, Gate>> = Arc::new(
        [a1, a2, b1]
            .into_iter()
            .map(|id| (id, (Arc::new(Notify::new()), Arc::new(Notify::new()))))
            .collect(),
    );
    let started = Arc::new(Mutex::new(Vec::<String>::new()));
    let (gate, log) = (gates.clone(), started.clone());
    let execute = run(move |job| {
        let (gates, started) = (gate.clone(), log.clone());
        async move {
            let (start, release) = gates[job.effect.id.as_str()].clone();
            started.lock().unwrap().push(job.effect.id.clone());
            start.notify_one();
            release.notified().await;
            Ok(None)
        }
    });
    let _daemon = db
        .worker(handlers(execute), options("concurrency-worker"))
        .spawn(DaemonOptions {
            concurrency: 2,
            ..DaemonOptions::default()
        });
    let thread_a = thread("thread:foundation-concurrent-effects:a");
    let thread_b = thread("thread:foundation-concurrent-effects:b");
    db.enqueue(&thread_a, vec![provider(a1), provider(a2)])
        .await;
    db.enqueue(&thread_b, vec![provider(b1)]).await;

    let wait = Duration::from_secs(5);
    tokio::time::timeout(wait, gates[a1].0.notified())
        .await
        .unwrap();
    tokio::time::timeout(wait, gates[b1].0.notified())
        .await
        .unwrap();
    assert!(!started.lock().unwrap().contains(&a2.to_string()));

    gates[a1].1.notify_one();
    gates[b1].1.notify_one();
    tokio::time::timeout(wait, gates[a2].0.notified())
        .await
        .unwrap();
    gates[a2].1.notify_one();
    eventually(async || {
        let mut rows = db.store.outbox(&thread_a).unwrap();
        rows.extend(db.store.outbox(&thread_b).unwrap());
        rows.iter().all(|row| row.status == EffectStatus::Succeeded)
    })
    .await;
}

#[tokio::test]
async fn does_not_reclaim_a_running_effect_after_its_process_local_lease_expires() {
    let db = db();
    let id = thread("thread:foundation-no-live-reclaim");
    let (first, second) = (
        "effect:foundation-no-live-reclaim:first",
        "effect:foundation-no-live-reclaim:second",
    );
    db.enqueue(&id, vec![cleanup(first), cleanup(second)]).await;
    let first_started = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let executions = Arc::new(Mutex::new(Vec::<String>::new()));
    let (started, release, log) = (
        first_started.clone(),
        release_first.clone(),
        executions.clone(),
    );
    let execute = run(move |job| {
        let (started, release, log) = (started.clone(), release.clone(), log.clone());
        async move {
            log.lock().unwrap().push(job.effect.id.clone());
            if job.effect.id == first {
                started.notify_one();
                release.notified().await;
            }
            Ok(None)
        }
    });
    let worker = db.worker(
        handlers(execute),
        WorkerOptions {
            lease: Duration::from_millis(1),
            ..options("no-live-reclaim-worker")
        },
    );
    let running = tokio::spawn({
        let worker = worker.clone();
        async move { worker.run_once().await }
    });
    first_started.notified().await;
    db.store
        .write(move |tx| {
            tx.execute(
                "UPDATE outbox SET lease_expires_at = 0 WHERE effect_id = ?1",
                [first],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    assert!(!worker.run_once().await.unwrap());
    assert_eq!(*executions.lock().unwrap(), [first]);

    release_first.notify_one();
    assert!(running.await.unwrap().unwrap());
    assert!(worker.run_once().await.unwrap());
    assert_eq!(*executions.lock().unwrap(), [first, second]);
}

async fn retires_live_provider_effects_and_requeues_after_process_loss(replay: fn(&str) -> Effect) {
    let db = db();
    let kind = effect_kind(&replay("probe").body).unwrap();
    let cancelled_id = format!("effect:a-foundation-cancel-provider-turn:{kind}");
    db.enqueue(
        &thread(&format!("thread:foundation-reclaim-running:{kind}")),
        vec![provider(&cancelled_id)],
    )
    .await;
    db.enqueue(
        &thread(&format!("thread:foundation-reclaim-cleanup:{kind}")),
        vec![replay(&format!(
            "effect:b-foundation-requeue-cleanup:{kind}"
        ))],
    )
    .await;
    assert!(db.claim("crashed-worker").await.is_some());
    assert!(db.claim("crashed-worker").await.is_some());

    assert_eq!(
        db.outbox
            .reconcile_after_process_loss(&handlers(succeed()))
            .await
            .unwrap(),
        Reconciled {
            cancelled: 1,
            requeued: 1
        }
    );
    assert_eq!(db.row(&cancelled_id).await.status, EffectStatus::Cancelled);

    let reclaimed = db.claim("recovery-worker").await.unwrap();
    assert_eq!(reclaimed.kind, kind);
    assert_eq!(reclaimed.attempts, 2);
    db.succeed(&reclaimed.effect.id, "recovery-worker").await;
}

#[tokio::test]
async fn retires_live_provider_effects_and_requeues_cleanup_after_process_loss() {
    retires_live_provider_effects_and_requeues_after_process_loss(cleanup).await;
}

#[tokio::test]
async fn retires_live_provider_effects_and_requeues_a_thread_command_after_process_loss() {
    retires_live_provider_effects_and_requeues_after_process_loss(forward).await;
}

/// The outbox half of "atomically cancels stale runs and their process-bound
/// effects"; terminalizing the runs is the domain's `Recover`.
#[tokio::test]
async fn cancels_process_bound_effects_once_after_process_loss() {
    let db = db();
    let id = thread("thread:foundation-process-loss");
    db.enqueue(&id, vec![provider("effect:foundation-process-loss")])
        .await;
    assert!(db.claim("crashed-worker").await.is_some());
    let handlers = handlers(succeed());

    let first = db
        .outbox
        .reconcile_after_process_loss(&handlers)
        .await
        .unwrap();
    assert_eq!(first.cancelled, 1);
    assert_eq!(
        db.row("effect:foundation-process-loss").await.status,
        EffectStatus::Cancelled
    );
    let second = db
        .outbox
        .reconcile_after_process_loss(&handlers)
        .await
        .unwrap();
    assert_eq!(second, Reconciled::default());
}

#[tokio::test]
async fn keeps_claiming_new_work_after_repeated_idle_periods() {
    let db = db();
    let completions: Arc<Mutex<HashMap<String, Arc<Notify>>>> = Arc::default();
    let signals = completions.clone();
    let execute = run(move |job| {
        let completion = signals.lock().unwrap()[&job.effect.id].clone();
        async move {
            completion.notify_one();
            Ok(None)
        }
    });
    let _daemon = db
        .worker(handlers(execute), options("idle-wave-worker"))
        .spawn(DaemonOptions {
            concurrency: 2,
            ..DaemonOptions::default()
        });
    for wave in 1..=6 {
        tokio::time::sleep(Duration::from_millis(125)).await;
        let effect_id = format!("effect:foundation-idle-wave:{wave}");
        let completion = Arc::new(Notify::new());
        completions
            .lock()
            .unwrap()
            .insert(effect_id.clone(), completion.clone());
        db.enqueue(
            &thread(&format!("thread:foundation-idle-wave:{wave}")),
            vec![cleanup(&effect_id)],
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), completion.notified())
            .await
            .unwrap_or_else(|_| panic!("worker stopped before idle wave {wave}"));
    }
}

// Runtime-specific behavior

async fn thread_with_start(db: &Db, id: &ThreadId) -> OutboxRow {
    let handle = db.registry.get_or_load(id).await.unwrap();
    handle
        .dispatch(command_id("create"), create(id), CommandOrigin::Client)
        .await
        .unwrap();
    let reply = handle
        .dispatch(command_id("send"), send("hello"), CommandOrigin::Client)
        .await
        .unwrap()
        .reply;
    assert!(matches!(reply, Reply::Run(_)), "{reply:?}");
    db.store
        .outbox(id)
        .unwrap()
        .into_iter()
        .find(|row| row.kind == START)
        .unwrap()
}
fn start_failed(effect: &Effect, message: &str) -> EffectResult {
    EffectResult::ProviderFailed {
        attempt: effect.attempt.clone().unwrap(),
        operation: ProviderOperation::Start,
        message: message.into(),
        message_id: None,
        turn_completed: false,
    }
}
async fn run_status(db: &Db, id: &ThreadId) -> RunStatus {
    db.registry.state(id).await.unwrap().runs[0].status
}
fn only(kind: &str, handler: Handler) -> EffectHandlers {
    EffectHandlers::default().with(kind, Arc::new(handler))
}

#[tokio::test]
async fn feeds_a_result_through_the_thread_and_settles_the_row_in_that_commit() {
    let db = db();
    let id = thread("thread:outbox-result");
    let start = thread_with_start(&db, &id).await;
    let execute = run(|job| async move { Ok(Some(start_failed(&job.effect, "spawn failed"))) });
    let worker = db.worker(
        only(START, handler(Durability::ProcessBound, execute)),
        options("result-worker"),
    );

    worker.drain(usize::MAX).await.unwrap();

    assert_eq!(run_status(&db, &id).await, RunStatus::Failed);
    assert_eq!(
        db.row(&start.effect.id).await.status,
        EffectStatus::Succeeded
    );
}

#[tokio::test]
async fn feeds_the_mapped_failure_after_a_permanent_error_and_fails_the_row() {
    let db = db();
    let id = thread("thread:outbox-failure");
    let start = thread_with_start(&db, &id).await;
    let mut start_handler = handler(
        Durability::ProcessBound,
        run(|_| async { Err(EffectError::Permanent("binary missing".into())) }),
    );
    start_handler.failure = Some(Arc::new(|effect, error| Some(start_failed(effect, error))));
    let worker = db.worker(only(START, start_handler), options("failure-worker"));

    worker.drain(usize::MAX).await.unwrap();

    assert_eq!(run_status(&db, &id).await, RunStatus::Failed);
    let row = db.row(&start.effect.id).await;
    assert_eq!(row.status, EffectStatus::Failed);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.last_error.as_deref(), Some("binary missing"));
}

#[tokio::test]
async fn cancels_a_process_bound_effect_its_thread_no_longer_wants() {
    let db = db();
    let id = thread("thread:outbox-skip");
    let start = thread_with_start(&db, &id).await;
    let executions = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(None::<String>));
    let mut start_handler = handler(Durability::ProcessBound, counting(&executions));
    let record = seen.clone();
    start_handler.should_run = Some(Arc::new(move |state, _| {
        *record.lock().unwrap() = state.thread.as_ref().map(|thread| thread.title.clone());
        false
    }));
    let worker = db.worker(only(START, start_handler), options("skip-worker"));

    worker.drain(usize::MAX).await.unwrap();

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(seen.lock().unwrap().as_deref(), Some("Thread"));
    assert_eq!(
        db.row(&start.effect.id).await.status,
        EffectStatus::Cancelled
    );
}

#[tokio::test]
async fn retries_a_panicking_handler_and_keeps_working() {
    let db = db();
    let id = thread("thread:outbox-panic");
    let effect_id = "effect:outbox-panic";
    db.enqueue(&id, vec![cleanup(effect_id)]).await;
    let panicked = Arc::new(AtomicBool::new(false));
    let once = panicked.clone();
    let execute = run(move |_| {
        let first = !once.swap(true, Ordering::SeqCst);
        async move {
            if first {
                panic!("handler bug");
            }
            Ok(None)
        }
    });
    let worker = db.worker(handlers(execute), options("panic-worker"));

    assert!(worker.run_once().await.unwrap());
    let row = db.row(effect_id).await;
    assert_eq!(row.status, EffectStatus::Pending);
    assert!(row.last_error.unwrap().contains("handler bug"));
    db.clock.advance(100);
    assert!(worker.run_once().await.unwrap());
    assert_eq!(db.row(effect_id).await.status, EffectStatus::Succeeded);
}

#[tokio::test]
async fn fails_rows_without_a_handler() {
    let db = db();
    db.enqueue(
        &thread("thread:outbox-missing"),
        vec![cleanup("effect:missing")],
    )
    .await;
    let worker = db.worker(EffectHandlers::default(), options("missing-worker"));

    assert!(worker.run_once().await.unwrap());

    let row = db.row("effect:missing").await;
    assert_eq!(row.status, EffectStatus::Failed);
    assert!(row.last_error.unwrap().contains(CLEANUP));
}

#[tokio::test]
async fn reconciles_unsettled_rows_by_handler_durability() {
    let db = db();
    let id = thread("thread:outbox-reconcile");
    db.enqueue(
        &id,
        vec![
            provider("effect:reconcile:pending-provider"),
            title("effect:reconcile:pending-title"),
        ],
    )
    .await;
    db.enqueue(
        &thread("thread:outbox-reconcile:unknown"),
        vec![forward("effect:reconcile:unknown")],
    )
    .await;
    let handlers = handlers(succeed());
    let without_forward = EffectHandlers::default()
        .with(PROVIDER, handlers.get(PROVIDER).unwrap().clone())
        .with(TITLE, handlers.get(TITLE).unwrap().clone());

    let reconciled = db
        .outbox
        .reconcile_after_process_loss(&without_forward)
        .await
        .unwrap();

    assert_eq!(
        reconciled,
        Reconciled {
            cancelled: 2,
            requeued: 0
        }
    );
    for (id, status) in [
        ("effect:reconcile:pending-provider", EffectStatus::Cancelled),
        ("effect:reconcile:pending-title", EffectStatus::Pending),
        ("effect:reconcile:unknown", EffectStatus::Cancelled),
    ] {
        assert_eq!(db.row(id).await.status, status, "{id}");
    }
}

#[test]
fn backs_off_exponentially_up_to_thirty_seconds() {
    let delays: Vec<u64> = [1, 2, 3, 4, 5, 9, 10, 40]
        .into_iter()
        .map(|attempt| retry_delay(attempt).as_millis() as u64)
        .collect();
    assert_eq!(delays, [100, 200, 400, 800, 1_600, 25_600, 30_000, 30_000]);
}
