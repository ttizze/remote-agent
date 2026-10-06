//! ProviderRuntimeRecoveryService.test.ts and RestartContinuation.test.ts through
//! a Host that stops and opens the same store again, plus the facade's entry points.
use super::*;
use crate::executor::tests::{FakeOps, Ops, codex, message, root_workspace, tid};
use crate::session::tests::codex_replies;
use crate::session::tests::fake::{FakeHost, Gate};
use crate::store::tests::at;
use crate::{EffectStatus, ManualClock, ShellUpdate, ThreadUpdate};
use agent_domain::{Attachment, AttachmentKind};
use agent_domain::{
    DispatchMode, InteractionMode, ProviderEvent, Question, Reply, RequestBody, RequestStatus,
    ResponseCapability, RunAttemptId, RunId, RunStatus, RuntimeMode, State, Workspace,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

struct Host {
    _dir: tempfile::TempDir,
    path: PathBuf,
    ops: Arc<FakeOps>,
    host: Arc<FakeHost>,
    clock: Arc<ManualClock>,
    serial: AtomicU64,
}

fn host() -> Host {
    let dir = tempfile::tempdir().unwrap();
    let host = FakeHost::new();
    host.respond(codex_replies);
    Host {
        path: dir.path().join("runtime.sqlite"),
        _dir: dir,
        ops: FakeOps::new(),
        host,
        clock: Arc::new(ManualClock::new(&at())),
        serial: AtomicU64::new(0),
    }
}

impl Host {
    async fn open(&self) -> Runtime {
        let mut config = RuntimeConfig::new(&self.path);
        config.clock = self.clock.clone();
        config.eviction = None;
        config.daemon = DaemonOptions {
            concurrency: 4,
            liveness: Duration::from_millis(20),
        };
        config.sessions = SessionOptions {
            reply_timeout: Duration::from_secs(5),
            close_grace: Duration::from_millis(200),
            ..SessionOptions::default()
        };
        Runtime::open(config, Arc::new(Ops(self.ops.clone())), self.host.clone())
            .await
            .unwrap()
    }
    fn id(&self) -> CommandId {
        CommandId::new(format!(
            "command-{}",
            self.serial.fetch_add(1, Ordering::SeqCst)
        ))
        .unwrap()
    }
    /// Commands that bypass the startup gate, as Host-internal work does.
    async fn internal(&self, runtime: &Runtime, thread: &ThreadId, command: Command) -> Reply {
        runtime
            .registry()
            .dispatch(thread, self.id(), command, CommandOrigin::Internal)
            .await
            .unwrap()
            .reply
    }
    async fn create(&self, runtime: &Runtime, thread: &ThreadId) {
        let reply = self
            .internal(
                runtime,
                thread,
                Command::Create {
                    thread: thread.clone(),
                    project: "project".into(),
                    title: "Thread".into(),
                    selection: codex(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    workspace: Some(root_workspace("/repo")),
                    created_by: agent_domain::MessageAuthor::User,
                    creation_source: "desktop".into(),
                },
            )
            .await;
        assert_eq!(reply, Reply::Thread(thread.clone()));
    }
    async fn send(
        &self,
        runtime: &Runtime,
        thread: &ThreadId,
        id: &str,
        mode: DispatchMode,
    ) -> RunId {
        match self
            .internal(runtime, thread, Command::Send(message(id, id, mode)))
            .await
        {
            Reply::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }
}

async fn state(runtime: &Runtime, thread: &ThreadId) -> Arc<State> {
    runtime.registry().state(thread).await.unwrap()
}

async fn attempt(runtime: &Runtime, thread: &ThreadId, run: &RunId) -> RunAttemptId {
    state(runtime, thread)
        .await
        .runs
        .iter()
        .find(|candidate| &candidate.id == run)
        .and_then(|run| run.attempt.clone())
        .unwrap()
}

async fn provider(
    runtime: &Runtime,
    thread: &ThreadId,
    attempt: &RunAttemptId,
    event: ProviderEvent,
) {
    runtime
        .registry()
        .get_or_load(thread)
        .await
        .unwrap()
        .provider(attempt.clone(), event)
        .await
        .unwrap();
}

async fn until(what: &str, check: impl AsyncFn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn request(key: &str, capability: ResponseCapability) -> ProviderEvent {
    ProviderEvent::RequestOpened {
        owner_path: vec![],
        key: key.into(),
        body: RequestBody::Questions {
            questions: vec![Question {
                required: true,
                id: "choice".into(),
                header: "Choose".into(),
                question: "Which?".into(),
                multiple: false,
                options: vec![],
            }],
        },
        capability,
    }
}

fn outbox(runtime: &Runtime, thread: &ThreadId) -> Vec<(String, EffectStatus)> {
    runtime
        .store()
        .outbox(thread)
        .unwrap()
        .into_iter()
        .map(|row| (row.kind, row.status))
        .collect()
}

// "reads recovery projections only for threads that need runtime recovery",
// "expires orphaned runtime requests before command readiness", "preserves async
// questions across startup and shutdown", "holds accepted queued work without
// cancelling its execution state after restart" and "leaves durable effects for
// the worker after runtime reconciliation".
#[tokio::test(flavor = "multi_thread")]
async fn startup_recovers_unfinished_threads_before_any_client_command() {
    let host = host();
    let running = tid("thread_recovery_requests");
    let idle = tid("thread_recovery_settled");
    let questioned = tid("async-recovery-thread");
    {
        let first = host.open().await;
        host.create(&first, &running).await;
        let cut = host
            .send(&first, &running, "cut", DispatchMode::StartImmediately)
            .await;
        let cut_attempt = attempt(&first, &running, &cut).await;
        provider(
            &first,
            &running,
            &cut_attempt,
            ProviderEvent::TurnStarted {
                native_turn: Some("turn-1".into()),
            },
        )
        .await;
        provider(
            &first,
            &running,
            &cut_attempt,
            request("orphaned", ResponseCapability::Live),
        )
        .await;
        host.send(&first, &running, "queued", DispatchMode::QueueAfterActive)
            .await;

        host.create(&first, &idle).await;

        host.create(&first, &questioned).await;
        let asked = host
            .send(&first, &questioned, "ask", DispatchMode::StartImmediately)
            .await;
        let asked_attempt = attempt(&first, &questioned, &asked).await;
        provider(
            &first,
            &questioned,
            &asked_attempt,
            ProviderEvent::TurnStarted {
                native_turn: Some("turn-1".into()),
            },
        )
        .await;
        provider(
            &first,
            &questioned,
            &asked_attempt,
            request("async", ResponseCapability::Message),
        )
        .await;
        provider(
            &first,
            &questioned,
            &asked_attempt,
            ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: Some("turn-1".into()),
            },
        )
        .await;
        // The previous process ends without shutting down.
    }
    let second = Arc::new(host.open().await);
    assert_eq!(
        second.store().threads_needing_recovery().unwrap(),
        std::slice::from_ref(&running)
    );
    let questioned_head = second.store().thread_head(&questioned).unwrap();
    let request_id = state(&second, &running).await.requests[0].id.clone();
    let client = {
        let (runtime, thread) = (second.clone(), running.clone());
        tokio::spawn(async move {
            runtime
                .dispatch(
                    thread,
                    CommandId::new("respond-after-restart").unwrap(),
                    Command::Respond {
                        request: request_id,
                        decision: None,
                        answers: Some(
                            [("choice".to_owned(), agent_domain::Answer::Text("A".into()))].into(),
                        ),
                        attachments: Default::default(),
                    },
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!client.is_finished(), "a client command waits for recovery");
    assert_eq!(second.registry().loaded(), vec![running.clone()]);

    second.start().await.unwrap();
    let reply = client.await.unwrap().unwrap().reply;
    assert!(matches!(reply, Reply::Rejected { .. }), "{reply:?}");

    let recovered = state(&second, &running).await;
    assert_eq!(recovered.requests[0].status, RequestStatus::Expired);
    assert_eq!(recovered.runs[0].status, RunStatus::Cancelled);
    assert_eq!(recovered.runs[1].status, RunStatus::Queued);
    assert!(recovered.runs[1].queue_held);
    assert_eq!(
        outbox(&second, &running),
        [("Provider.Start".to_owned(), EffectStatus::Cancelled)]
    );
    let mut loaded = second.registry().loaded();
    loaded.sort();
    assert_eq!(loaded, vec![running.clone()]);
    assert_eq!(
        second.store().thread_head(&questioned).unwrap(),
        questioned_head
    );
    let asked = state(&second, &questioned).await;
    assert_eq!(asked.requests[0].status, RequestStatus::Pending);
    second.shutdown().await;
}

/// A Codex turn that is running on a live provider process.
async fn live_turn(host: &Host, runtime: &Runtime, thread: &ThreadId) -> (RunId, RunAttemptId) {
    runtime
        .dispatch(
            thread.clone(),
            host.id(),
            Command::Create {
                thread: thread.clone(),
                project: "project".into(),
                title: "Thread".into(),
                selection: codex(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: Some(root_workspace("/repo")),
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        )
        .await
        .unwrap();
    let spawned = host.host.spawned();
    let Reply::Run(run) = runtime
        .dispatch(
            thread.clone(),
            host.id(),
            Command::Send(message("cut", "Keep going", DispatchMode::StartImmediately)),
        )
        .await
        .unwrap()
        .reply
    else {
        panic!("no run")
    };
    until("turn started", async || {
        host.host.spawned() > spawned
            && host
                .host
                .last()
                .unwrap()
                .written()
                .iter()
                .any(|frame| frame["method"] == "turn/start")
    })
    .await;
    host.host.last().unwrap().emit(json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"turn-1"}}}));
    until("run running", async || {
        state(runtime, thread)
            .await
            .runs
            .iter()
            .any(|candidate| candidate.id == run && candidate.status == RunStatus::Running)
    })
    .await;
    let attempt = attempt(runtime, thread, &run).await;
    (run, attempt)
}

// "uses the same reconciliation path to cancel runtime requests during shutdown"
// and RestartContinuation through a restarted Host.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_cancels_live_work_and_the_cut_turn_continues_after_restart() {
    let host = host();
    host.ops.continue_enabled.store(true, Ordering::SeqCst);
    let thread = tid("thread_shutdown_requests");
    let source = {
        let first = host.open().await;
        first.start().await.unwrap();
        let (run, attempt) = live_turn(&host, &first, &thread).await;
        provider(
            &first,
            &thread,
            &attempt,
            request("request_shutdown", ResponseCapability::Live),
        )
        .await;
        let process = host.host.last().unwrap();

        first.shutdown().await;
        assert!(process.exited());
        let stopped = state(&first, &thread).await;
        assert_eq!(stopped.requests[0].status, RequestStatus::Cancelled);
        assert_eq!(stopped.runs[0].status, RunStatus::Cancelled);
        let rows = outbox(&first, &thread);
        assert!(
            rows.iter()
                .all(|(kind, status)| !kind.starts_with("Provider.")
                    || *status != EffectStatus::Pending && *status != EffectStatus::Running),
            "{rows:?}"
        );
        assert!(
            rows.contains(&("SendToThread".to_owned(), EffectStatus::Pending)),
            "{rows:?}"
        );
        assert!(matches!(
            first
                .dispatch(thread.clone(), host.id(), Command::MarkUnread)
                .await,
            Err(RuntimeError::Closed)
        ));
        run
    };
    let second = host.open().await;
    second.start().await.unwrap();
    until("continuation started", async || {
        state(&second, &thread)
            .await
            .runs
            .iter()
            .any(|run| run.restart_of.as_ref() == Some(&source))
    })
    .await;
    second.shutdown().await;
}

// "does not cancel or resume a run that completes while shutdown intent commits":
// the turn finishes while shutdown is stopping its provider process, after the
// effect worker stopped and before the shutdown is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_finishes_during_shutdown_is_neither_cancelled_nor_resumed() {
    let host = host();
    host.ops.continue_enabled.store(true, Ordering::SeqCst);
    let thread = tid("thread_finished_during_shutdown");
    let run = {
        let first = Arc::new(host.open().await);
        first.start().await.unwrap();
        let (run, attempt) = live_turn(&host, &first, &thread).await;
        let finished = Arc::new(AtomicBool::new(false));
        let (runtime, target, flag) = (first.clone(), thread.clone(), finished.clone());
        *host.host.last().unwrap().on_stdin_close.lock().unwrap() =
            Some(Box::new(move |process| {
                tokio::spawn(async move {
                    provider(
                        &runtime,
                        &target,
                        &attempt,
                        ProviderEvent::TurnFinished {
                            status: RunStatus::Completed,
                            native_head: Some("turn-1".into()),
                        },
                    )
                    .await;
                    flag.store(true, Ordering::SeqCst);
                    process.exit(true);
                });
            }));
        assert_eq!(
            state(&first, &thread).await.runs[0].status,
            RunStatus::Running
        );

        first.shutdown().await;
        assert!(finished.load(Ordering::SeqCst));
        let stopped = state(&first, &thread).await;
        let status = stopped.runs[0].status;
        assert!(
            matches!(status, RunStatus::Completed | RunStatus::Waiting),
            "{status:?}"
        );
        assert!(
            !outbox(&first, &thread)
                .iter()
                .any(|(kind, _)| kind == "SendToThread")
        );
        run
    };
    let second = host.open().await;
    second.start().await.unwrap();
    until("checkpoint captured", async || {
        state(&second, &thread).await.runs[0].status == RunStatus::Completed
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = state(&second, &thread).await;
    assert_eq!(settled.runs.len(), 1);
    assert_eq!(settled.runs[0].id, run);
    second.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_facade_admits_client_commands_and_serves_reads() {
    let host = host();
    let runtime = host.open().await;
    runtime.start().await.unwrap();
    let launched = runtime
        .launch(LaunchThread {
            command: CommandId::new("command:facade:launch").unwrap(),
            thread: None,
            project: "project".into(),
            title: "Facade".into(),
            generate_title: false,
            selection: codex(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: crate::WorkspaceStrategy::Root { branch: None },
            initial_message: None,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "desktop".into(),
        })
        .await
        .unwrap();
    let thread = launched.thread;
    let internal = runtime
        .dispatch(
            thread.clone(),
            host.id(),
            Command::ContinueRestart {
                source: RunId::new("run").unwrap(),
                enabled: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        internal.reply,
        Reply::Rejected {
            reason: "internal-command".into()
        }
    );
    let sent = runtime
        .dispatch(
            thread.clone(),
            host.id(),
            Command::Send(message(
                "searchable",
                "Find this later",
                DispatchMode::QueueAfterActive,
            )),
        )
        .await
        .unwrap();
    assert!(matches!(sent.reply, Reply::Run(_)));

    let view = runtime.state(&thread).await.unwrap();
    assert!(view.head.global_seq >= sent.global_seq);
    let matches = runtime.search("Find this", None).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].thread, thread);
    assert!(
        !runtime
            .history(&thread, None)
            .await
            .unwrap()
            .rows
            .is_empty()
    );
    let mut shell = runtime.subscribe_shell(Default::default()).await.unwrap();
    assert!(matches!(
        shell.updates.recv().await,
        Some(ShellUpdate::Snapshot(_))
    ));
    let mut stream = runtime
        .subscribe_thread(thread.clone(), Default::default())
        .await
        .unwrap();
    assert!(matches!(
        stream.updates.recv().await,
        Some(ThreadUpdate::Snapshot(_))
    ));
    assert!(runtime.import("project", None).await.is_err());
    runtime.shutdown().await;
}

fn launch_request(
    command: &str,
    workspace: crate::WorkspaceStrategy,
    message: bool,
) -> LaunchThread {
    LaunchThread {
        command: CommandId::new(command).unwrap(),
        thread: None,
        project: "project".into(),
        title: "Launched".into(),
        generate_title: false,
        selection: codex(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace,
        initial_message: message.then(|| crate::InitialMessage {
            context: None,
            id: None,
            text: "Start here".into(),
            attachments: vec![],
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "web".into(),
        }),
        created_by: agent_domain::MessageAuthor::User,
        creation_source: "desktop".into(),
    }
}

fn new_worktree() -> crate::WorkspaceStrategy {
    crate::WorkspaceStrategy::Worktree {
        base_ref: "main".into(),
        branch: None,
        start_from_origin: false,
    }
}

fn file(path: &str) -> Attachment {
    Attachment {
        kind: AttachmentKind::File,
        source: None,
        id: "attachment".into(),
        name: "notes.txt".into(),
        mime_type: "text/plain".into(),
        path: path.into(),
        size: 4,
    }
}

fn answer_with(request: agent_domain::RuntimeRequestId, path: &str) -> Command {
    Command::Respond {
        request,
        decision: None,
        answers: Some([("choice".to_owned(), agent_domain::Answer::Text("A".into()))].into()),
        attachments: [("choice".to_owned(), vec![file(path)])].into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_waits_for_an_admitted_client_command() {
    let host = host();
    let runtime = Arc::new(host.open().await);
    runtime.start().await.unwrap();
    let thread = tid("thread_admitted");
    host.create(&runtime, &thread).await;
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    *host.ops.on_real_path.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = hook.clone();
        Box::pin(async move { gate.pass().await })
    }));
    let command = {
        let (runtime, thread) = (runtime.clone(), thread.clone());
        tokio::spawn(async move {
            runtime
                .dispatch(
                    thread,
                    CommandId::new("respond-while-stopping").unwrap(),
                    answer_with(
                        agent_domain::RuntimeRequestId::new("request-unknown").unwrap(),
                        "/attachments/notes.txt",
                    ),
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.until_arrived(1))
        .await
        .expect("the command was admitted");
    let stopping = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !stopping.is_finished(),
        "shutdown waits for the admitted command"
    );
    gate.release();
    let committed = command.await.unwrap().unwrap();
    assert!(matches!(committed.reply, Reply::Rejected { .. }));
    stopping.await.unwrap();
    assert!(matches!(
        runtime
            .dispatch(thread, host.id(), Command::MarkUnread)
            .await,
        Err(RuntimeError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_admits_once_and_a_shutdown_during_startup_leaves_the_runtime_closed() {
    let host = host();
    let thread = tid("thread_start_race");
    {
        let first = host.open().await;
        host.create(&first, &thread).await;
        host.send(&first, &thread, "cut", DispatchMode::StartImmediately)
            .await;
    }
    let runtime = host.open().await;
    let (started, ()) = tokio::join!(runtime.start(), runtime.shutdown());
    assert!(matches!(started, Err(RuntimeError::Closed)), "{started:?}");
    assert!(runtime.start().await.is_err());
    assert!(matches!(
        runtime
            .dispatch(thread.clone(), host.id(), Command::MarkUnread)
            .await,
        Err(RuntimeError::Closed)
    ));

    let runtime = host.open().await;
    let (first, second) = tokio::join!(runtime.start(), runtime.start());
    assert_eq!(
        [first.is_ok(), second.is_ok()]
            .iter()
            .filter(|started| **started)
            .count(),
        1
    );
    runtime.shutdown().await;
}

// The effect worker has stopped, and no executor still runs, before the shutdown
// is recorded on the threads.
#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_running_executors_before_recording_the_shutdown() {
    struct Slow(Arc<FakeOps>);
    impl Drop for Slow {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_millis(100));
            self.0.record("setup-dropped");
        }
    }
    let host = host();
    let runtime = host.open().await;
    runtime.start().await.unwrap();
    let gate = Arc::new(Gate::default());
    let (hook, ops) = (gate.clone(), host.ops.clone());
    *host.ops.setup.lock().unwrap() = Some(Arc::new(move |_| {
        let (gate, slow) = (hook.clone(), Slow(ops.clone()));
        Box::pin(async move {
            let _slow = slow;
            gate.pass().await;
            Ok(())
        })
    }));
    runtime
        .launch(launch_request(
            "command:launch:stopping",
            crate::WorkspaceStrategy::Root { branch: None },
            true,
        ))
        .await
        .unwrap();
    gate.until_arrived(1).await;
    host.ops.log_settings.store(true, Ordering::SeqCst);

    runtime.shutdown().await;
    let log = host.ops.logged();
    let dropped = log.iter().position(|entry| entry == "setup-dropped");
    let recorded = log
        .iter()
        .position(|entry| entry.starts_with("continue-setting"));
    assert!(
        dropped.is_some() && recorded.is_some() && dropped < recorded,
        "{log:?}"
    );
}

// Background preparation runs in a scope: shutdown stops it, and the next
// start resumes the recorded launch.
#[tokio::test(flavor = "multi_thread")]
async fn a_background_preparation_stops_at_shutdown_and_resumes_after_restart() {
    let host = host();
    let gate = Arc::new(Gate::default());
    let hook = gate.clone();
    *host.ops.worktree.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = hook.clone();
        Box::pin(async move {
            gate.pass().await;
            Err("never released".into())
        })
    }));
    let thread = {
        let first = host.open().await;
        first.start().await.unwrap();
        let launched = first
            .launch(launch_request(
                "command:launch:resumed",
                new_worktree(),
                false,
            ))
            .await
            .unwrap();
        gate.until_arrived(1).await;
        first.shutdown().await;
        let stopped = state(&first, &launched.thread).await;
        assert_eq!(stopped.thread.as_ref().unwrap().workspace, None);
        launched.thread
    };
    *host.ops.worktree.lock().unwrap() = None;
    let second = host.open().await;
    second.start().await.unwrap();
    until("worktree bound", async || {
        state(&second, &thread)
            .await
            .thread
            .as_ref()
            .and_then(|thread| thread.workspace.as_ref())
            .is_some_and(|workspace| workspace.cwd == "/repo-worktrees/feature")
    })
    .await;
    second.shutdown().await;
    assert!(second.store().unprepared_launches().unwrap().is_empty());
}

/// A transcript file system whose first lookup once armed blocks until the test
/// releases it.
struct BlockedFs {
    armed: AtomicBool,
    arrived: AtomicBool,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl TranscriptFs for BlockedFs {
    fn read_dir(&self, _: &Path) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    fn stat(&self, path: &Path) -> std::io::Result<crate::FileStat> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            path.display().to_string(),
        ))
    }
    fn real_path(&self, path: &Path) -> std::io::Result<PathBuf> {
        if self.armed.load(Ordering::SeqCst) && !self.arrived.swap(true, Ordering::SeqCst) {
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        }
        Ok(path.to_path_buf())
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn crate::TranscriptFile>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            path.display().to_string(),
        ))
    }
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            path.display().to_string(),
        ))
    }
}

fn first_run_done(runtime: &Runtime) -> bool {
    runtime
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM runtime_meta WHERE key = ?1)",
                [crate::FIRST_RUN_IMPORT_KEY],
                |row| row.get::<_, bool>(0),
            )?)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_the_first_run_import_itself() {
    let host = host();
    let (release, blocked) = std::sync::mpsc::channel();
    let fs = Arc::new(BlockedFs {
        armed: AtomicBool::new(false),
        arrived: AtomicBool::new(false),
        release: Mutex::new(blocked),
    });
    let mut config = RuntimeConfig::new(&host.path);
    config.clock = host.clock.clone();
    config.eviction = None;
    let dir = tempfile::tempdir().unwrap();
    config.import = Some(ImportSettings {
        scan: ScanConfig {
            homes: vec![],
            home_dir: dir.path().into(),
            temp_dir: dir.path().into(),
            managed_dirs: vec![],
        },
        fs: fs.clone(),
    });
    let runtime = Runtime::open(config, Arc::new(Ops(host.ops.clone())), host.host.clone())
        .await
        .unwrap();
    fs.armed.store(true, Ordering::SeqCst);
    runtime.start().await.unwrap();
    until("import scanning", async || {
        fs.arrived.load(Ordering::SeqCst)
    })
    .await;

    // The scan step in flight finishes during shutdown; nothing after it runs.
    let releasing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        release.send(()).unwrap();
    });
    runtime.shutdown().await;
    releasing.await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!first_run_done(&runtime));
}

#[tokio::test(flavor = "multi_thread")]
async fn import_waits_for_startup_and_is_refused_after_shutdown() {
    let host = host();
    let runtime = Arc::new(host.open().await);
    let early = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.import("project", None).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!early.is_finished(), "an import waits for startup recovery");
    runtime.start().await.unwrap();
    assert!(matches!(
        early.await.unwrap(),
        Err(ImportError::Scan(message)) if message == "import is not configured"
    ));
    runtime.shutdown().await;
    assert!(matches!(
        runtime.import("project", None).await,
        Err(ImportError::Runtime(RuntimeError::Closed))
    ));
}

// An answer whose attachment is gone is refused before the request resolves.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_with_an_unavailable_attachment_is_rejected() {
    let host = host();
    host.ops.real_files.store(true, Ordering::SeqCst);
    let runtime = host.open().await;
    runtime.start().await.unwrap();
    let thread = tid("thread_answer_attachment");
    let (_, attempt) = live_turn(&host, &runtime, &thread).await;
    provider(
        &runtime,
        &thread,
        &attempt,
        request("question", ResponseCapability::Live),
    )
    .await;
    let request_id = state(&runtime, &thread).await.requests[0].id.clone();
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.txt");

    let refused = runtime
        .dispatch(
            thread.clone(),
            host.id(),
            answer_with(request_id.clone(), &missing.to_string_lossy()),
        )
        .await
        .unwrap();
    assert_eq!(
        refused.reply,
        Reply::Rejected {
            reason: "attachment-unavailable".into()
        }
    );
    assert_eq!(
        state(&runtime, &thread).await.requests[0].status,
        RequestStatus::Pending
    );

    let present = dir.path().join("notes.txt");
    std::fs::write(&present, "note").unwrap();
    let accepted = runtime
        .dispatch(
            thread.clone(),
            host.id(),
            answer_with(request_id.clone(), &present.to_string_lossy()),
        )
        .await
        .unwrap();
    assert_eq!(accepted.reply, Reply::Request(request_id));
    runtime.shutdown().await;
}

// Updating thread metadata keeps a thread without a worktree in its project
// root when only the branch changes, and a retried command replays its first
// result although the Host filled in the project root.
#[tokio::test(flavor = "multi_thread")]
async fn a_branch_update_keeps_the_project_root_and_replays_on_retry() {
    let host = host();
    let runtime = host.open().await;
    runtime.start().await.unwrap();
    let thread = tid("thread_branch_only");
    let created = host
        .internal(
            &runtime,
            &thread,
            Command::Create {
                thread: thread.clone(),
                project: "project".into(),
                title: "Thread".into(),
                selection: codex(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
                created_by: agent_domain::MessageAuthor::User,
                creation_source: "desktop".into(),
            },
        )
        .await;
    assert_eq!(created, Reply::Thread(thread.clone()));
    let update = |branch: &str| Command::UpdateMetadata {
        title: None,
        regenerate_title: None,
        branch: Some(Some(branch.into())),
        worktree_path: None,
        expected_worktree_path: None,
        expected_empty: false,
        limit_recovery: None,
        linked_pull_request: None,
        project_root: None,
    };
    let id = host.id();
    let first = runtime
        .dispatch(thread.clone(), id.clone(), update("feature"))
        .await
        .unwrap();
    assert_eq!(first.reply, Reply::Accepted);
    assert_eq!(
        state(&runtime, &thread)
            .await
            .thread
            .as_ref()
            .unwrap()
            .workspace,
        Some(Workspace {
            cwd: "/repo".into(),
            worktree_path: None,
            branch: Some("feature".into()),
        })
    );
    let retried = runtime
        .dispatch(thread.clone(), id, update("feature"))
        .await
        .unwrap();
    assert_eq!(retried.reply, Reply::Accepted);
    assert!(retried.replayed);
    runtime.shutdown().await;
}

// Cancels a stale waiting run when no checkpoint capture can finish it,
// through a restarted Host.
#[tokio::test(flavor = "multi_thread")]
async fn startup_cancels_a_waiting_run_whose_capture_failed_for_good() {
    let host = host();
    let thread = tid("thread_stale_waiting");
    {
        let first = host.open().await;
        host.create(&first, &thread).await;
        first
            .registry()
            .get_or_load(&thread)
            .await
            .unwrap()
            .input(Input::CheckpointScope {
                run: None,
                attempt: None,
                scope: Some(crate::checkpoint_scope(&thread, "/repo")),
            })
            .await
            .unwrap();
        let run = host
            .send(&first, &thread, "cut", DispatchMode::StartImmediately)
            .await;
        let attempt = attempt(&first, &thread, &run).await;
        for event in [
            ProviderEvent::TurnStarted {
                native_turn: Some("turn-1".into()),
            },
            ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: Some("turn-1".into()),
            },
        ] {
            provider(&first, &thread, &attempt, event).await;
        }
        assert_eq!(
            state(&first, &thread).await.runs[0].status,
            RunStatus::Waiting
        );
        // The capture ran out of attempts.
        first
            .store()
            .write(|tx| {
                tx.execute(
                    "UPDATE outbox SET status = 'failed' WHERE kind = 'CaptureCheckpoint'",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let second = host.open().await;
    second.start().await.unwrap();
    let recovered = state(&second, &thread).await;
    assert_eq!(recovered.runs[0].status, RunStatus::Cancelled);
    assert!(recovered.captures.is_empty());
    second.shutdown().await;
}
