//! The conversation runtime the Host serves: store, thread actors, the effect
//! worker with every executor, provider sessions, shell publishing and import.
use crate::{
    ActorContext, ActorRegistry, Clock, CommandOrigin, Committed, DaemonOptions, EffectDaemon,
    EffectHandlers, EffectWorker, ExecutorContext, HandoffCatalog, HistoryPage, HostOperations,
    IDLE_EVICTION, ImportCounts, ImportError, ImportProject, Importer, LaunchError, LaunchReply,
    LaunchThread, LiveSessions, NoHandoffCatalog, ProjectDirectory, ProjectRoots, ProjectShell,
    QueryError, RuntimeError, ScanConfig, Scanner, SearchMatch, SessionHost, SessionManager,
    SessionOptions, ShellHub, ShellSubscribe, ShellSubscription, SqliteOutbox, Store, SystemClock,
    ThreadSubscribe, ThreadSubscription, ThreadView, TranscriptFs, WorkerOptions,
    with_runtime_handlers,
};
use agent_domain::{Command, CommandId, Input, RecoveryTrigger, ThreadId};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
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
    fn projects(&self) -> Vec<ProjectShell> {
        self.0
            .projects()
            .into_iter()
            .map(|project| ProjectShell {
                id: project.id,
                payload: project.payload,
            })
            .collect()
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
    importer: Option<Arc<Importer>>,
    daemon: DaemonOptions,
    eviction: Option<Duration>,
    phase: watch::Sender<Phase>,
    background: Mutex<Background>,
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
        let projects = Arc::new(HostProjects(ops));
        let shell = ShellHub::new(store, projects.clone())?;
        let importer = config.import.map(|settings| {
            Arc::new(Importer::new(
                registry.clone(),
                Arc::new(Scanner::new(
                    settings.scan,
                    settings.fs,
                    config.clock.clone(),
                )),
            ))
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
            phase: watch::Sender::new(Phase::Opened),
            background: Mutex::new(Background::default()),
        })
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
    /// work recovers. Then the effect worker and the first-run import start.
    pub async fn start(&self) -> Result<(), RuntimeError> {
        if *self.phase.borrow() != Phase::Opened {
            return Err(RuntimeError::InvalidInput(
                "the runtime was already started",
            ));
        }
        let recovered = async {
            self.outbox
                .reconcile_after_process_loss(&self.handlers)
                .await?;
            self.recover(RecoveryTrigger::Startup).await
        }
        .await;
        if let Err(error) = recovered {
            // Waiting commands fail instead of running against unrecovered threads.
            self.phase.send_replace(Phase::Closed);
            return Err(error);
        }
        let mut background = self.background.lock().expect("runtime background");
        background.daemon = Some(self.worker.clone().spawn(self.daemon.clone()));
        if let Some(every) = self.eviction {
            background
                .tasks
                .push(self.registry().spawn_eviction(every, IDLE_EVICTION));
        }
        if let Some(importer) = &self.importer {
            let handle = importer.clone().spawn_first_run(self.projects.clone());
            background.tasks.push(tokio::spawn(async move {
                match handle.await {
                    Ok(Err(error)) => tracing::warn!(%error, "the first-run import failed"),
                    Err(error) => tracing::warn!(%error, "the first-run import stopped"),
                    Ok(Ok(_)) => {}
                }
            }));
        }
        drop(background);
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
            let input = Input::Recover {
                trigger,
                continue_after_restart,
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

    /// Waits until startup recovery finished; fails once the runtime shut down.
    async fn ready(&self) -> Result<(), RuntimeError> {
        let mut phase = self.phase.subscribe();
        match *phase
            .wait_for(|phase| *phase != Phase::Opened)
            .await
            .map_err(|_| RuntimeError::Closed)?
        {
            Phase::Started => Ok(()),
            _ => Err(RuntimeError::Closed),
        }
    }

    /// A client command. Internal commands are rejected.
    pub async fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> Result<Committed, RuntimeError> {
        self.ready().await?;
        self.registry()
            .dispatch(&thread, id, command, CommandOrigin::Client)
            .await
    }

    pub async fn launch(&self, request: LaunchThread) -> Result<LaunchReply, LaunchError> {
        if let Err(error) = self.ready().await {
            return Err(LaunchError {
                operation: crate::LaunchOperation::CreateThread,
                command: request.command,
                project: request.project,
                thread: request.thread,
                cause: error.to_string(),
            });
        }
        crate::launch::launch(&self.executors, request).await
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
        match &self.importer {
            Some(importer) => {
                importer
                    .import(self.projects.as_ref(), project, expected_root)
                    .await
            }
            None => Err(ImportError::Scan("import is not configured".into())),
        }
    }

    /// A project was added, renamed or removed.
    pub fn project_changed(&self, project: &str) {
        self.shell.project_changed(project);
    }

    /// Stops the effect worker and provider processes, then lets every thread with
    /// unfinished work record the shutdown (a continuation, when enabled, runs
    /// after the next start).
    pub async fn shutdown(&self) {
        if self.phase.send_replace(Phase::Closed) == Phase::Closed {
            return;
        }
        let background = std::mem::take(&mut *self.background.lock().expect("runtime background"));
        drop(background.daemon);
        for task in background.tasks {
            task.abort();
        }
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
