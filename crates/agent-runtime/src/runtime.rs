//! The conversation runtime the Host serves: store, thread actors, the effect
//! worker with every executor, provider sessions, shell publishing and import.
use crate::{
    ActorContext, ActorRegistry, Clock, CommandOrigin, Committed, DaemonOptions, EffectDaemon,
    EffectHandlers, EffectWorker, ExecutorContext, HandoffCatalog, HistoryPage, HostOperations,
    HostProject, IDLE_EVICTION, ImportCounts, ImportError, ImportProject, Importer, LaunchError,
    LaunchReply, LaunchThread, LiveSessions, NoHandoffCatalog, Preparations, ProjectDirectory,
    ProjectRoots, QueryError, RuntimeError, ScanConfig, ScanResult, Scanner, SearchMatch,
    SessionHost, SessionManager, SessionOptions, ShellHub, ShellSubscribe, ShellSubscription,
    SqliteOutbox, Store, SystemClock, ThreadSubscribe, ThreadSubscription, ThreadView,
    TranscriptFs, WorkerOptions, WorkspaceFence, with_runtime_handlers,
};
use agent_domain::{Command, CommandId, Input, RecoveryTrigger, ResolvedPlan, ThreadId};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{RwLock, RwLockReadGuard, watch};
use tokio::task::JoinHandle;

/// Transcript discovery for the first-run and per-project import.
#[derive(Clone)]
pub struct ImportSettings {
    pub scan: ScanConfig,
    pub fs: Arc<dyn TranscriptFs>,
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub database: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub handoff: Arc<dyn HandoffCatalog>,
    pub sessions: SessionOptions,
    pub worker: WorkerOptions,
    pub daemon: DaemonOptions,
    /// How often idle actors are evicted; `None` keeps them loaded.
    pub eviction: Option<Duration>,
    /// `None` disables the first-run import.
    pub import: Option<ImportSettings>,
}
impl RuntimeConfig {
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
            clock: Arc::new(SystemClock),
            handoff: Arc::new(NoHandoffCatalog),
            sessions: SessionOptions::default(),
            worker: WorkerOptions::default(),
            daemon: DaemonOptions::default(),
            eviction: Some(Duration::from_secs(60)),
            import: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Opened,
    Starting,
    Started,
    Closed,
}

#[derive(Default)]
struct Background {
    daemon: Option<EffectDaemon>,
    tasks: Vec<JoinHandle<()>>,
}

/// Projects the Host owns, seen by the shell stream, search and import.
struct HostProjects(Arc<dyn HostOperations>);
impl ProjectDirectory for HostProjects {
    fn projects(&self) -> Vec<HostProject> {
        self.0.projects()
    }
}
impl ProjectRoots for HostProjects {
    fn projects(&self) -> Vec<ImportProject> {
        self.0
            .projects()
            .into_iter()
            .map(|project| ImportProject {
                id: project.id,
                root: PathBuf::from(project.root),
            })
            .collect()
    }
}

pub struct Runtime {
    executors: ExecutorContext,
    outbox: Arc<SqliteOutbox>,
    handlers: EffectHandlers,
    worker: Arc<EffectWorker>,
    shell: Arc<ShellHub>,
    projects: Arc<HostProjects>,
    importer: Option<(Arc<Importer>, Arc<Scanner>)>,
    daemon: DaemonOptions,
    eviction: Option<Duration>,
    preparations: Preparations,
    phase: watch::Sender<Phase>,
    /// Client operations hold it shared; shutdown takes it to wait for them.
    admission: RwLock<()>,
    /// Held while `start` recovers, so a shutdown waits for it.
    lifecycle: tokio::sync::Mutex<()>,
    background: Mutex<Background>,
    sweeps: crate::sweep::Sweeps,
}

impl Runtime {
    /// Opens the store and wires every component. Nothing runs until `start`.
    pub async fn open(
        config: RuntimeConfig,
        ops: Arc<dyn HostOperations>,
        host: Arc<dyn SessionHost>,
    ) -> Result<Self, RuntimeError> {
        let path = config.database.clone();
        let store = tokio::task::spawn_blocking(move || Store::open(path))
            .await
            .map_err(|_| RuntimeError::InvalidInput("the store could not be opened"))??;
        let live = LiveSessions::default();
        let mut context = ActorContext::new(store.clone());
        context.clock = config.clock.clone();
        context.handoff = config.handoff.clone();
        context.residency = Arc::new(live.clone());
        let registry = ActorRegistry::new(context);
        let sessions = SessionManager::new(registry.clone(), host, config.sessions.clone(), live);
        let executors = ExecutorContext {
            store: store.clone(),
            registry: registry.clone(),
            sessions,
            ops: ops.clone(),
            workspaces: Arc::new(WorkspaceFence::default()),
        };
        let handlers = with_runtime_handlers(EffectHandlers::default(), &executors);
        let outbox = SqliteOutbox::new(store.clone(), config.clock.clone());
        let worker = Arc::new(EffectWorker::new(
            outbox.clone(),
            handlers.clone(),
            registry.clone(),
            config.clock.clone(),
            config.worker.clone(),
        ));
        let sweeps = crate::sweep::Sweeps {
            store: store.clone(),
            registry: registry.clone(),
            ops: ops.clone(),
            clock: config.clock.clone(),
            settings_changed: Arc::default(),
        };
        let projects = Arc::new(HostProjects(ops));
        let shell = ShellHub::new(store, projects.clone())?;
        let importer = config.import.map(|settings| {
            let scanner = Arc::new(Scanner::new(
                settings.scan,
                settings.fs,
                config.clock.clone(),
            ));
            (
                Arc::new(Importer::new(registry.clone(), scanner.clone())),
                scanner,
            )
        });
        Ok(Self {
            executors,
            outbox,
            handlers,
            worker,
            shell,
            projects,
            importer,
            daemon: config.daemon,
            eviction: config.eviction,
            preparations: Preparations::default(),
            phase: watch::Sender::new(Phase::Opened),
            admission: RwLock::new(()),
            lifecycle: tokio::sync::Mutex::new(()),
            background: Mutex::new(Background::default()),
            sweeps,
        })
    }

    /// Conversation settings changed; automatic settlement runs again now.
    pub fn settings_changed(&self) {
        self.sweeps.settings_changed.notify_one();
    }

    pub fn store(&self) -> &Store {
        &self.executors.store
    }

    pub fn registry(&self) -> &Arc<ActorRegistry> {
        &self.executors.registry
    }

    pub fn sessions(&self) -> &Arc<SessionManager> {
        &self.executors.sessions
    }

    /// Settles what the previous process left behind before any client command:
    /// process-bound effects are cancelled, then every thread with unfinished
    /// work recovers. Then the effect worker, unfinished launch preparations and
    /// the first-run import start.
    pub async fn start(&self) -> Result<(), RuntimeError> {
        let claimed = self.phase.send_if_modified(|phase| {
            let opened = *phase == Phase::Opened;
            if opened {
                *phase = Phase::Starting;
            }
            opened
        });
        if !claimed {
            return Err(RuntimeError::InvalidInput(
                "the runtime was already started",
            ));
        }
        let _lifecycle = self.lifecycle.lock().await;
        let recovered = async {
            self.outbox
                .reconcile_after_process_loss(&self.handlers)
                .await?;
            self.recover(RecoveryTrigger::Startup).await?;
            Ok::<_, RuntimeError>(
                self.store()
                    .blocking(|store| store.unprepared_launches())
                    .await?,
            )
        }
        .await;
        let unprepared = match recovered {
            Ok(unprepared) => unprepared,
            Err(error) => {
                // Waiting commands fail instead of running against unrecovered threads.
                self.phase.send_replace(Phase::Closed);
                return Err(error);
            }
        };
        let mut background = self.background.lock().expect("runtime background");
        if *self.phase.borrow() != Phase::Starting {
            return Err(RuntimeError::Closed);
        }
        background.daemon = Some(self.worker.clone().spawn(self.daemon.clone()));
        if let Some(every) = self.eviction {
            background
                .tasks
                .push(self.registry().spawn_eviction(every, IDLE_EVICTION));
        }
        for (command, thread) in unprepared {
            self.preparations.schedule(&self.executors, command, thread);
        }
        background
            .tasks
            .push(tokio::spawn(self.sweeps.clone().run()));
        if let Some((importer, _)) = &self.importer {
            let (importer, projects) = (importer.clone(), self.projects.clone());
            background.tasks.push(tokio::spawn(async move {
                if let Err(error) = importer.first_run(projects.as_ref()).await {
                    tracing::warn!(%error, "the first-run import failed");
                }
            }));
        }
        self.phase.send_replace(Phase::Started);
        Ok(())
    }

    /// Hands every thread with unfinished work to its state machine.
    async fn recover(&self, trigger: RecoveryTrigger) -> Result<usize, RuntimeError> {
        let threads = self
            .store()
            .blocking(|store| store.threads_needing_recovery())
            .await?;
        for thread in &threads {
            let state = self.registry().state(thread).await?;
            let continue_after_restart = state
                .thread
                .as_ref()
                .is_some_and(|current| self.executors.ops.continue_after_restart(&current.project));
            let lookup = thread.clone();
            let capturing = self
                .store()
                .blocking(move |store| store.capturing_runs(&lookup))
                .await?;
            let input = Input::Recover {
                trigger,
                continue_after_restart,
                capturing,
            };
            let mut retried = false;
            loop {
                let actor = self.registry().get_or_load(thread).await?;
                match actor.input(input.clone()).await {
                    Err(RuntimeError::ActorStopped) if !retried => retried = true,
                    result => {
                        result?;
                        break;
                    }
                }
            }
        }
        Ok(threads.len())
    }

    /// Waits until startup recovery finished and admits one client operation,
    /// which shutdown waits for; fails once the runtime shut down.
    async fn admit(&self) -> Result<RwLockReadGuard<'_, ()>, RuntimeError> {
        let mut phase = self.phase.subscribe();
        let ready = *phase
            .wait_for(|phase| !matches!(phase, Phase::Opened | Phase::Starting))
            .await
            .map_err(|_| RuntimeError::Closed)?;
        if ready != Phase::Started {
            return Err(RuntimeError::Closed);
        }
        let admitted = self.admission.read().await;
        if *self.phase.borrow() != Phase::Started {
            return Err(RuntimeError::Closed);
        }
        Ok(admitted)
    }

    /// A client command. Internal commands are rejected.
    pub async fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> Result<Committed, RuntimeError> {
        let _admitted = self.admit().await?;
        let command = self.checked_attachments(&id, command).await?;
        let command = self.with_host_context(&id, &thread, command).await?;
        self.registry()
            .dispatch(&thread, id, command, CommandOrigin::Client)
            .await
    }

    /// Facts the state machine of one thread cannot read itself: another
    /// thread's proposed plan, and the project root a cleared worktree falls
    /// back to. A replayed command keeps its first result.
    async fn with_host_context(
        &self,
        id: &CommandId,
        thread: &ThreadId,
        mut command: Command,
    ) -> Result<Command, RuntimeError> {
        let needed = matches!(&command, Command::Send(message)
            if message.source_plan.as_ref().is_some_and(|source| source.thread != *thread))
            || matches!(
                &command,
                Command::UpdateMetadata {
                    worktree_path: Some(None),
                    ..
                }
            );
        let lookup = id.clone();
        if !needed
            || self
                .store()
                .blocking(move |store| store.receipt(&lookup))
                .await?
                .is_some()
        {
            return Ok(command);
        }
        match &mut command {
            Command::Send(message) => {
                if let Some(source) = message.source_plan.clone()
                    && source.thread != *thread
                {
                    let state = self.registry().state(&source.thread).await.ok();
                    message.resolved_plan = state.and_then(|state| {
                        let project = state.thread.as_ref()?.project.clone();
                        let plan = state.plans.iter().find(|plan| plan.id == source.plan)?;
                        Some(ResolvedPlan {
                            project,
                            kind: plan.kind,
                            implemented: plan.implemented_by.is_some(),
                        })
                    });
                }
            }
            Command::UpdateMetadata {
                worktree_path: Some(None),
                project_root,
                ..
            } => {
                let state = self.registry().state(thread).await?;
                *project_root = state
                    .thread
                    .as_ref()
                    .and_then(|current| self.executors.ops.project(&current.project))
                    .map(|project| project.root);
            }
            _ => {}
        }
        Ok(command)
    }

    /// An answer attachment that no longer exists reaches the state machine
    /// without a path, which it rejects (T3 `appendUserInputAttachmentPaths`). A
    /// replayed command keeps its first result.
    async fn checked_attachments(
        &self,
        id: &CommandId,
        command: Command,
    ) -> Result<Command, RuntimeError> {
        let Command::Respond {
            request,
            decision,
            answers,
            mut attachments,
        } = command
        else {
            return Ok(command);
        };
        let lookup = id.clone();
        let replayed = !attachments.is_empty()
            && self
                .store()
                .blocking(move |store| store.receipt(&lookup))
                .await?
                .is_some();
        if !replayed {
            for file in attachments.values_mut().flatten() {
                let found = self.executors.ops.real_path(file.path.clone()).await;
                if !matches!(found, Ok(Some(_))) {
                    file.path.clear();
                }
            }
        }
        Ok(Command::Respond {
            request,
            decision,
            answers,
            attachments,
        })
    }

    pub async fn launch(&self, request: LaunchThread) -> Result<LaunchReply, LaunchError> {
        let _admitted = match self.admit().await {
            Ok(admitted) => admitted,
            Err(error) => {
                return Err(LaunchError {
                    kind: crate::LaunchFailure::Unavailable,
                    operation: crate::LaunchOperation::CreateThread,
                    command: request.command,
                    project: request.project,
                    thread: request.thread,
                    cause: error.to_string(),
                });
            }
        };
        crate::launch::launch(&self.executors, &self.preparations, request).await
    }

    pub async fn subscribe_thread(
        &self,
        thread: ThreadId,
        options: ThreadSubscribe,
    ) -> Result<ThreadSubscription, RuntimeError> {
        self.registry()
            .get_or_load(&thread)
            .await?
            .subscribe(options)
            .await
    }

    pub async fn subscribe_shell(
        &self,
        options: ShellSubscribe,
    ) -> Result<ShellSubscription, RuntimeError> {
        self.shell.subscribe(options).await
    }

    pub async fn state(&self, thread: &ThreadId) -> Result<ThreadView, RuntimeError> {
        self.registry().get_or_load(thread).await?.view().await
    }

    /// The page before `cursor`, or the newest page.
    pub async fn history(
        &self,
        thread: &ThreadId,
        cursor: Option<&str>,
    ) -> Result<HistoryPage, QueryError> {
        self.registry()
            .get_or_load(thread)
            .await?
            .history(cursor)
            .await
    }

    pub fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<SearchMatch>, QueryError> {
        self.store().search(query, limit, self.projects.as_ref())
    }

    /// Imports a project's recent sessions, refusing a project whose root moved.
    pub async fn import(
        &self,
        project: &str,
        expected_root: Option<&Path>,
    ) -> Result<ImportCounts, ImportError> {
        let _admitted = self.admit().await?;
        match &self.importer {
            Some((importer, _)) => {
                importer
                    .import(self.projects.as_ref(), project, expected_root)
                    .await
            }
            None => Err(ImportError::Scan("import is not configured".into())),
        }
    }

    /// Directories with Codex or Claude transcripts, matched to registered projects.
    pub async fn scan(&self) -> Result<ScanResult, ImportError> {
        let Some((_, scanner)) = &self.importer else {
            return Err(ImportError::Scan("import is not configured".into()));
        };
        let (scanner, projects) = (
            scanner.clone(),
            ProjectRoots::projects(self.projects.as_ref()),
        );
        tokio::task::spawn_blocking(move || scanner.scan(&projects))
            .await
            .map_err(|error| ImportError::Scan(error.to_string()))
    }

    /// A project was added, renamed or removed.
    pub async fn project_changed(&self, project: &str) -> Result<(), RuntimeError> {
        Ok(self.shell.project_changed(project).await?)
    }

    /// Stops admitting client operations and waits for the admitted ones, stops
    /// the effect worker, launch preparations, the first-run import and provider
    /// processes, then lets every thread with unfinished work record the shutdown
    /// (a continuation, when enabled, runs after the next start).
    pub async fn shutdown(&self) {
        if self.phase.send_replace(Phase::Closed) == Phase::Closed {
            return;
        }
        let _lifecycle = self.lifecycle.lock().await;
        let _drained = self.admission.write().await;
        let background = std::mem::take(&mut *self.background.lock().expect("runtime background"));
        if let Some(daemon) = background.daemon {
            daemon.stop().await;
        }
        for task in &background.tasks {
            task.abort();
        }
        for task in background.tasks {
            let _ = task.await;
        }
        self.preparations.stop().await;
        self.sessions().shutdown().await;
        if let Err(error) = self.recover(RecoveryTrigger::Shutdown).await {
            tracing::warn!(%error, "shutdown recovery failed");
        }
        if let Err(error) = self
            .outbox
            .reconcile_after_process_loss(&self.handlers)
            .await
        {
            tracing::warn!(%error, "could not settle provider effects at shutdown");
        }
    }
}

#[cfg(test)]
mod tests;
