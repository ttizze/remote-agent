//! ProviderRuntimeRecoveryService.test.ts and RestartContinuation.test.ts through
//! a Host that stops and opens the same store again, plus the facade's entry points.
use super::*;
use crate::executor::tests::{FakeOps, Ops, codex, message, root_workspace, tid};
use crate::session::tests::codex_replies;
use crate::session::tests::fake::FakeHost;
use crate::store::tests::at;
use crate::{EffectStatus, ManualClock, ShellUpdate, ThreadUpdate};
use agent_domain::{
    DispatchMode, InteractionMode, ProviderEvent, Question, Reply, RequestBody, RequestStatus,
    ResponseCapability, RunAttemptId, RunId, RunStatus, RuntimeMode, State,
};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

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

// "does not cancel or resume a run that completes while shutdown intent commits"
#[tokio::test(flavor = "multi_thread")]
async fn a_turn_that_finished_before_shutdown_is_neither_cancelled_nor_resumed() {
    let host = host();
    host.ops.continue_enabled.store(true, Ordering::SeqCst);
    let thread = tid("thread_finished_before_shutdown");
    {
        let first = host.open().await;
        first.start().await.unwrap();
        let (run, attempt) = live_turn(&host, &first, &thread).await;
        provider(
            &first,
            &thread,
            &attempt,
            ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: Some("turn-1".into()),
            },
        )
        .await;
        first.shutdown().await;
        let finished = state(&first, &thread).await;
        assert_eq!(
            finished
                .runs
                .iter()
                .find(|candidate| candidate.id == run)
                .unwrap()
                .status,
            RunStatus::Completed
        );
        assert!(
            !outbox(&first, &thread)
                .iter()
                .any(|(kind, _)| kind == "SendToThread")
        );
    }
    let second = host.open().await;
    second.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state(&second, &thread).await.runs.len(), 1);
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
