//! Ports of T3 ProviderSessionManager, ProviderTurnStartService and
//! ProviderTurnControlService behavior tests, plus the frame ordering rule.
pub(crate) mod fake;
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
    rig_handlers(options, max_attempts, |sessions, registry, _| {
        with_session_handlers(EffectHandlers::default(), sessions)
            .with("SendToThread", Arc::new(Forward(registry.clone())))
    })
}

/// A rig whose every effect runs through the runtime executors with fake Host
/// operations, as the Host runs them.
pub(crate) fn runtime_rig(options: SessionOptions, max_attempts: u32) -> Rig {
    rig_handlers(options, max_attempts, |sessions, registry, store| {
        crate::with_runtime_handlers(
            EffectHandlers::default(),
            &crate::ExecutorContext {
                store: store.clone(),
                registry: registry.clone(),
                sessions: sessions.clone(),
                ops: Arc::new(crate::executor::tests::Ops(
                    crate::executor::tests::FakeOps::new(),
                )),
                workspaces: Arc::default(),
            },
        )
    })
}

fn rig_handlers(
    options: SessionOptions,
    max_attempts: u32,
    handlers: impl FnOnce(&Arc<SessionManager>, &Arc<ActorRegistry>, &Store) -> EffectHandlers,
) -> Rig {
    let (dir, store) = temp_store();
    let clock = Arc::new(ManualClock::new(&at()));
    let live = LiveSessions::default();
    let mut context = ActorContext::new(store.clone());
    context.clock = clock.clone();
    context.residency = Arc::new(live.clone());
    let registry = ActorRegistry::new(context);
    let host = FakeHost::new();
    let sessions = SessionManager::new(registry.clone(), host.clone(), options, live);
    let handlers = handlers(&sessions, &registry, &store);
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

pub(crate) fn codex_replies(frame: &Value) -> Vec<Value> {
    let id = frame["id"].clone();
    match frame["method"].as_str() {
        Some("initialize") => vec![json!({"id":id,"result":{}})],
        // A process's first thread is `native-thread`; later threads of a
        // shared app-server are told apart by their request id.
        Some("thread/start") if id == 2 => {
            vec![json!({"id":id,"result":{"thread":{"id":"native-thread"}}})]
        }
        Some("thread/start") => {
            vec![json!({"id":id,"result":{"thread":{"id":format!("native-thread-{id}")}}})]
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
    if frame["type"] == "control_request" {
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
                context: None,
                created_by: MessageAuthor::User,
                creation_source: "web".into(),
                id,
                text: text.into(),
                attachments: vec![],
                selection: None,
                mode,
                intent: None,
                source_plan: None,
                resolved_plan: None,
                continuation: None,
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
        self.codex_turn_on(id, "codex").await
    }
    async fn codex_turn_on(&self, id: &ThreadId, instance: &str) -> RunAttemptId {
        self.host.respond(codex_replies);
        let mut selection = selection(Driver::Codex, "gpt-6-luna");
        selection.instance = instance.into();
        self.create(id, selection, RuntimeMode::FullAccess).await;
        let started = |process: &fake::FakeProcess| {
            process
                .written()
                .iter()
                .filter(|frame| frame["method"] == "turn/start")
                .count()
        };
        let before = self.host.last().map_or(0, |process| started(&process));
        let spawned = self.host.spawned();
        self.send(id, "hello", DispatchMode::StartImmediately).await;
        self.drain().await;
        let attempt = self.attempt(id).await;
        self.until("turn accepted", async || {
            self.host.last().is_some_and(|process| {
                started(&process)
                    > if self.host.spawned() == spawned {
                        before
                    } else {
                        0
                    }
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

/// A live Claude process of `thread` opened in `cwd`, for tests outside the
/// session module.
pub(crate) async fn live_claude_process(sessions: &SessionManager, thread: &ThreadId, cwd: &str) {
    let target = LaunchTarget {
        key: SessionKey {
            thread: thread.clone(),
            instance: "claude".into(),
        },
        selection: selection(Driver::Claude, "claude-sonnet-4-6"),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace: Some(agent_domain::Workspace {
            cwd: cwd.into(),
            worktree_path: Some(cwd.into()),
            branch: None,
        }),
    };
    let launch = claude::claude_launch(&target, &ClaudeSettings::default(), None, None, None);
    let Ok(_) = sessions.spawn(&target, Some(launch)).await else {
        panic!("the fake Claude process opens");
    };
}

fn options(idle: u64, pin: u64) -> SessionOptions {
    SessionOptions {
        idle_timeout: Duration::from_millis(idle),
        max_idle_pin: Duration::from_millis(pin),
        reply_timeout: Duration::from_secs(5),
        interrupt_timeout: Duration::from_secs(5),
        close_grace: Duration::from_millis(200),
        write_timeout: Duration::from_secs(5),
    }
}

#[test]
fn control_failures_name_their_own_provider_operation() {
    use task::failed_operation;
    assert_eq!(
        failed_operation("set_permission_mode"),
        Some((ProviderOperation::Start, false))
    );
    assert_eq!(
        failed_operation("thread/resume"),
        Some((ProviderOperation::Start, true))
    );
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
    ];
    let kinds: Vec<_> = commands
        .into_iter()
        .map(|command| effect_kind(&EffectBody::Provider(command)).unwrap())
        .collect();
    assert_eq!(kinds, PROCESS_BOUND_PROVIDER_KINDS);
    assert_eq!(
        effect_kind(&EffectBody::ForkNative {
            instance: "codex".into(),
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
            instance: None,
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
    for (id, instance) in [(&a, "codex"), (&b, "codex-work")] {
        let mut selection = selection(Driver::Codex, "gpt");
        selection.instance = instance.into();
        rig.create(id, selection, RuntimeMode::FullAccess).await;
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

// "closes every live session for a provider instance": both threads share the
// instance's app-server, and closing it releases both credentials.
#[tokio::test(flavor = "multi_thread")]
async fn closes_every_live_session_for_a_provider_instance() {
    let rig = rig(SessionOptions::default(), 5);
    let (a, b) = (thread("thread-logout-a"), thread("thread-logout-b"));
    rig.codex_turn(&a).await;
    rig.codex_turn(&b).await;
    assert_eq!(rig.host.spawned(), 1);
    assert_eq!(rig.sessions.sessions().len(), 2);
    rig.sessions.close_instance("codex").await;
    assert!(rig.sessions.sessions().is_empty());
    assert!(rig.host.process(0).exited());
    let mut revoked: Vec<_> = rig
        .host
        .logged()
        .into_iter()
        .filter(|entry| entry.starts_with("revoked:"))
        .collect();
    revoked.sort();
    assert_eq!(
        revoked,
        [format!("revoked:{a}:codex"), format!("revoked:{b}:codex")]
    );
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
    assert!(rig.host.logged().contains(&format!("revoked:{id}:codex")));
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
    assert!(rig.host.logged().contains(&format!("revoked:{id}:*")));
}

// "terminal detach revokes credentials even when no session is live" (#18).
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_detach_revokes_credentials_without_a_live_process() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-credentials-idle");
    rig.sessions.detach(&id, true).await;
    assert_eq!(rig.host.logged(), [format!("revoked:{id}:*")]);
    rig.sessions.detach(&id, false).await;
    assert_eq!(rig.host.logged().len(), 1);
}

// "releases idle sessions without sweeping all sessions" and "keeps active
// sessions alive until the provider turn terminates".
#[tokio::test(flavor = "multi_thread")]
async fn releases_an_idle_session_only_after_its_turn_terminates() {
    let rig = rig(options(150, 60_000), 5);
    let (id, other) = (thread("thread-idle"), thread("thread-busy"));
    rig.codex_turn(&id).await;
    rig.codex_turn_on(&other, "codex-work").await;
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
            instance: "codex-work".into()
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
    rig.sessions.close_instance("codex").await;
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
    assert_eq!(request.status, RequestStatus::Expired);
    assert_eq!(
        request.capability,
        agent_domain::ResponseCapability::NotResumable
    );
    assert_eq!(card.status, ItemStatus::Failed);
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

// T3 ClaudeAdapterV2 openQuery: a live query is reused only for the same
// policy and selection; a new model or mode replaces it with a resume.
#[tokio::test(flavor = "multi_thread")]
async fn claude_replaces_its_process_for_another_model_or_mode() {
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
    // Runtime mode changes detach Claude (T3), so this changes the
    // interaction mode; both apply when the next turn starts.
    rig.command(
        &id,
        Command::InteractionMode {
            mode: agent_domain::InteractionMode::Plan,
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
    assert!(!process.exited());
    let before = process.written().len();
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 2);
    assert!(process.exited());
    assert!(
        claude_kinds(&process.written()[before..])
            .iter()
            .all(|kind| kind != "user")
    );
    let replacement = rig.host.process(1);
    let launch = replacement.request.claude.clone().unwrap();
    assert_eq!(launch.native_session.as_deref(), Some(session.as_str()));
    assert_eq!(launch.model, "claude-opus-4-6[1m]");
    assert_eq!(launch.policy.permission_mode, "plan");
    assert_eq!(claude_kinds(&replacement.written()), ["initialize", "user"]);
}

/// A rejection settles the waiter of its own request id, not another request
/// of the same operation.
#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_request_settles_only_the_waiter_of_its_id() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(|frame| {
        if frame["request"]["subtype"] == "set_permission_mode" {
            return vec![];
        }
        claude_replies(frame)
    });
    let id = thread("thread-reply-ids");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let entry = rig
        .sessions
        .entry(&Slot::Thread(rig.sessions.sessions()[0].clone()))
        .unwrap();
    let owner = id.clone();
    let set_mode = move |mode: &'static str| {
        Request::new(&owner, move |p| {
            Ok(frames(vec![
                p.claude()?
                    .control
                    .request("set_permission_mode", json!({ "mode": mode })),
            ]))
        })
    };
    rig.sessions
        .send(&entry, set_mode("default"))
        .await
        .unwrap();
    let awaited = tokio::spawn({
        let (sessions, entry) = (rig.sessions.clone(), entry.clone());
        async move { sessions.request_reply(&entry, set_mode("plan")).await }
    });
    let process = rig.host.process(0);
    let requests = || {
        process
            .written()
            .into_iter()
            .filter(|frame| frame["request"]["subtype"] == "set_permission_mode")
            .map(|frame| frame["request_id"].clone())
            .collect::<Vec<_>>()
    };
    rig.until("both requests written", async || requests().len() == 2)
        .await;
    let ids = requests();
    process.emit(json!({"type":"control_response","response":{"subtype":"error","request_id":ids[0],"error":"mode rejected"}}));
    process.emit(json!({"type":"control_response","response":{"subtype":"success","request_id":ids[1],"response":{"mode":"plan"}}}));
    assert_eq!(awaited.await.unwrap(), Ok(json!({"mode":"plan"})));
    rig.until("rejection recorded on the run", async || {
        rig.state(&id).await.items.iter().any(|item| {
            matches!(&item.kind, ItemKind::Error { message, .. } if message == "mode rejected")
        })
    })
    .await;
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
    // T3 detaches the Claude session when the runtime mode changes.
    rig.command(
        &id,
        Command::RuntimeMode {
            mode: RuntimeMode::ApprovalRequired,
        },
    )
    .await;
    rig.drain().await;
    assert!(old.exited());
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 2);
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
                source: agent_domain::SourcePoint::Run(run),
                title: None,
            },
        )
        .await;
    assert_eq!(reply, Reply::Thread(child.clone()));
    rig.drain().await;
    // T3 forks natively when the child sends its first message.
    rig.send(&child, "child", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    let fork = (0..rig.host.spawned())
        .flat_map(|index| rig.host.process(index).written())
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
async fn a_rejected_codex_fork_fails_the_childs_first_run() {
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
            source: agent_domain::SourcePoint::Run(run),
            title: None,
        },
    )
    .await;
    rig.drain().await;
    assert!(rig.state(&child).await.thread.is_some());
    rig.send(&child, "child", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    // T3 ProviderTurnStartService.ts: the failed fork fails the run.
    let state = rig.state(&child).await;
    assert_eq!(state.runs[0].status, RunStatus::Failed);
    assert!(state.native_sessions.is_empty());
    assert!(state.transfers[0].delivery.is_none());
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
            source: agent_domain::SourcePoint::Run(run),
            title: None,
        },
    )
    .await;
    rig.drain().await;
    rig.send(&child, "child", DispatchMode::StartImmediately)
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
    // T3 forkThread closes the source's live query before reading its transcript.
    assert!(process.exited());
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

fn written_methods(process: &fake::FakeProcess, method: &str) -> Vec<Value> {
    process
        .written()
        .into_iter()
        .filter(|frame| frame["method"] == method)
        .collect()
}

fn claude_kinds(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .map(|frame| {
            frame["request"]["subtype"]
                .as_str()
                .or(frame["type"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect()
}

impl Rig {
    /// A Claude thread whose first turn completed on a live process.
    async fn claude_turn(&self, id: &ThreadId, mode: RuntimeMode) -> Arc<fake::FakeProcess> {
        self.create(id, selection(Driver::Claude, "claude-sonnet-4-6"), mode)
            .await;
        self.send(id, "first", DispatchMode::StartImmediately).await;
        self.drain().await;
        let process = self.host.process(0);
        let session = process.request.claude.clone().unwrap().new_session.unwrap();
        process
            .emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
        process.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
        self.until_status(id, RunStatus::Completed).await;
        process
    }
    async fn fail_fact_writes(&self, failing: bool) {
        self.store
            .on_writer(move |c| {
                c.execute_batch(if failing {
                    "CREATE TEMP TRIGGER fail_facts BEFORE INSERT ON facts
                     BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
                } else {
                    "DROP TRIGGER fail_facts;"
                })?;
                Ok(())
            })
            .await
            .unwrap();
    }
    fn draining(&self) -> tokio::task::JoinHandle<()> {
        let worker = self.worker.clone();
        tokio::spawn(async move {
            worker.drain(100).await.unwrap();
        })
    }
}

// ProviderTurnStartService.ts:1165: a Stop while the session opens leaves no turn to send.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_while_the_session_opens_sends_no_turn() {
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
    let id = thread("thread-stop-while-opening");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "hello", DispatchMode::StartImmediately).await;
    let draining = rig.draining();
    gate.until_arrived(1).await;
    rig.command(&id, Command::Stop).await;
    gate.release();
    draining.await.unwrap();
    rig.drain().await;
    let process = rig.host.process(0);
    assert!(written_methods(&process, "thread/start").is_empty());
    assert!(written_methods(&process, "turn/start").is_empty());
    assert_eq!(rig.run_status(&id).await, RunStatus::Interrupted);
}

// ProviderTurnControlService.ts:220 interruptAndAwaitTerminal: a restart sends
// the replacement turn only after the old one ends, and the old turn's late
// frames stay with its own attempt.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_waits_for_the_old_turn_and_keeps_its_late_replies_with_it() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-restart");
    let first = rig.codex_turn(&id).await;
    rig.host.respond(|frame| {
        if frame["method"] == "turn/interrupt" {
            return vec![];
        }
        codex_replies(frame)
    });
    let process = rig.host.process(0);
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    rig.until_status(&id, RunStatus::Running).await;
    let run = rig.state(&id).await.runs[0].id.clone();
    let reply = rig
        .send(&id, "instead", DispatchMode::RestartActive { run })
        .await;
    assert!(!matches!(reply, Reply::Rejected { .. }), "{reply:?}");
    let draining = rig.draining();
    rig.until("interrupt written", async || {
        !written_methods(&process, "turn/interrupt").is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(written_methods(&process, "turn/start").len(), 1);
    process.emit(json!({"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"native-turn","status":"interrupted"}}}));
    rig.until("replacement started", async || {
        written_methods(&process, "turn/start").len() == 2
    })
    .await;
    draining.await.unwrap();
    let second = rig.attempt(&id).await;
    assert_ne!(first, second);
    let state = rig.state(&id).await;
    assert!(
        state.runs[0].status.blocking(),
        "{:?}",
        state.runs[0].status
    );
    let interrupt = written_methods(&process, "turn/interrupt")[0]["id"].clone();
    process.emit(json!({"id":interrupt,"error":{"code":-32600,"message":"no running turn"}}));
    rig.until("rejection handled", async || process.stdout.drained())
        .await;
    let state = rig.state(&id).await;
    assert!(state.runs[0].status.blocking());
    assert!(
        !state
            .items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::Error { .. })),
        "the old interrupt's rejection reached the replacement attempt"
    );
}

// RunExecutionService.ts:427: a command retained from an earlier turn reports
// to that turn's attempt even while a later turn runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_retained_command_finishes_under_the_turn_that_started_it() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-retained");
    rig.codex_turn(&id).await;
    let process = rig.host.process(0);
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    process.emit(json!({"method":"item/started","params":{"threadId":"native-thread","turnId":"native-turn","item":{"type":"commandExecution","id":"call-bg","command":"sleep 20","processId":"4242","status":"inProgress"}}}));
    process.emit(json!({"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"native-turn","status":"completed"}}}));
    rig.until_status(&id, RunStatus::Completed).await;
    assert!(!rig.state(&id).await.background_work.is_empty());
    rig.send(&id, "next", DispatchMode::StartImmediately).await;
    rig.drain().await;
    rig.until("second turn", async || {
        written_methods(&process, "turn/start").len() == 2
    })
    .await;
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn-2"}}}));
    rig.until_status(&id, RunStatus::Running).await;
    process.emit(json!({"method":"item/completed","params":{"threadId":"native-thread","turnId":"native-turn","item":{"type":"commandExecution","id":"call-bg","command":"sleep 20","processId":"4242","status":"completed","exitCode":0,"aggregatedOutput":"done\n"}}}));
    rig.until("background work reported", async || {
        rig.state(&id).await.background_work.is_empty()
    })
    .await;
    let state = rig.state(&id).await;
    let commands: Vec<_> = state
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::CommandExecution { .. }))
        .collect();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].run.as_ref(), Some(&state.runs[0].id));
    assert_eq!(commands[0].status, ItemStatus::Completed);
    assert_eq!(state.runs[1].status, RunStatus::Running);
}

// ProviderSessionManager.ts:885: a provider that stops reading its stdin never
// stalls the task that reads its output and handles Close.
#[tokio::test(flavor = "multi_thread")]
async fn a_blocked_stdin_write_keeps_output_and_close_handled() {
    let rig = rig(
        SessionOptions {
            write_timeout: Duration::from_secs(60),
            close_grace: Duration::from_millis(200),
            ..SessionOptions::default()
        },
        5,
    );
    rig.host.respond(codex_replies);
    *rig.host.stall.lock().unwrap() = Some("turn/start");
    let id = thread("thread-stalled-stdin");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "hello", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let process = rig.host.process(0);
    rig.until("thread started", async || {
        !written_methods(&process, "thread/start").is_empty()
    })
    .await;
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    rig.until_status(&id, RunStatus::Running).await;
    tokio::time::timeout(Duration::from_secs(2), rig.sessions.close_instance("codex"))
        .await
        .expect("close is handled while a write is blocked");
    assert!(process.exited());
    assert!(rig.sessions.sessions().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_the_provider_never_accepts_fails_the_session() {
    let rig = rig(
        SessionOptions {
            write_timeout: Duration::from_millis(200),
            close_grace: Duration::from_millis(200),
            ..SessionOptions::default()
        },
        5,
    );
    rig.host.respond(codex_replies);
    *rig.host.stall.lock().unwrap() = Some("turn/start");
    let id = thread("thread-write-timeout");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "hello", DispatchMode::StartImmediately).await;
    rig.drain().await;
    rig.gone(&id).await;
    rig.until_status(&id, RunStatus::Failed).await;
    let state = rig.state(&id).await;
    assert!(
        state.items.iter().any(|item| matches!(
            &item.kind,
            ItemKind::Error { message, .. } if message == "The provider stopped reading its input."
        )),
        "{:?} {:?}",
        state.items,
        rig.host.logged()
    );
}

// ClaudeAdapterV2.ts:7297 and :7328: Stop interrupts and then closes the
// query, so the run ends even without a result frame, and background shells
// of a settled root end with the process.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_stop_closes_the_process_and_ends_the_run() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-stop");
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
    process.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    rig.until("prompt sent", async || process.stdout.drained())
        .await;
    rig.command(&id, Command::Stop).await;
    rig.drain().await;
    assert!(
        process
            .written()
            .iter()
            .any(|frame| frame["request"]["subtype"] == "interrupt")
    );
    assert!(process.exited());
    rig.gone(&id).await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Interrupted);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_stop_after_the_turn_ends_its_background_shells() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-stop-background");
    let process = rig.claude_turn(&id, RuntimeMode::FullAccess).await;
    let attempt = rig.attempt(&id).await;
    background_work(&rig, &id, &attempt, None).await;
    rig.command(&id, Command::Stop).await;
    rig.drain().await;
    assert!(process.exited());
    rig.gone(&id).await;
    assert!(rig.state(&id).await.background_work.is_empty());
}

// ProviderSessionManager.ts:763: a commit the store rejects for a while is
// retried; the translation is not dropped.
#[tokio::test(flavor = "multi_thread")]
async fn a_frame_whose_commit_fails_for_a_while_is_retried() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-commit-retry");
    rig.codex_turn(&id).await;
    rig.fail_fact_writes(true).await;
    rig.finish_codex_turn(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.run_status(&id).await, RunStatus::Starting);
    rig.fail_fact_writes(false).await;
    rig.until_status(&id, RunStatus::Completed).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_that_cannot_be_committed_fails_the_session() {
    let rig = rig(options(60_000, 60_000), 5);
    let id = thread("thread-commit-failure");
    rig.codex_turn(&id).await;
    let process = rig.host.process(0);
    rig.fail_fact_writes(true).await;
    rig.finish_codex_turn(0);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !process.exited() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the session kept running"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    rig.fail_fact_writes(false).await;
    rig.gone(&id).await;
    rig.until_status(&id, RunStatus::Failed).await;
    assert!(rig.state(&id).await.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Error { message, .. } if message.starts_with("Provider output could not be recorded")
    )));
}

// ProviderSessionManager.ts:654: closure reaches every attempt whose work the
// process still holds, not only the latest owner.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_process_ends_the_work_of_every_attempt_it_ran() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-closed-earlier-work");
    let first = rig.codex_turn(&id).await;
    background_work(&rig, &id, &first, None).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    rig.send(&id, "next", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let process = rig.host.process(0);
    rig.until("second turn", async || {
        written_methods(&process, "turn/start").len() == 2
    })
    .await;
    rig.finish_codex_turn(0);
    rig.until("second turn completed", async || {
        rig.state(&id).await.runs[1].status == RunStatus::Completed
    })
    .await;
    assert!(!rig.state(&id).await.background_work.is_empty());
    process.exit(false);
    rig.gone(&id).await;
    rig.until("earlier work ended", async || {
        rig.state(&id).await.background_work.is_empty()
    })
    .await;
}

// ProviderTurnStartService.ts:644: a compaction with no native thread starts one.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_codex_compaction_starts_a_native_thread() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(codex_replies);
    let id = thread("thread-first-compact");
    rig.create(
        &id,
        selection(Driver::Codex, "gpt"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "/compact", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    let process = rig.host.process(0);
    rig.until("compact sent", async || {
        !written_methods(&process, "thread/compact/start").is_empty()
    })
    .await;
    let methods: Vec<_> = process
        .written()
        .iter()
        .map(|frame| frame["method"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "thread/start",
            "thread/compact/start"
        ]
    );
    assert_eq!(
        rig.state(&id)
            .await
            .native_sessions
            .get("codex")
            .map(String::as_str),
        Some("native-thread")
    );
}

// T3 applies a model selection from the next turn: the running query gets no
// set_model, and the next turn on the new selection replaces the process.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_model_change_during_a_turn_applies_from_the_next_turn() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-model-next-turn");
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
    process.emit(json!({"type":"system","subtype":"init","session_id":session,"uuid":"init-1"}));
    rig.until_status(&id, RunStatus::Running).await;
    let before = process.written().len();
    rig.command(
        &id,
        Command::SelectModel {
            selection: selection(Driver::Claude, "claude-opus-4-6"),
        },
    )
    .await;
    rig.drain().await;
    assert_eq!(process.written().len(), before);
    assert!(!process.exited());
    process.emit(json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"done","session_id":session,"uuid":"r-1"}));
    rig.until_status(&id, RunStatus::Completed).await;
    let state = rig.state(&id).await;
    assert_eq!(state.runs[0].selection.model, "claude-sonnet-4-6");
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 2);
    assert!(process.exited());
    let launch = rig.host.process(1).request.claude.clone().unwrap();
    assert_eq!(launch.model, "claude-opus-4-6[1m]");
    assert_eq!(launch.native_session.as_deref(), Some(session.as_str()));
}

// ClaudeAdapterV2.ts:7063 and :6930: the CLI switches its own mode (plan mode);
// the next prompt restores the thread's mode.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_mode_the_cli_reports_is_restored_before_the_next_prompt() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-plan-mode");
    let process = rig.claude_turn(&id, RuntimeMode::FullAccess).await;
    let mode = process
        .request
        .claude
        .clone()
        .unwrap()
        .policy
        .permission_mode;
    assert_ne!(mode, "plan");
    process.emit(json!({"type":"system","subtype":"status","permissionMode":"plan","session_id":"s","uuid":"status-1"}));
    rig.until("status handled", async || process.stdout.drained())
        .await;
    let before = process.written().len();
    rig.send(&id, "second", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    let written = &process.written()[before..];
    assert_eq!(claude_kinds(written), ["set_permission_mode", "user"]);
    assert_eq!(written[0]["request"]["mode"], mode);
}

// ClaudeAdapterV2.ts:6940: a new selection that needs a new process is refused
// while the live process runs background work.
#[tokio::test(flavor = "multi_thread")]
async fn claude_keeps_a_process_with_background_work_when_the_selection_changes() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-background-blocks");
    let process = rig.claude_turn(&id, RuntimeMode::FullAccess).await;
    let attempt = rig.attempt(&id).await;
    background_work(&rig, &id, &attempt, None).await;
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
    assert!(!process.exited());
    let state = rig.state(&id).await;
    assert_eq!(state.runs[1].status, RunStatus::Failed);
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Error { message, .. } if message == claude::BACKGROUND_BLOCKS_REPLACEMENT
    )));
}

// ProviderSessionManager.ts:1023: background traffic does not postpone the pin cap.
#[tokio::test(flavor = "multi_thread")]
async fn background_traffic_does_not_extend_the_pin_cap() {
    let rig = rig(options(100, 400), 5);
    let id = thread("thread-pin-traffic");
    let attempt = rig.codex_turn(&id).await;
    background_work(&rig, &id, &attempt, None).await;
    rig.finish_codex_turn(0);
    rig.until_status(&id, RunStatus::Completed).await;
    let process = rig.host.process(0);
    let traffic = tokio::spawn({
        let process = process.clone();
        async move {
            while !process.exited() {
                process.emit(json!({"method":"account/rateLimits/updated","params":{}}));
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
    });
    rig.gone(&id).await;
    assert!(process.exited());
    traffic.await.unwrap();
}

// ProviderTurnControlService.ts:296: a steer rejection names its own message.
#[tokio::test(flavor = "multi_thread")]
async fn a_steer_rejection_promotes_the_message_of_its_own_request() {
    let rig = rig(SessionOptions::default(), 5);
    let id = thread("thread-steer-ids");
    rig.codex_turn(&id).await;
    rig.host.respond(|frame| {
        if frame["method"] == "turn/steer" {
            return vec![];
        }
        codex_replies(frame)
    });
    let process = rig.host.process(0);
    process.emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"native-turn"}}}));
    rig.until_status(&id, RunStatus::Running).await;
    let run = rig.state(&id).await.runs[0].id.clone();
    for text in ["first steer", "second steer"] {
        rig.send(&id, text, DispatchMode::SteerActive { run: run.clone() })
            .await;
        rig.drain().await;
    }
    let steers = written_methods(&process, "turn/steer");
    assert_eq!(steers.len(), 2);
    process.emit(json!({"id":steers[1]["id"],"error":{"code":-32600,"message":"turn completed"}}));
    rig.until("follow-up", async || rig.state(&id).await.runs.len() == 2)
        .await;
    let state = rig.state(&id).await;
    let message = state
        .messages
        .iter()
        .find(|message| message.id == state.runs[1].message)
        .unwrap();
    assert_eq!(message.text, "second steer");
}

// ClaudeAdapterV2.ts:2970 and :7392: skills are read again for every prompt and steer.
#[tokio::test(flavor = "multi_thread")]
async fn claude_reads_skills_again_for_a_reused_prompt_and_a_steer() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-skills");
    let process = rig.claude_turn(&id, RuntimeMode::FullAccess).await;
    rig.host.claude.lock().unwrap().skills = vec!["review".into()];
    let before = process.written().len();
    rig.send(&id, "$review now", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(rig.host.spawned(), 1);
    let prompt = process.written()[before..]
        .iter()
        .find(|frame| frame["type"] == "user")
        .cloned()
        .unwrap();
    assert_eq!(
        prompt["message"]["content"],
        json!([{"type":"text","text":"/review now"}])
    );
    rig.until_status(&id, RunStatus::Running).await;
    rig.host.claude.lock().unwrap().skills = vec!["ship".into()];
    let run = rig.state(&id).await.runs[1].id.clone();
    rig.send(&id, "$ship it", DispatchMode::SteerActive { run })
        .await;
    rig.drain().await;
    let steer = process
        .written()
        .into_iter()
        .rfind(|frame| frame["type"] == "user")
        .unwrap();
    assert_eq!(steer["priority"], "now");
    assert_eq!(
        steer["message"]["content"],
        json!([{"type":"text","text":"/ship it"}])
    );
}

// A new Claude session is bound before its prompt makes the CLI write a
// transcript, so an import cannot adopt it first.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_claude_session_is_bound_before_its_prompt_is_sent() {
    let rig = rig(SessionOptions::default(), 5);
    let observed = Arc::new(Mutex::new(None));
    let (store, seen) = (rig.store.clone(), observed.clone());
    rig.host.respond(move |frame| {
        if frame["type"] == "user" {
            let bound: Option<String> = store
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT json_extract(payload, '$.SessionBound.native_thread') FROM facts
                         WHERE kind = 'SessionBound' LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok())
                })
                .unwrap();
            *seen.lock().unwrap() = Some(bound);
        }
        claude_replies(frame)
    });
    let id = thread("thread-claude-bound-first");
    rig.create(
        &id,
        selection(Driver::Claude, "claude-sonnet-4-6"),
        RuntimeMode::FullAccess,
    )
    .await;
    rig.send(&id, "first", DispatchMode::StartImmediately).await;
    rig.drain().await;
    let session = rig
        .host
        .process(0)
        .request
        .claude
        .clone()
        .unwrap()
        .new_session;
    assert!(session.is_some());
    assert_eq!(*observed.lock().unwrap(), Some(session));
}

// The forked Claude session is reserved by the consuming child thread before
// its transcript is written.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_fork_reserves_its_session_before_writing_the_transcript() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    let id = thread("thread-claude-fork-reserve");
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
    let child = thread("thread-claude-fork-reserve-child");
    let owners = Arc::new(Mutex::new(vec![]));
    let (store, seen, other) = (rig.store.clone(), owners.clone(), id.clone());
    *rig.host.before_session_write.lock().unwrap() = Some(Arc::new(move |session: &str| {
        let owner = store
            .read(|c| crate::store::native_session_owner(c, session, &other))
            .unwrap();
        seen.lock().unwrap().push(owner);
    }));
    let run = rig.state(&id).await.runs[0].id.clone();
    rig.command(
        &id,
        Command::Fork {
            target: child.clone(),
            source: agent_domain::SourcePoint::Run(run),
            title: None,
        },
    )
    .await;
    rig.drain().await;
    rig.send(&child, "child", DispatchMode::StartImmediately)
        .await;
    rig.drain().await;
    assert_eq!(*owners.lock().unwrap(), [Some(child.clone())]);
    assert!(
        rig.state(&child)
            .await
            .native_sessions
            .contains_key("claude")
    );
}

/// Replies with native ids derived from the request id, as a shared app-server
/// serving several threads.
fn shared_replies(frame: &Value) -> Vec<Value> {
    let id = frame["id"].clone();
    match frame["method"].as_str() {
        Some("initialize" | "account/login/start" | "thread/unsubscribe" | "turn/interrupt") => {
            vec![json!({"id":id,"result":{}})]
        }
        Some("thread/start") => {
            vec![json!({"id":id,"result":{"thread":{"id":format!("native-{id}")}}})]
        }
        Some("turn/start") => {
            vec![json!({"id":id,"result":{"turn":{"id":format!("turn-{id}")}}})]
        }
        _ => vec![],
    }
}

impl Rig {
    /// Starts a Codex turn on the shared app-server; its native thread and turn.
    async fn shared_turn(&self, id: &ThreadId) -> (String, String) {
        self.create(
            id,
            selection(Driver::Codex, "gpt-6-luna"),
            RuntimeMode::FullAccess,
        )
        .await;
        let starts = || {
            self.host
                .last()
                .map_or(vec![], |process| written_methods(&process, "turn/start"))
        };
        let before = starts().len();
        self.send(id, "hello", DispatchMode::StartImmediately).await;
        self.drain().await;
        self.until("turn started", async || starts().len() > before)
            .await;
        let start = starts().pop().unwrap();
        let thread = start["params"]["threadId"].as_str().unwrap().to_owned();
        let turn = format!("turn-{}", start["id"]);
        self.host
            .process(0)
            .emit(json!({"method":"turn/started","params":{"threadId":thread,"turn":{"id":turn}}}));
        self.until_status(id, RunStatus::Running).await;
        (thread, turn)
    }
}

fn complete(process: &fake::FakeProcess, thread: &str, turn: &str, text: &str) {
    process.emit(json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"item":{"type":"agentMessage","id":format!("reply-{turn}"),"text":text,"phase":"final_answer"}}}));
    process.emit(json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"status":"completed"}}}));
}

fn replies(state: &State) -> Vec<String> {
    state
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::AssistantMessage { .. }))
        .map(|item| item.text.clone())
        .collect()
}

// T3 Orchestrator providerSessionIdFor: Codex supports several provider threads
// per session, so an instance's threads share one app-server and JSON-RPC id
// space; each native thread's output reaches its own thread.
#[tokio::test(flavor = "multi_thread")]
async fn codex_threads_share_one_app_server_and_keep_their_own_output() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(shared_replies);
    let (a, b) = (thread("thread-shared-a"), thread("thread-shared-b"));
    let (native_a, turn_a) = rig.shared_turn(&a).await;
    let (native_b, turn_b) = rig.shared_turn(&b).await;
    assert_ne!(native_a, native_b);
    assert_eq!(rig.host.spawned(), 1);
    let process = rig.host.process(0);
    assert_eq!(written_methods(&process, "initialize").len(), 1);
    let ids: Vec<u64> = process
        .written()
        .iter()
        .filter_map(|frame| frame["id"].as_u64())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    complete(&process, &native_b, &turn_b, "reply for b");
    rig.until_status(&b, RunStatus::Completed).await;
    assert_eq!(replies(&*rig.state(&b).await), ["reply for b"]);
    assert_eq!(rig.run_status(&a).await, RunStatus::Running);
    assert!(replies(&*rig.state(&a).await).is_empty());
    rig.command(&a, Command::Stop).await;
    rig.drain().await;
    let interrupts = written_methods(&process, "turn/interrupt");
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0]["params"]["threadId"], native_a.as_str());
    assert_eq!(interrupts[0]["params"]["turnId"], turn_a.as_str());
    complete(&process, &native_a, &turn_a, "reply for a");
    rig.until("a settles", async || rig.run_status(&a).await.terminal())
        .await;
    assert_eq!(replies(&*rig.state(&b).await), ["reply for b"]);
}

// T3 ProviderSwitchService: selecting another instance releases the thread's
// session of the previous one; the shared app-server keeps the other thread.
#[tokio::test(flavor = "multi_thread")]
async fn switching_provider_releases_only_that_threads_previous_session() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(shared_replies);
    let (a, b) = (thread("thread-switch-a"), thread("thread-switch-b"));
    let (native_a, turn_a) = rig.shared_turn(&a).await;
    rig.shared_turn(&b).await;
    let process = rig.host.process(0);
    complete(&process, &native_a, &turn_a, "done");
    rig.until_status(&a, RunStatus::Completed).await;
    rig.command(
        &a,
        Command::SelectModel {
            selection: selection(Driver::Claude, "claude-sonnet-4-6"),
        },
    )
    .await;
    rig.drain().await;
    rig.until("a is released", async || {
        rig.sessions.sessions().iter().all(|key| key.thread != a)
    })
    .await;
    let unsubscribed = written_methods(&process, "thread/unsubscribe");
    assert_eq!(unsubscribed.len(), 1);
    assert_eq!(unsubscribed[0]["params"]["threadId"], native_a.as_str());
    assert!(!process.exited());
    assert!(rig.sessions.sessions().iter().any(|key| key.thread == b));
    assert_eq!(rig.run_status(&b).await, RunStatus::Running);
}

// T3 CodexAdapterV2 account/rateLimits/updated: the app-server's account
// snapshot reaches every thread it serves, and fills the reset of a turn a
// usage limit already stopped.
#[tokio::test(flavor = "multi_thread")]
async fn shared_rate_limits_reach_every_thread_and_fill_a_stopped_turns_reset() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(shared_replies);
    let (a, b) = (thread("thread-limits-a"), thread("thread-limits-b"));
    let (native_a, turn_a) = rig.shared_turn(&a).await;
    rig.shared_turn(&b).await;
    let process = rig.host.process(0);
    process.emit(json!({"method":"turn/completed","params":{"threadId":native_a,"turn":{"id":turn_a,"status":"failed","error":{"message":"Usage limit reached.","codexErrorInfo":"usageLimitExceeded"}}}}));
    rig.until_status(&a, RunStatus::Failed).await;
    process.emit(json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":100,"resetsAt":2000000000}}}}));
    rig.until("b records the snapshot", async || {
        rig.state(&b).await.rate_limit_resets.get("codex") == Some(&Some(2_000_000_000))
    })
    .await;
    let state = rig.state(&a).await;
    assert!(state.items.iter().any(|item| matches!(
        &item.kind,
        ItemKind::Error { class: Some(class), reset_at: Some(reset), .. }
            if class == "usage_limit" && reset.millis() == 2_000_000_000_000
    )));
    assert_eq!(rig.run_status(&b).await, RunStatus::Running);
}

// T3 ProviderSessionManager: the shared session is busy while any thread's
// turn runs, and is released once every thread is idle.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_app_server_stays_while_any_thread_runs() {
    let rig = rig(options(150, 60_000), 5);
    rig.host.respond(shared_replies);
    let (a, b) = (
        thread("thread-shared-idle-a"),
        thread("thread-shared-idle-b"),
    );
    let (native_a, turn_a) = rig.shared_turn(&a).await;
    let (native_b, turn_b) = rig.shared_turn(&b).await;
    let process = rig.host.process(0);
    complete(&process, &native_a, &turn_a, "done");
    rig.until_status(&a, RunStatus::Completed).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!process.exited());
    assert_eq!(rig.sessions.sessions().len(), 2);
    complete(&process, &native_b, &turn_b, "done");
    rig.gone(&b).await;
    assert!(process.exited());
    assert!(rig.sessions.sessions().is_empty());
}

// T3 ProviderSessionManager.detach for a multi-thread session: the thread's
// running turn is interrupted and its native thread unloaded, while the
// app-server keeps serving the other thread. Only a terminal detach revokes.
#[tokio::test(flavor = "multi_thread")]
async fn detaching_from_the_shared_app_server_interrupts_and_unloads_only_that_thread() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(shared_replies);
    let (a, b) = (thread("thread-detach-a"), thread("thread-detach-b"));
    let (native_a, turn_a) = rig.shared_turn(&a).await;
    rig.shared_turn(&b).await;
    let process = rig.host.process(0);
    rig.sessions.detach(&a, false).await;
    let interrupts = written_methods(&process, "turn/interrupt");
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0]["params"]["threadId"], native_a.as_str());
    assert_eq!(interrupts[0]["params"]["turnId"], turn_a.as_str());
    let unloads = written_methods(&process, "thread/unsubscribe");
    assert_eq!(unloads.len(), 1);
    assert_eq!(unloads[0]["params"]["threadId"], native_a.as_str());
    assert!(!process.exited());
    assert_eq!(
        rig.sessions.sessions(),
        [SessionKey {
            thread: b.clone(),
            instance: "codex".into()
        }]
    );
    assert!(
        !rig.host
            .logged()
            .iter()
            .any(|entry| entry.starts_with("revoked:"))
    );
    rig.sessions.detach(&b, true).await;
    assert!(rig.host.logged().contains(&format!("revoked:{b}:*")));
    assert!(rig.sessions.sessions().is_empty());
}

// T3 CodexAdapterV2 resolveRuntime: a new app-server runs as the selected
// managed account, and its token refreshes are answered from that account.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_app_server_signs_in_with_the_managed_account_and_refreshes_its_token() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(shared_replies);
    let login = json!({"type":"chatgptAuthTokens","accessToken":"token","chatgptAccountId":"account-1","chatgptPlanType":"pro"});
    *rig.host.codex_login.lock().unwrap() = Some(login.clone());
    let id = thread("thread-managed-account");
    rig.shared_turn(&id).await;
    let process = rig.host.process(0);
    let methods: Vec<_> = process
        .written()
        .iter()
        .filter_map(|frame| frame["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        methods[..4],
        [
            "initialize",
            "initialized",
            "account/login/start",
            "thread/start"
        ]
    );
    assert_eq!(
        written_methods(&process, "account/login/start")[0]["params"],
        login
    );
    process.emit(json!({"id":"refresh-1","method":"account/chatgptAuthTokens/refresh","params":{"reason":"unauthorized","previousAccountId":"account-1"}}));
    rig.until("refresh answered", async || {
        process
            .written()
            .iter()
            .any(|frame| frame["id"] == "refresh-1")
    })
    .await;
    let answer = process
        .written()
        .into_iter()
        .find(|frame| frame["id"] == "refresh-1")
        .unwrap();
    assert_eq!(answer["result"]["accessToken"], "fresh");
    assert!(
        rig.host
            .logged()
            .contains(&"refresh:codex:Some(\"account-1\")".to_owned())
    );
}

// T3 ClaudeAdapterV2 query options: the app's MCP tools are pre-approved after
// the policy's own allowed tools, the workspace and attachments are added
// directories, and the runtime and orchestration instructions are appended to
// the system prompt.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_launch_pre_approves_the_app_tools_and_appends_the_instructions() {
    let rig = rig(SessionOptions::default(), 5);
    rig.host.respond(claude_replies);
    *rig.host.claude.lock().unwrap() = ClaudeSettings {
        mcp_servers: BTreeMap::from([(
            "orchestration".to_owned(),
            json!({"command":"agent","timeout":agent_providers::CLAUDE_MCP_TOOL_TIMEOUT_MS}),
        )]),
        mcp_allowed_tools: vec!["mcp__orchestration__*".into()],
        additional_directories: vec!["/workspace".into(), "/attachments".into()],
        append_system_prompt: agent_providers::claude_append_system_prompt(true),
        ..ClaudeSettings::default()
    };
    let id = thread("thread-claude-launch-settings");
    let process = rig.claude_turn(&id, RuntimeMode::ApprovalRequired).await;
    let args = process.request.claude.clone().unwrap().args();
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == &format!("--{name}"))
            .map(|index| args[index + 1].clone())
    };
    assert_eq!(
        value("allowedTools").as_deref(),
        Some("mcp__orchestration__*")
    );
    assert_eq!(
        args.iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--add-dir")
            .map(|(index, _)| args[index + 1].as_str())
            .collect::<Vec<_>>(),
        ["/workspace", "/attachments"]
    );
    let config: Value = serde_json::from_str(&value("mcp-config").unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["orchestration"]["timeout"],
        65 * 60 * 1000
    );
    let initialize = process
        .written()
        .into_iter()
        .find(|frame| frame["request"]["subtype"] == "initialize")
        .unwrap();
    assert_eq!(
        initialize["request"]["appendSystemPrompt"],
        agent_providers::claude_append_system_prompt(true)
    );
}
