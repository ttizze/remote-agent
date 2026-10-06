//! Fake Host operations and a store-backed rig for the executor ports.
mod checkpoint;
mod cleanup;
mod rollback;
mod send;
mod title;

use super::*;
use crate::session::tests::fake::FakeHost;
use crate::store::tests::{at, temp_store};
use crate::{
    CreatedWorktree, Durability, EffectHandler, EffectJob, EffectWorker, HostProject, LiveSessions,
    ManualClock, PreparedRestore, SessionOptions, SetupRequest, SqliteOutbox,
    TextGenerationRequest, WorkerOptions, WorktreeRequest, effect_kind,
};
use agent_domain::{
    CheckpointScope, DispatchMode, Driver, EffectResult, InteractionMode, MessageAuthor, MessageId,
    ModelSelection, ProviderEvent, Reply, RunAttemptId, RunStatus, RuntimeMode, SendMessage,
};
use futures_util::future::BoxFuture;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

pub(crate) type Hook<I, O> = Arc<dyn Fn(I) -> BoxFuture<'static, O> + Send + Sync>;

/// Host operations recorded in order, with scriptable outcomes.
pub(crate) struct FakeOps {
    pub(crate) log: Mutex<Vec<String>>,
    pub(crate) projects: Mutex<Vec<HostProject>>,
    pub(crate) continue_enabled: AtomicBool,
    pub(crate) not_git: Mutex<BTreeSet<String>>,
    pub(crate) refs: Mutex<BTreeSet<(String, String)>>,
    pub(crate) fail_capture: AtomicBool,
    pub(crate) fail_lookup: AtomicBool,
    pub(crate) fail_commit: AtomicBool,
    pub(crate) fail_delete: AtomicBool,
    /// Resolve paths on the real file system instead of as given.
    pub(crate) real_files: AtomicBool,
    pub(crate) worktree: Mutex<Option<Hook<WorktreeRequest, Result<CreatedWorktree, String>>>>,
    pub(crate) setup: Mutex<Option<Hook<SetupRequest, Result<(), String>>>>,
    pub(crate) on_restore: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub(crate) on_real_path: Mutex<Option<Hook<String, ()>>>,
    /// Record each read of the restart-continuation setting.
    pub(crate) log_settings: AtomicBool,
    pub(crate) titles: Mutex<VecDeque<Result<String, String>>>,
    pub(crate) generations: Mutex<Vec<TextGenerationRequest>>,
}

impl FakeOps {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(vec![]),
            projects: Mutex::new(vec![project("project", "/repo")]),
            continue_enabled: AtomicBool::new(false),
            not_git: Mutex::new(BTreeSet::new()),
            refs: Mutex::new(BTreeSet::new()),
            fail_capture: AtomicBool::new(false),
            fail_lookup: AtomicBool::new(false),
            fail_commit: AtomicBool::new(false),
            fail_delete: AtomicBool::new(false),
            real_files: AtomicBool::new(false),
            worktree: Mutex::new(None),
            setup: Mutex::new(None),
            on_restore: Mutex::new(None),
            on_real_path: Mutex::new(None),
            log_settings: AtomicBool::new(false),
            titles: Mutex::new(VecDeque::new()),
            generations: Mutex::new(vec![]),
        })
    }
    pub(crate) fn record(&self, entry: impl Into<String>) {
        self.log.lock().unwrap().push(entry.into());
    }
    pub(crate) fn logged(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    pub(crate) fn logged_with(&self, prefix: &str) -> Vec<String> {
        self.logged()
            .into_iter()
            .filter(|entry| entry.starts_with(prefix))
            .collect()
    }
}

pub(crate) fn project(id: &str, root: &str) -> HostProject {
    HostProject {
        id: id.into(),
        name: id.into(),
        root: root.into(),
    }
}

/// `ordinal/N` of a checkpoint ref.
fn ordinal(reference: &str) -> String {
    reference.rsplit('/').next().unwrap_or_default().to_owned()
}

struct FakeRestore(Arc<FakeOps>);
impl PreparedRestore for FakeRestore {
    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            self.0.record("commit");
            if self.0.fail_commit.load(Ordering::SeqCst) {
                return Err("simulated commit failure".into());
            }
            Ok(())
        })
    }
    fn undo(self: Box<Self>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            self.0.record("undo");
            Ok(())
        })
    }
}

pub(crate) struct Ops(pub(crate) Arc<FakeOps>);
impl HostOperations for Ops {
    fn projects(&self) -> Vec<HostProject> {
        self.0.projects.lock().unwrap().clone()
    }
    fn continue_after_restart(&self, project: &str) -> bool {
        if self.0.log_settings.load(Ordering::SeqCst) {
            self.0.record(format!("continue-setting {project}"));
        }
        self.0.continue_enabled.load(Ordering::SeqCst)
    }
    fn real_path(&self, path: String) -> BoxFuture<'_, io::Result<Option<String>>> {
        Box::pin(async move {
            let hook = self.0.on_real_path.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(path.clone()).await;
            }
            if !self.0.real_files.load(Ordering::SeqCst) {
                return Ok(Some(path));
            }
            match std::fs::canonicalize(&path) {
                Ok(real) => Ok(Some(real.to_string_lossy().into_owned())),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
    fn is_git_repository(&self, cwd: String) -> BoxFuture<'_, bool> {
        Box::pin(async move { !self.0.not_git.lock().unwrap().contains(&cwd) })
    }
    fn capture_checkpoint(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0
                .record(format!("capture {cwd} {}", ordinal(&reference)));
            if self.0.fail_capture.load(Ordering::SeqCst) {
                return Err("simulated capture failure".into());
            }
            self.0.refs.lock().unwrap().insert((cwd, reference));
            Ok(())
        })
    }
    fn has_checkpoint(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<bool, String>> {
        Box::pin(async move {
            self.0
                .record(format!("lookup {cwd} {}", ordinal(&reference)));
            if self.0.fail_lookup.load(Ordering::SeqCst) {
                return Err("simulated ref lookup timeout".into());
            }
            Ok(self.0.refs.lock().unwrap().contains(&(cwd, reference)))
        })
    }
    fn prepare_restore(
        &self,
        cwd: String,
        reference: String,
    ) -> BoxFuture<'_, Result<Box<dyn PreparedRestore>, String>> {
        Box::pin(async move {
            self.0
                .record(format!("prepare {cwd} {}", ordinal(&reference)));
            let hook = self.0.on_restore.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
            Ok(Box::new(FakeRestore(self.0.clone())) as Box<dyn PreparedRestore>)
        })
    }
    fn finish_restore(&self, cwd: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0.record(format!("finish-restore {cwd}"));
            Ok(())
        })
    }
    fn delete_checkpoints(
        &self,
        cwd: String,
        references: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let ordinals: Vec<_> = references.iter().map(|r| ordinal(r)).collect();
            self.0
                .record(format!("delete {cwd} {}", ordinals.join(",")));
            if self.0.fail_delete.load(Ordering::SeqCst) {
                return Err("simulated ref deletion failure".into());
            }
            let mut refs = self.0.refs.lock().unwrap();
            for reference in references {
                refs.remove(&(cwd.clone(), reference));
            }
            Ok(())
        })
    }
    fn create_worktree(
        &self,
        request: WorktreeRequest,
    ) -> BoxFuture<'_, Result<CreatedWorktree, String>> {
        Box::pin(async move {
            self.0.record(format!("worktree {}", request.base_ref));
            let hook = self.0.worktree.lock().unwrap().clone();
            match hook {
                Some(hook) => hook(request).await,
                None => Ok(CreatedWorktree {
                    path: "/repo-worktrees/feature".into(),
                    branch: Some(request.branch.unwrap_or_else(|| "feature".into())),
                }),
            }
        })
    }
    fn remove_worktree(&self, _: String, path: String) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0.record(format!("remove-worktree {path}"));
            Ok(())
        })
    }
    fn run_setup(&self, request: SetupRequest) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0.record(format!("setup {}", request.cwd));
            let hook = self.0.setup.lock().unwrap().clone();
            match hook {
                Some(hook) => hook(request).await,
                None => Ok(()),
            }
        })
    }
    fn run_finalized(&self, thread: &ThreadId, _: &RunId, cwd: &str) {
        self.0.record(format!("finalized {thread} {cwd}"));
    }
    fn delete_attachments(
        &self,
        thread: ThreadId,
        paths: Vec<String>,
    ) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0
                .record(format!("delete-attachments {thread} {}", paths.join(",")));
            Ok(())
        })
    }
    fn cleanup_terminals(&self, thread: ThreadId) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.0.record(format!("terminals {thread}"));
            Ok(())
        })
    }
    fn generate_text(
        &self,
        request: TextGenerationRequest,
    ) -> BoxFuture<'_, Result<String, String>> {
        Box::pin(async move {
            self.0.generations.lock().unwrap().push(request);
            self.0
                .titles
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(r#"{"title":"Generated title","needsRefinement":false}"#.into())
                })
        })
    }
}

/// A provider turn start that only records itself, so tests drive the turn's events.
pub(crate) struct StubStart(pub(crate) Arc<FakeOps>);
impl EffectHandler for StubStart {
    fn durability(&self) -> Durability {
        Durability::ProcessBound
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            self.0.record(format!("provider-start {}", job.thread));
            Ok(None)
        })
    }
}

use agent_domain::RunId;

pub(crate) struct Rig {
    _dir: tempfile::TempDir,
    pub(crate) store: Store,
    pub(crate) registry: Arc<ActorRegistry>,
    pub(crate) sessions: Arc<SessionManager>,
    pub(crate) host: Arc<FakeHost>,
    pub(crate) ops: Arc<FakeOps>,
    pub(crate) context: ExecutorContext,
    pub(crate) preparations: crate::Preparations,
    pub(crate) handlers: EffectHandlers,
    pub(crate) worker: Arc<EffectWorker>,
    pub(crate) clock: Arc<ManualClock>,
    serial: AtomicU64,
}

pub(crate) struct RigOptions {
    pub(crate) max_attempts: u32,
    /// Replace provider turn starts by `StubStart` (behind the checkpoint baseline).
    pub(crate) stub_start: bool,
}
impl Default for RigOptions {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            stub_start: true,
        }
    }
}

pub(crate) fn rig() -> Rig {
    rig_with(RigOptions::default())
}

pub(crate) fn rig_with(options: RigOptions) -> Rig {
    let (dir, store) = temp_store();
    let clock = Arc::new(ManualClock::new(&at()));
    let live = LiveSessions::default();
    let mut actors = crate::ActorContext::new(store.clone());
    actors.clock = clock.clone();
    actors.residency = Arc::new(live.clone());
    let registry = ActorRegistry::new(actors);
    let host = FakeHost::new();
    let sessions = SessionManager::new(
        registry.clone(),
        host.clone(),
        SessionOptions {
            reply_timeout: Duration::from_secs(5),
            close_grace: Duration::from_millis(200),
            ..SessionOptions::default()
        },
        live,
    );
    let ops = FakeOps::new();
    let context = ExecutorContext {
        store: store.clone(),
        registry: registry.clone(),
        sessions: sessions.clone(),
        ops: Arc::new(Ops(ops.clone())),
        workspaces: Arc::new(WorkspaceFence::default()),
    };
    let mut handlers = with_runtime_handlers(EffectHandlers::default(), &context);
    if options.stub_start {
        handlers = handlers.with(
            "Provider.Start",
            Arc::new(BaselineBeforeStart {
                context: context.clone(),
                inner: Arc::new(StubStart(ops.clone())),
            }),
        );
    }
    let outbox = SqliteOutbox::new(store.clone(), clock.clone());
    let worker = Arc::new(EffectWorker::new(
        outbox.clone(),
        handlers.clone(),
        registry.clone(),
        clock.clone(),
        WorkerOptions {
            max_attempts: options.max_attempts,
            ..WorkerOptions::default()
        },
    ));
    Rig {
        _dir: dir,
        store,
        registry,
        sessions,
        host,
        ops,
        context,
        preparations: crate::Preparations::default(),
        handlers,
        worker,
        clock,
        serial: AtomicU64::new(0),
    }
}

pub(crate) fn codex() -> ModelSelection {
    ModelSelection {
        instance: "codex".into(),
        driver: Driver::Codex,
        model: "gpt-6-luna".into(),
        options: BTreeMap::new(),
    }
}

pub(crate) fn tid(id: &str) -> ThreadId {
    ThreadId::new(id).unwrap()
}

pub(crate) fn message(id: &str, text: &str, mode: DispatchMode) -> SendMessage {
    SendMessage {
        created_by: MessageAuthor::User,
        creation_source: "web".into(),
        id: MessageId::new(id).unwrap(),
        text: text.into(),
        attachments: vec![],
        selection: None,
        mode,
        intent: None,
        source_plan: None,
        title_seed: None,
    }
}

pub(crate) fn root_workspace(cwd: &str) -> Workspace {
    Workspace {
        cwd: cwd.into(),
        worktree_path: None,
        branch: None,
    }
}

pub(crate) fn worktree(cwd: &str) -> Workspace {
    Workspace {
        cwd: cwd.into(),
        worktree_path: Some(cwd.into()),
        branch: Some("feature".into()),
    }
}

impl Rig {
    pub(crate) fn command_id(&self) -> CommandId {
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
    pub(crate) async fn create(&self, id: &ThreadId, workspace: Option<Workspace>) {
        self.create_in(id, "project", workspace).await;
    }
    pub(crate) async fn create_in(
        &self,
        id: &ThreadId,
        project: &str,
        workspace: Option<Workspace>,
    ) {
        let reply = self
            .command(
                id,
                Command::Create {
                    thread: id.clone(),
                    project: project.into(),
                    title: "Seed title".into(),
                    selection: codex(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    workspace,
                },
            )
            .await;
        assert_eq!(reply, Reply::Thread(id.clone()));
    }
    /// A thread in `cwd` whose runs capture checkpoints there.
    pub(crate) async fn scoped(&self, id: &ThreadId, workspace: Workspace) -> CheckpointScope {
        let scope = checkpoint_scope(id, &workspace.cwd);
        self.create(id, Some(workspace)).await;
        self.input(
            id,
            Input::CheckpointScope {
                run: None,
                attempt: None,
                scope: Some(scope.clone()),
            },
        )
        .await;
        scope
    }
    pub(crate) async fn input(&self, thread: &ThreadId, input: Input) -> Reply {
        self.context.input(thread, input).await.unwrap().reply
    }
    pub(crate) async fn send(&self, thread: &ThreadId, id: &str, text: &str) -> RunId {
        match self
            .command(
                thread,
                Command::Send(message(id, text, DispatchMode::StartImmediately)),
            )
            .await
        {
            Reply::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }
    pub(crate) async fn state(&self, thread: &ThreadId) -> Arc<State> {
        self.registry.state(thread).await.unwrap()
    }
    pub(crate) async fn drain(&self) {
        self.worker.drain(100).await.unwrap();
    }
    pub(crate) async fn attempt(&self, thread: &ThreadId, run: &RunId) -> RunAttemptId {
        self.state(thread)
            .await
            .runs
            .iter()
            .find(|candidate| &candidate.id == run)
            .and_then(|run| run.attempt.clone())
            .unwrap()
    }
    pub(crate) async fn provider(
        &self,
        thread: &ThreadId,
        attempt: &RunAttemptId,
        event: ProviderEvent,
    ) {
        self.registry
            .get_or_load(thread)
            .await
            .unwrap()
            .provider(attempt.clone(), event)
            .await
            .unwrap();
    }
    /// Starts the run's provider turn (through the outbox) and finishes it at `head`.
    pub(crate) async fn turn(&self, thread: &ThreadId, run: &RunId, head: &str, status: RunStatus) {
        self.drain().await;
        let attempt = self.attempt(thread, run).await;
        for event in [
            ProviderEvent::SessionReady {
                native_thread: "native-thread".into(),
            },
            ProviderEvent::TurnStarted {
                native_turn: Some(head.into()),
            },
            ProviderEvent::TurnFinished {
                status,
                native_head: Some(head.into()),
            },
        ] {
            self.provider(thread, &attempt, event).await;
        }
    }
    /// A run that ran to completion and captured its checkpoint.
    pub(crate) async fn completed_run(&self, thread: &ThreadId, id: &str, head: &str) -> RunId {
        let run = self.send(thread, id, id).await;
        self.turn(thread, &run, head, RunStatus::Completed).await;
        self.drain().await;
        run
    }
    pub(crate) async fn run(&self, thread: &ThreadId, run: &RunId) -> agent_domain::Run {
        self.state(thread)
            .await
            .runs
            .iter()
            .find(|candidate| &candidate.id == run)
            .cloned()
            .unwrap()
    }
    /// The latest outbox row of `kind`, as its first execution.
    pub(crate) async fn job(&self, thread: &ThreadId, kind: &str) -> EffectJob {
        let id = thread.clone();
        let row = self
            .store
            .blocking(move |store| store.outbox(&id))
            .await
            .unwrap()
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
    pub(crate) async fn execute(
        &self,
        job: EffectJob,
    ) -> Result<Option<EffectResult>, EffectError> {
        let kind = effect_kind(&job.effect.body).unwrap();
        self.handlers.get(&kind).unwrap().run(job).await
    }
    pub(crate) async fn outbox_kinds(
        &self,
        thread: &ThreadId,
    ) -> Vec<(String, crate::EffectStatus)> {
        let id = thread.clone();
        self.store
            .blocking(move |store| store.outbox(&id))
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.kind, row.status))
            .collect()
    }
}
