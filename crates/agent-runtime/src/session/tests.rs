//! Ports of T3 ProviderSessionManager, ProviderTurnStartService and
//! ProviderTurnControlService behavior tests, plus the frame ordering rule.
mod fake;
mod replay;

use super::*;
use crate::store::tests::{at, temp_store};
use crate::{
    ActorContext, CommandOrigin, Durability, EffectError, EffectHandler, EffectHandlers, EffectJob,
    EffectWorker, ManualClock, SqliteOutbox, Store, WorkerOptions, effect_kind,
};
use agent_domain::{
    BackgroundKind, Command, CommandId, DispatchMode, Effect, EffectBody, ItemKind, ItemStatus,
    MessageAuthor, MessageId, Reply, RequestStatus, RunId, RunStatus, SendMessage,
};
use fake::{FakeHost, Gate};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

pub(crate) struct Forward(pub(crate) Arc<ActorRegistry>);
impl EffectHandler for Forward {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::SendToThread { thread, command } = &job.effect.body else {
                return Err(EffectError::Permanent("not a forward".into()));
            };
            self.0
                .dispatch(
                    thread,
                    CommandId::new(format!("effect:{}", job.effect.id)).unwrap(),
                    (**command).clone(),
                    CommandOrigin::Internal,
                )
                .await
                .map_err(|error| EffectError::Retryable(error.to_string()))?;
            Ok(None)
        })
    }
}

pub(crate) struct Rig {
    _dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) registry: Arc<ActorRegistry>,
    pub(crate) sessions: Arc<SessionManager>,
    pub(crate) host: Arc<FakeHost>,
    pub(crate) handlers: EffectHandlers,
    pub(crate) worker: Arc<EffectWorker>,
    serial: AtomicU64,
}

pub(crate) fn rig(options: SessionOptions, max_attempts: u32) -> Rig {
    let (dir, store) = temp_store();
    let clock = Arc::new(ManualClock::new(&at()));
    let live = LiveSessions::default();
    let mut context = ActorContext::new(store.clone());
    context.clock = clock.clone();
    context.residency = Arc::new(live.clone());
    let registry = ActorRegistry::new(context);
    let host = FakeHost::new();
    let sessions = SessionManager::new(registry.clone(), host.clone(), options, live);
    let handlers = with_session_handlers(EffectHandlers::default(), &sessions)
        .with("SendToThread", Arc::new(Forward(registry.clone())));
    let outbox = SqliteOutbox::new(store.clone(), clock.clone());
    let worker = Arc::new(EffectWorker::new(
        outbox,
        handlers.clone(),
        registry.clone(),
        clock.clone(),
        WorkerOptions {
            max_attempts,
            ..WorkerOptions::default()
        },
    ));
    Rig {
        _dir: dir,
        store,
        registry,
        sessions,
        host,
        handlers,
        worker,
        serial: AtomicU64::new(0),
    }
}

pub(crate) fn thread(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

pub(crate) fn selection(driver: Driver, model: &str) -> ModelSelection {
    ModelSelection {
        instance: match driver {
            Driver::Codex => "codex".into(),
            Driver::Claude => "claude".into(),
        },
        driver,
        model: model.into(),
        options: BTreeMap::new(),
    }
}

fn codex_replies(frame: &Value) -> Vec<Value> {
    let id = frame["id"].clone();
    match frame["method"].as_str() {
        Some("initialize") => vec![json!({"id":id,"result":{}})],
        Some("thread/start") => {
            vec![json!({"id":id,"result":{"thread":{"id":"native-thread"}}})]
        }
        Some("thread/resume") => {
            vec![json!({"id":id,"result":{"thread":{"id":frame["params"]["threadId"]}}})]
        }
        Some("turn/start") => vec![json!({"id":id,"result":{"turn":{"id":"native-turn"}}})],
        Some("turn/interrupt") => vec![json!({"id":id,"result":{}})],
        _ => vec![],
    }
}

fn claude_replies(frame: &Value) -> Vec<Value> {
    if frame["type"] == "control_request" && frame["request"]["subtype"] != "interrupt" {
        return vec![
            json!({"type":"control_response","response":{"subtype":"success","request_id":frame["request_id"],"response":{}}}),
        ];
    }
    vec![]
}

impl Rig {
    fn command_id(&self) -> CommandId {
        CommandId::new(format!(
            "command-{}",
            self.serial.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap()
    }
    pub(crate) async fn command(&self, thread: &ThreadId, command: Command) -> Reply {
        self.registry
            .dispatch(thread, self.command_id(), command, CommandOrigin::Client)
            .await
            .unwrap()
            .reply
    }
    pub(crate) async fn create(&self, id: &ThreadId, selection: ModelSelection, mode: RuntimeMode) {
        let reply = self
            .command(
                id,
                Command::Create {
                    thread: id.clone(),
                    project: "project".into(),
                    title: "Thread".into(),
                    selection,
                    runtime_mode: mode,
                    interaction_mode: InteractionMode::Default,
                    workspace: None,
                },
            )
            .await;
        assert_eq!(reply, Reply::Thread(id.clone()));
    }
    pub(crate) async fn send(&self, thread: &ThreadId, text: &str, mode: DispatchMode) -> Reply {
        let id = MessageId::new(format!(
            "message-{}",
            self.serial.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap();
        self.command(
            thread,
            Command::Send(SendMessage {
                created_by: MessageAuthor::User,
                creation_source: "web".into(),
                id,
                text: text.into(),
                attachments: vec![],
                selection: None,
                mode,
                intent: None,
                source_plan: None,
                title_seed: None,
            }),
        )
        .await
    }
    pub(crate) async fn state(&self, thread: &ThreadId) -> Arc<State> {
        self.registry.state(thread).await.unwrap()
    }
    async fn drain(&self) {
        self.worker.drain(100).await.unwrap();
    }
    /// The latest outbox row of `kind` for `thread`, as its first execution.
    async fn job(&self, thread: &ThreadId, kind: &str) -> EffectJob {
        let id = thread.clone();
        let rows = self
            .store
            .blocking(move |store| store.outbox(&id))
            .await
            .unwrap();
        let row = rows
            .into_iter()
            .rev()
            .find(|row| row.kind == kind)
            .unwrap_or_else(|| panic!("no {kind} effect"));
        EffectJob {
            effect: row.effect,
            thread: thread.clone(),
            attempt: 1,
            will_retry: true,
        }
    }
    async fn run(&self, job: EffectJob) -> Result<Option<EffectResult>, EffectError> {
        let kind = effect_kind(&job.effect.body).unwrap();
        self.handlers.get(&kind).unwrap().run(job).await
    }
    async fn attempt(&self, thread: &ThreadId) -> RunAttemptId {
        self.state(thread)
            .await
            .runs
            .last()
            .unwrap()
            .attempt
            .clone()
            .unwrap()
    }
    async fn until(&self, what: &str, check: impl AsyncFn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !check().await {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn gone(&self, thread: &ThreadId) {
        self.until("session closed", async || {
            !self
                .sessions
                .sessions()
                .iter()
                .any(|key| &key.thread == thread)
        })
        .await;
    }
    /// A Codex thread whose first turn is running on a live session.
    async fn codex_turn(&self, id: &ThreadId) -> RunAttemptId {
        self.host.respond(codex_replies);
        self.create(
            id,
            selection(Driver::Codex, "gpt-6-luna"),
            RuntimeMode::FullAccess,
        )
        .await;
        self.send(id, "hello", DispatchMode::StartImmediately).await;
        self.drain().await;
        let attempt = self.attempt(id).await;
        self.until("turn accepted", async || {
            self.host.last().is_some_and(|process| {
                process
                    .written()
                    .iter()
                    .any(|frame| frame["method"] == "turn/start")
            })
        })
        .await;
        attempt
    }
    fn finish_codex_turn(&self, index: usize) {
        let process = self.host.process(index);
        process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
        process.emit(json!({"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"native-turn","status":"completed"}}}));
    }
    async fn run_status(&self, thread: &ThreadId) -> RunStatus {
        self.state(thread).await.runs.last().unwrap().status
    }
    async fn until_status(&self, thread: &ThreadId, status: RunStatus) {
        self.until(&format!("run {status:?}"), async || {
            self.run_status(thread).await == status
        })
        .await;
    }
}

fn options(idle: u64, pin: u64) -> SessionOptions {
    SessionOptions {
        idle_timeout: Duration::from_millis(idle),
        max_idle_pin: Duration::from_millis(pin),
        reply_timeout: Duration::from_secs(5),
        close_grace: Duration::from_millis(200),
    }
}

#[test]
fn registered_kinds_are_the_outbox_kinds_of_the_effects() {
    let selection = selection(Driver::Codex, "gpt");
    let commands = [
        ProviderCommand::Start {
            resume_interrupted_turn: false,
            selection: selection.clone(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            text: String::new(),
            note: None,
            attachments: vec![],
            native_thread: None,
            resume_at: None,
            context: None,
        },
        ProviderCommand::Steer {
            message: MessageId::new("m").unwrap(),
            text: String::new(),
            attachments: vec![],
        },
        ProviderCommand::Interrupt {
            native_thread: None,
            native_turn: None,
        },
        ProviderCommand::Respond {
            native_key: "k".into(),
            decision: None,
            answers: None,
            input: None,
        },
        ProviderCommand::Compact {
            native_thread: None,
        },
        ProviderCommand::SetModel {
            selection: selection.clone(),
        },
        ProviderCommand::SetRuntimeMode {
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
        },
    ];
    let kinds: Vec<_> = commands
        .into_iter()
        .map(|command| effect_kind(&EffectBody::Provider(command)).unwrap())
        .collect();
    assert_eq!(kinds, PROCESS_BOUND_PROVIDER_KINDS);
    assert_eq!(
        effect_kind(&EffectBody::ForkNative {
            command: CommandId::new("c").unwrap(),
            provider: ProviderCommand::Fork {
                native_thread: "t".into(),
                through_turn: None,
            },
        })
        .unwrap(),
        FORK_NATIVE_KIND
    );
    assert_eq!(
        effect_kind(&EffectBody::DetachSessions {
            reason: String::new(),
            revoke_credentials: false,
        })
        .unwrap(),
        DETACH_SESSIONS_KIND
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_attempts_skip_provider_work_except_stops_and_steers() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-skip");
    let attempt = rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    let state = rig.state(&id).await;
    let handler = rig.handlers.get("Provider.Respond").unwrap();
    let effect = |command| Effect {
        id: "effect".into(),
        attempt: Some(attempt.clone()),
        body: EffectBody::Provider(command),
    };
    assert!(!handler.should_run(
        &state,
        &effect(ProviderCommand::Respond {
            native_key: "1".into(),
            decision: None,
            answers: None,
            input: None,
        })
    ));
    assert!(handler.should_run(
        &state,
        &effect(ProviderCommand::Interrupt {
            native_thread: None,
            native_turn: None,
        })
    ));
    assert!(handler.should_run(
        &state,
        &effect(ProviderCommand::Steer {
            message: MessageId::new("m").unwrap(),
            text: "more".into(),
            attachments: vec![],
        })
    ));
}

// ProviderSessionManager.test.ts: "opens independent sessions concurrently".
#[tokio::test(flavor = "multi_thread")]
async fn opens_independent_sessions_concurrently() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(codex_replies);
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    rig.host.before_spawn(move |_| {
        let gate = hook.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(())
        })
    });
    let (a, b) = (thread("thread-concurrent-a"), thread("thread-concurrent-b"));
    for id in [&a, &b] {
        rig.create(id, selection(Driver::Codex, "gpt"), RuntimeMode::FullAccess)
            .await;
        rig.send(id, "hello", DispatchMode::StartImmediately).await;
    }
    let (first, second) = (
        rig.job(&a, "Provider.Start").await,
        rig.job(&b, "Provider.Start").await,
    );
    let opening = tokio::spawn({
        let handler = rig.handlers.get("Provider.Start").unwrap().clone();
        async move { tokio::join!(handler.run(first), handler.run(second)) }
    });
    tokio::time::timeout(Duration::from_secs(5), gate.until_arrived(2))
        .await
        .expect("both opens start before either finishes");
    gate.release();
    let (first, second) = opening.await.unwrap();
    assert_eq!((first.unwrap(), second.unwrap()), (None, None));
    assert_eq!(rig.host.spawned(), 2);
    assert_eq!(rig.sessions.sessions().len(), 2);
}

// "opens a duplicate session only once".
#[tokio::test(flavor = "multi_thread")]
async fn opens_a_duplicate_session_only_once() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(codex_replies);
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    rig.host.before_spawn(move |_| {
        let gate = hook.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(())
        })
    });
    let id = thread("thread-single-flight");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "hello", DispatchMode::StartImmediately).await;
    let job = rig.job(&id, "Provider.Start").await;
    let opening = tokio::spawn({
        let handler = rig.handlers.get("Provider.Start").unwrap().clone();
        let again = job.clone();
        async move { tokio::join!(handler.run(job), handler.run(again)) }
    });
    gate.until_arrived(1).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(*gate.arrived.lock().unwrap(), 1);
    gate.release();
    let (first, second) = opening.await.unwrap();
    assert!(first.is_ok() && second.is_ok());
    assert_eq!(rig.host.spawned(), 1);
}

// "closes every live session for a provider instance".
#[tokio::test(flavor = "multi_thread")]
async fn closes_every_live_session_for_a_provider_instance() {
    let rig = rig(SessionOptions::default(), 5);
    let (a, b) = (thread("thread-logout-a"), thread("thread-logout-b"));
    rig.codex_turn(&a).await;
    rig.codex_turn(&b).await;
    rig.sessions.close_instance("codex").await;
    assert!(rig.sessions.sessions().is_empty());
    assert!(rig.host.process(0).exited() && rig.host.process(1).exited());
    let released: Vec<_> = rig
        .host
        .logged()
        .into_iter()
        .filter(|entry| entry.starts_with("released:"))
        .collect();
    assert_eq!(released.len(), 2);
}

// "releases live sessions when its layer shuts down": recovery, not the
// session manager, decides what the runs become.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_releases_live_sessions_without_reporting_them_closed() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-shutdown");
    rig.codex_turn(&id).await;
    let before = rig.state(&id).await;
    rig.sessions.shutdown().await;
    assert!(rig.sessions.sessions().is_empty());
    assert!(rig.host.process(0).exited());
    assert_eq!(rig.state(&id).await, before);
}

// "drains subscribers when the provider stops".
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_provider_commits_its_last_frames_then_releases_the_session() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-provider-stop");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.host.process(0).exit(true);
    rig.gone(&id).await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Completed);
    assert!(rig.host.logged().contains(&format!("released:{id}:false")));
}

// "issues MCP credentials before opening and revokes them on close" and
// "terminal detach revokes the thread's MCP credential".
#[tokio::test(flavor = "multi_thread")]
async fn configures_before_spawning_and_revokes_credentials_on_detach() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-credentials");
    rig.codex_turn(&id).await;
    let log = rig.host.logged();
    assert_eq!(&log[..2], [format!("context:{id}"), format!("spawn:{id}")]);
    rig.sessions.detach(&id, true).await;
    assert!(rig.sessions.sessions().is_empty());
    assert!(rig.host.logged().contains(&format!("released:{id}:true")));
}

// "releases idle sessions without sweeping all sessions" and "keeps active
// sessions alive until the provider turn terminates".
#[tokio::test(flavor = "multi_thread")]
async fn releases_an_idle_session_only_after_its_turn_terminates() {
    let rig = rig(options(150, 60_000), 5);
    let (id, other) = (thread("thread-idle"), thread("thread-busy"));
    rig.codex_turn(&id).await;
    rig.codex_turn(&other).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(rig.sessions.sessions().len(), 2);
    rig.finish_codex_turn(0);
    rig.gone(&id).await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Completed);
    assert!(rig.host.process(0).exited());
    assert_eq!(
        rig.sessions.sessions(),
        [SessionKey {
            thread: other.clone(),
            instance: "codex".into()
        }]
    );
}

async fn background_work(
    rig: &Rig,
    id: &ThreadId,
    attempt: &RunAttemptId,
    status: Option<ItemStatus>,
) {
    rig.registry
        .get_or_load(id)
        .await
        .unwrap()
        .provider(
            attempt.clone(),
            ProviderEvent::BackgroundTask {
                key: "background".into(),
                tool: "background".into(),
                kind: BackgroundKind::Command,
                description: "sleep 60".into(),
                status,
                summary: None,
                exit_code: None,
            },
        )
        .await
        .unwrap();
}

// "defers idle release while background work is pending".
#[tokio::test(flavor = "multi_thread")]
async fn defers_idle_release_while_background_work_is_pending() {
    let rig = rig(options(100, 60_000), 5);
    let id = thread("thread-background");
    let attempt = rig.codex_turn(&id).await;
    background_work(&rig, &id, &attempt, None).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    assert!(!rig.state(&id).await.background_work.is_empty());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(rig.sessions.sessions().len(), 1);
    background_work(&rig, &id, &attempt, Some(ItemStatus::Completed)).await;
    assert!(rig.state(&id).await.background_work.is_empty());
    rig.gone(&id).await;
}

// "releases pinned idle sessions once the pin cap expires".
#[tokio::test(flavor = "multi_thread")]
async fn releases_a_pinned_idle_session_once_the_pin_cap_expires() {
    let rig = rig(options(100, 400), 5);
    let id = thread("thread-pin-cap");
    let attempt = rig.codex_turn(&id).await;
    background_work(&rig, &id, &attempt, None).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    let pinned = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(rig.sessions.sessions().len(), 1);
    rig.gone(&id).await;
    assert!(pinned.elapsed() >= Duration::from_millis(400));
}

// "does not apply a stale idle pin to a replacement session": the replacement
// has its own task, pin and generation.
#[tokio::test(flavor = "multi_thread")]
async fn a_replacement_session_keeps_its_own_lifetime() {
    let rig = rig(options(150, 60_000), 5);
    let id = thread("thread-replacement");
    let attempt = rig.codex_turn(&id).await;
    background_work(&rig, &id, &attempt, None).await;
    rig.sessions.detach(&id, false).await;
    rig.until_status(&id, RunStatus::Failed).await;
    rig.send(&id, "again", DispatchMode::StartImmediately).await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 2);
    assert_eq!(rig.sessions.sessions().len(), 1);
    rig.host.process(1).emit(json!({"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"native-turn","status":"completed"}}}));
    rig.until_status(&id, RunStatus::Completed).await;
    rig.gone(&id).await;
}

// "uses the same release path for runtime failures" and "releases sessions
// when provider event streams fail".
#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_provider_fails_its_running_turn() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-crash");
    rig.codex_turn(&id).await;
    rig.host.process(0).exit(false);
    rig.gone(&id).await;
    let state = rig.state(&id).await;
    assert_eq!(state.runs[0].status, RunStatus::Failed);
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Error { message, class, .. }
            if message == "Provider process exited" && class.as_deref() == Some("provider_error")
    )));
}

// "marks pending runtime requests non-live on release".
#[tokio::test(flavor = "multi_thread")]
async fn a_released_session_leaves_no_live_request() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-request");
    rig.codex_turn(&id).await;
    let process = rig.host.process(0);
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    process.emit(json!({"id":90,"method":"item/commandExecution/requestApproval","params":{"threadId":"native-thread","turnId":"native-turn","itemId":"command","command":"rm -rf build"}}));
    rig.until("request opened", async || {
        !rig.state(&id).await.requests.is_empty()
    })
    .await;
    process.exit(false);
    rig.gone(&id).await;
    let state = rig.state(&id).await;
    let request = &state.requests[0];
    assert_ne!(request.status, RequestStatus::Pending);
    let card = state
        .items
        .iter()
        .find(|item| matches!(&item.kind, ItemKind::ApprovalRequest { request: r } if *r == request.id))
        .unwrap();
    assert!(card.status.terminal());
}

// ProviderTurnStartService.test.ts: "terminalizes a starting run when its
// provider session cannot open".
#[tokio::test(flavor = "multi_thread")]
async fn fails_a_starting_run_when_its_provider_cannot_open_on_the_last_attempt() {
    let rig = rig(SessionOptions::default(), 1);
    rig.host.before_spawn(|_| {
        Box::pin(async { Err(io::Error::other("DESCRIPTION is not valid ACP JSON")) })
    });
    let id = thread("thread-open-failure");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "Continue", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    let state = rig.state(&id).await;
    assert_eq!(state.runs[0].status, RunStatus::Failed);
    assert_eq!(
        state.attempts[0].status,
        agent_domain::AttemptStatus::Failed
    );
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Error { message, class, .. }
            if message == "DESCRIPTION is not valid ACP JSON"
                && class.as_deref() == Some("provider_error")
    ) && item.status == ItemStatus::Failed));
}

// "leaves the run starting when a session-open failure will be retried".
#[tokio::test(flavor = "multi_thread")]
async fn leaves_the_run_starting_while_an_open_failure_will_be_retried() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host
        .before_spawn(|_| Box::pin(async { Err(io::Error::other("provider session rejected")) }));
    let id = thread("thread-open-retry");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "Continue", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Starting);
    let id_ = id.clone();
    let rows = rig
        .store
        .blocking(move |store| store.outbox(&id_))
        .await
        .unwrap();
    let start = rows
        .iter()
        .find(|row| row.kind == "Provider.Start")
        .unwrap();
    assert_eq!(
        (start.status, start.attempts),
        (crate::EffectStatus::Pending, 1)
    );
}

// "does not overwrite a run interrupted while its provider session opens".
#[tokio::test(flavor = "multi_thread")]
async fn a_late_open_failure_does_not_overwrite_a_stopped_run() {
    let rig = rig(SessionOptions::default(), 1);
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    rig.host.before_spawn(move |_| {
        let gate = hook.clone();
        Box::pin(async move {
            gate.pass().await;
            Err(io::Error::other("provider session rejected"))
        })
    });
    let id = thread("thread-open-stopped");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "Continue", DispatchMode::StartImmediately)
        .await;
    let draining = tokio::spawn({
        let worker = rig.worker.clone();
        async move { worker.drain(100).await.unwrap() }
    });
    gate.until_arrived(1).await;
    rig.command(&id, Command::Stop).await;
    let stopped = rig.state(&id).await.runs[0].status;
    gate.release();
    draining.await.unwrap();
    rig.drain().await;
    let state = rig.state(&id).await;
    assert_ne!(stopped, RunStatus::Failed);
    assert_eq!(state.runs[0].status, RunStatus::Interrupted);
    assert!(
        !state
            .items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::Error { .. }))
    );
}

// "fails a starting run when its last start attempt cannot load the thread":
// a failed native resume continues the run on a fresh native session instead
// (ARCHITECTURE 判断 2026-10-06, the T3 resume fallback).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_resume_continues_the_run_on_a_fresh_native_session() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-resume-failure");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.host.process(0).exit(true);
    rig.gone(&id).await;
    rig.host.respond(|frame| {
        let id = frame["id"].clone();
        match frame["method"].as_str() {
            Some("thread/resume") => {
                vec![json!({"id":id,"error":{"code":-32600,"message":"thread not found"}})]
            }
            Some("thread/start") => {
                vec![json!({"id":id,"result":{"thread":{"id":"fresh-thread"}}})]
            }
            Some("thread/inject_items") => vec![json!({"id":id,"result":{}})],
            _ => codex_replies(frame),
        }
    });
    rig.send(&id, "again", DispatchMode::StartImmediately).await;
    rig.until("fresh start", async || {
        rig.drain().await;
        rig.host
            .process(1)
            .written()
            .iter()
            .any(|frame| frame["method"] == "turn/start")
    })
    .await;
    rig.drain().await;
    let methods: Vec<_> = rig
        .host
        .process(1)
        .written()
        .iter()
        .map(|frame| frame["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "thread/resume",
            "thread/start",
            "thread/inject_items",
            "turn/start"
        ]
    );
    let state = rig.state(&id).await;
    let run = state.runs.last().unwrap();
    assert!(run.status.blocking());
    assert_eq!(
        state
            .attempts
            .iter()
            .filter(|attempt| attempt.run == run.id)
            .map(|attempt| attempt.status)
            .collect::<Vec<_>>(),
        [
            agent_domain::AttemptStatus::Failed,
            agent_domain::AttemptStatus::Pending
        ]
    );
    assert_eq!(
        state.native_sessions.get("codex").map(String::as_str),
        Some("fresh-thread")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compact_on_a_fresh_process_resumes_the_saved_native_thread() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-compact-restart");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.host.process(0).exit(true);
    rig.gone(&id).await;
    rig.command(&id, Command::Compact).await;
    rig.until("compact sent", async || {
        rig.drain().await;
        rig.host.spawned() > 1
            && rig
                .host
                .process(1)
                .written()
                .iter()
                .any(|frame| frame["method"] == "thread/compact/start")
    })
    .await;
    let written = rig.host.process(1).written();
    let methods: Vec<_> = written
        .iter()
        .map(|frame| frame["method"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "thread/resume",
            "thread/compact/start"
        ]
    );
    assert_eq!(written[2]["params"]["threadId"], "native-thread");
    assert_eq!(written[3]["params"]["threadId"], "native-thread");
}

// ProviderTurnControlService.test.ts: "interrupts the historical session only
// for the exact committed restart replacement" — a stop reaches only the
// process that ran its attempt.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_reaches_only_the_process_that_ran_its_attempt() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-historical");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let first = rig.attempt(&id).await;
    rig.host.process(0).exit(true);
    rig.gone(&id).await;
    rig.until_status(&id, RunStatus::Failed).await;
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    let second = rig.attempt(&id).await;
    let interrupt = |attempt: &RunAttemptId| EffectJob {
        effect: Effect {
            id: format!("interrupt-{attempt}"),
            attempt: Some(attempt.clone()),
            body: EffectBody::Provider(ProviderCommand::Interrupt {
                native_thread: None,
                native_turn: None,
            }),
        },
        thread: id.clone(),
        attempt: 1,
        will_retry: true,
    };
    let interrupts = |index| {
        rig.host
            .process(index)
            .written()
            .iter()
            .filter(|frame| frame["request"]["subtype"] == "interrupt")
            .count()
    };
    assert_eq!(rig.run(interrupt(&first)).await.unwrap(), None);
    assert_eq!((interrupts(0), interrupts(1)), (0, 0));
    assert_eq!(rig.run(interrupt(&second)).await.unwrap(), None);
    assert_eq!((interrupts(0), interrupts(1)), (0, 1));
}

/// A provider event's facts are committed before the frame that follows it is written.
#[tokio::test(flavor = "multi_thread")]
async fn commits_a_reply_before_writing_the_next_frame() {
    let rig = rig(SessionOptions::default(), 5);
    let observed = Arc::new(Mutex::new(None));
    let (store, seen) = (rig.store.clone(), observed.clone());
    rig.host.respond(move |frame| {
        if frame["method"] == "turn/start" {
            let bound: bool = store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM facts WHERE payload LIKE '%native-thread%')",
                        [],
                        |row| row.get(0),
                    )?)
                })
                .unwrap();
            *seen.lock().unwrap() = Some(bound);
        }
        codex_replies(frame)
    });
    let id = thread("thread-ordering");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "hello", DispatchMode::StartImmediately).await;
    rig.drain().await;
    rig.until("turn/start written", async || {
        observed.lock().unwrap().is_some()
    })
    .await;
    assert_eq!(*observed.lock().unwrap(), Some(true));
}

/// A frame's events reach the owning actor in order, and a stale process's
/// frames are never read once it is replaced.
#[tokio::test(flavor = "multi_thread")]
async fn claude_reuses_its_process_after_aligning_model_and_mode() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-reuse");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::ApprovalRequired,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let process = rig.host.process(0);
    let request = process.request.claude.clone().unwrap();
    let session = request.new_session.clone().unwrap();
    assert_eq!(request.native_session, None);
    let prompt = process.written()[1]["uuid"].as_str().unwrap().to_owned();
    process.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    process.emit(json!({"type":"user","uuid":prompt,"session_id":session,"message":{"role":"user","content":"first"}}));
    process.emit(json!({"type":"assistant","uuid":"a-1","session_id":session,"message":{"id":"m-1","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"done"}]}}));
    process.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
    rig.until_status(&id, RunStatus::Completed).await;
    rig.command(
        &id,
        Command::RuntimeMode {
            mode: RuntimeMode::AutoAcceptEdits,
        },
    )
    .await;
    rig.command(
        &id,
        Command::SelectModel {
            selection: selection(Driver::Claude, "claude-opus-4-6"),
        },
    )
    .await;
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 1);
    let kinds: Vec<_> = process.written()[2..]
        .iter()
        .map(|frame| {
            frame["request"]["subtype"]
                .as_str()
                .or(frame["type"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(kinds, ["set_model", "set_permission_mode", "user"]);
    assert_eq!(
        process.written()[2]["request"]["model"],
        "claude-opus-4-6[1m]"
    );
    assert_eq!(process.written()[3]["request"]["mode"], "acceptEdits");
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_respawns_when_launch_flags_change_and_ignores_the_old_process() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-respawn");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let old = rig.host.process(0);
    let session = old.request.claude.clone().unwrap().new_session.unwrap();
    old.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    old.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
    rig.until_status(&id, RunStatus::Completed).await;
    rig.command(
        &id,
        Command::RuntimeMode {
            mode: RuntimeMode::ApprovalRequired,
        },
    )
    .await;
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 2);
    assert!(old.exited());
    let launch = rig.host.process(1).request.claude.clone().unwrap();
    assert_eq!(launch.native_session.as_deref(), Some(session.as_str()));
    assert!(launch.policy.install_permission_callback);
    let before = rig.state(&id).await;
    old.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"stale","session_id":session,"uuid":"r-2"}));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(rig.state(&id).await.runs, before.runs);
}

// A resume the CLI cannot perform reports the lost session; the domain retries
// the run on a fresh native session.
#[tokio::test(flavor = "multi_thread")]
async fn claude_reports_a_lost_session_when_its_resume_fails() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-lost");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let first = rig.host.process(0);
    let session = first.request.claude.clone().unwrap().new_session.unwrap();
    first.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    first.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
    rig.until_status(&id, RunStatus::Completed).await;
    first.exit(true);
    rig.gone(&id).await;
    rig.host.respond(|frame| {
        if frame["request"]["subtype"] == "initialize" {
            vec![]
        } else {
            claude_replies(frame)
        }
    });
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    let draining = tokio::spawn({
        let worker = rig.worker.clone();
        async move { worker.drain(100).await.unwrap() }
    });
    rig.until("resume spawned", async || rig.host.spawned() == 2)
        .await;
    let resumed = rig.host.process(1);
    assert_eq!(
        resumed
            .request
            .claude
            .clone()
            .unwrap()
            .native_session
            .as_deref(),
        Some(session.as_str())
    );
    rig.until("initialize sent", async || !resumed.written().is_empty())
        .await;
    rig.host.respond(claude_replies);
    resumed.exit(false);
    draining.await.unwrap();
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 3);
    let fresh = rig.host.process(2).request.claude.clone().unwrap();
    assert_eq!(fresh.native_session, None);
    assert!(fresh.new_session.is_some());
    let state = rig.state(&id).await;
    assert!(state.runs.last().unwrap().status.blocking());
}

// A stop whose process is already gone ends the attempt instead of waiting forever.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_without_a_live_session_closes_the_attempt() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-stop-without-session");
    rig.codex_turn(&id).await;
    rig.sessions.shutdown().await;
    rig.command(&id, Command::Stop).await;
    rig.drain().await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Interrupted);
}

// A steer that arrives after its turn completed becomes the next turn.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_steer_becomes_a_follow_up_turn() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-late-steer");
    rig.codex_turn(&id).await;
    rig.host.process(0).emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    rig.until_status(&id, RunStatus::Running).await;
    let active = rig.state(&id).await.runs[0].id.clone();
    let reply = rig
        .send(&id, "and also", DispatchMode::SteerActive { run: active })
        .await;
    assert!(!matches!(reply, Reply::Rejected { .. }), "{reply:?}");
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.drain().await;
    let state = rig.state(&id).await;
    assert_eq!(state.runs.len(), 2);
    assert!(state.runs[1].status.blocking());
    let turns = rig
        .host
        .process(0)
        .written()
        .iter()
        .filter(|frame| frame["method"] == "turn/start")
        .count();
    assert_eq!(turns, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_native_fork_binds_the_child_to_the_forked_thread() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-fork-source");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.host.respond(|frame| {
        if frame["method"] == "thread/fork" {
            return vec![json!({"id":frame["id"],"result":{"thread":{"id":"forked-thread"}}})];
        }
        codex_replies(frame)
    });
    let run: RunId = rig.state(&id).await.runs[0].id.clone();
    let child = thread("thread-fork-child");
    let reply = rig
        .command(
            &id,
            Command::Fork {
                target: child.clone(),
                through_run: run,
                title: None,
            },
        )
        .await;
    assert_eq!(reply, Reply::Thread(child.clone()));
    rig.drain().await;
    let fork = rig
        .host
        .process(0)
        .written()
        .into_iter()
        .find(|frame| frame["method"] == "thread/fork")
        .unwrap();
    assert_eq!(fork["params"]["threadId"], "native-thread");
    let state = rig.state(&child).await;
    assert_eq!(
        state.native_sessions.get("codex").map(String::as_str),
        Some("forked-thread")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_codex_fork_still_creates_the_child_with_portable_context() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-fork-rejected");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.host.respond(|frame| {
        if frame["method"] == "thread/fork" {
            return vec![
                json!({"id":frame["id"],"error":{"code":-32600,"message":"fork unsupported"}}),
            ];
        }
        codex_replies(frame)
    });
    let run = rig.state(&id).await.runs[0].id.clone();
    let child = thread("thread-fork-rejected-child");
    rig.command(
        &id,
        Command::Fork {
            target: child.clone(),
            through_run: run,
            title: None,
        },
    )
    .await;
    rig.drain().await;
    let state = rig.state(&child).await;
    assert!(state.thread.is_some());
    assert!(state.native_sessions.is_empty());
    assert!(rig.state(&id).await.pending_forks.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_native_fork_copies_the_transcript_through_the_head() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-fork");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let process = rig.host.process(0);
    let session = process.request.claude.clone().unwrap().new_session.unwrap();
    let prompt = process.written()[1]["uuid"].as_str().unwrap().to_owned();
    process.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    process.emit(json!({"type":"user","uuid":prompt,"session_id":session,"message":{"role":"user","content":"first"}}));
    process.emit(json!({"type":"assistant","uuid":"a-1","session_id":session,"message":{"id":"m-1","role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"done"}]}}));
    process.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
    rig.until_status(&id, RunStatus::Completed).await;
    let lines = [
        json!({"type":"user","uuid":prompt,"parentUuid":null,"sessionId":session,"message":{"role":"user","content":"first"},"timestamp":"2026-10-06T00:00:00Z"}),
        json!({"type":"assistant","uuid":"a-1","parentUuid":prompt,"sessionId":session,"message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"timestamp":"2026-10-06T00:00:01Z"}),
    ];
    rig.host.transcripts.lock().unwrap().insert(
        session.clone(),
        lines.iter().map(|line| format!("{line}\n")).collect(),
    );
    let run = rig.state(&id).await.runs[0].id.clone();
    let child = thread("thread-claude-fork-child");
    rig.command(
        &id,
        Command::Fork {
            target: child.clone(),
            through_run: run,
            title: None,
        },
    )
    .await;
    rig.drain().await;
    let forked = rig
        .state(&child)
        .await
        .native_sessions
        .get("claude")
        .cloned();
    let forked = forked.expect("the child is bound to the forked session");
    assert_ne!(forked, session);
    let copy = rig.host.transcripts.lock().unwrap()[&forked].clone();
    assert!(copy.contains("\"a-1\"") || copy.contains("done"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_rollback_reverts_after_the_absolute_head() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-rollback");
    rig.codex_turn(&id).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.host.respond(|frame| {
        let id = frame["id"].clone();
        match frame["method"].as_str() {
            Some("thread/read") => vec![json!({"id":id,"result":{"thread":{"historyMode":"paginated","status":{"type":"idle"}}}})],
            Some("thread/turns/list") => vec![json!({"id":id,"result":{"data":[{"id":"turn-2"},{"id":"turn-1"}]}})],
            Some("thread/revert") => vec![json!({"id":id,"result":{}})],
            _ => codex_replies(frame),
        }
    });
    let binding = rig
        .sessions
        .rollback(
            &id,
            "codex",
            &ProviderCommand::Rollback {
                native_thread: "native-thread".into(),
                absolute_head: Some("turn-1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        binding,
        Some(NativeBinding {
            instance: "codex".into(),
            thread: "native-thread".into(),
            head: Some("turn-1".into()),
        })
    );
    let revert = rig
        .host
        .process(0)
        .written()
        .into_iter()
        .find(|frame| frame["method"] == "thread/revert")
        .unwrap();
    assert_eq!(revert["params"]["beforeTurnId"], "turn-2");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_rollback_closes_the_process_for_a_resume_at_the_head() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-rollback");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let rollback = |head: Option<&str>| ProviderCommand::Rollback {
        native_thread: "session".into(),
        absolute_head: head.map(str::to_owned),
    };
    let bound = rig
        .sessions
        .rollback(&id, "claude", &rollback(Some("a-1")))
        .await
        .unwrap();
    assert!(rig.host.process(0).exited());
    assert!(rig.sessions.sessions().is_empty());
    assert_eq!(
        bound,
        Some(NativeBinding {
            instance: "claude".into(),
            thread: "session".into(),
            head: Some("a-1".into()),
        })
    );
    assert_eq!(
        rig.sessions
            .rollback(&id, "claude", &rollback(None))
            .await
            .unwrap(),
        None
    );
}
