//! Turns recent transcripts into settled threads bound to their native sessions.
use super::paths::comparison_key;
use super::sources::{completed_sources, first_run_done, mark_first_run, record_source};
use super::{ImportProject, ImportSource, RecentThread, Scanner, SessionThread};
use crate::{ActorRegistry, CommandOrigin, RuntimeError, Store, StoreError};
use agent_domain::{
    Command, CommandId, Driver, ImportedMessage, ModelSelection, NativeBinding, Reply, ThreadId,
    Workspace,
};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

/// Models for transcripts that never name one (T3 `DEFAULT_MODEL_BY_PROVIDER`).
pub const DEFAULT_CODEX_MODEL: &str = "gpt-6-astra";
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-fable-5-1";

/// The Host's registered projects.
pub trait ProjectRoots: Send + Sync {
    fn projects(&self) -> Vec<ImportProject>;
}

/// Recent transcripts for one project root; `Scanner` in production.
pub trait SessionSource: Send + Sync {
    /// Blocking; the iterator reads lazily and may block on each item.
    fn recent_threads(
        &self,
        root: &Path,
        completed: &[ImportSource],
    ) -> Box<dyn Iterator<Item = RecentThread> + Send>;
}

impl SessionSource for Scanner {
    fn recent_threads(
        &self,
        root: &Path,
        completed: &[ImportSource],
    ) -> Box<dyn Iterator<Item = RecentThread> + Send> {
        Box::new(Scanner::recent_threads(self, root, completed))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub imported: usize,
    pub skipped: usize,
}
impl std::ops::AddAssign for ImportCounts {
    fn add_assign(&mut self, other: Self) {
        self.imported += other.imported;
        self.skipped += other.skipped;
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ImportError {
    #[error("project {0} does not exist")]
    ProjectNotFound(String),
    #[error("project {0} changed directories; scan again before importing history")]
    ProjectChanged(String),
    #[error("the transcript scan stopped: {0}")]
    Scan(String),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}
impl From<StoreError> for ImportError {
    fn from(error: StoreError) -> Self {
        Self::Runtime(error.into())
    }
}

pub fn import_thread_id(source: &ImportSource) -> String {
    format!("import:{}:{}", source.instance, source.session)
}

fn claude_session_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let hex = |group: &str, len: usize| {
        group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit())
    };
    groups.len() == 5
        && hex(groups[0], 8)
        && hex(groups[1], 4)
        && hex(groups[2], 4)
        && matches!(groups[2].as_bytes()[0], b'1'..=b'8')
        && hex(groups[3], 4)
        && matches!(
            groups[3].as_bytes()[0].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
        && hex(groups[4], 12)
}

fn import_command(project: &ImportProject, id: &ThreadId, thread: SessionThread) -> Command {
    let model = thread.model.unwrap_or_else(|| {
        match thread.source {
            Driver::Codex => DEFAULT_CODEX_MODEL,
            Driver::Claude => DEFAULT_CLAUDE_MODEL,
        }
        .to_owned()
    });
    Command::Import {
        thread: id.clone(),
        project: project.id.clone(),
        title: thread.title,
        selection: ModelSelection {
            instance: thread.instance.clone(),
            driver: thread.source,
            model,
            options: BTreeMap::new(),
        },
        // The session ran in the project checkout itself, never an isolated worktree.
        workspace: Some(Workspace {
            cwd: project.root.to_string_lossy().into_owned(),
            worktree_path: None,
            branch: None,
        }),
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        messages: thread
            .messages
            .into_iter()
            .map(|message| ImportedMessage {
                role: message.role,
                text: message.text,
                at: message.created_at,
            })
            .collect(),
        native: NativeBinding {
            instance: thread.instance,
            thread: thread.session,
            head: None,
        },
    }
}

pub struct Importer {
    registry: Arc<ActorRegistry>,
    source: Arc<dyn SessionSource>,
}

impl Importer {
    pub fn new(registry: Arc<ActorRegistry>, source: Arc<dyn SessionSource>) -> Self {
        Self { registry, source }
    }

    fn store(&self) -> &Store {
        &self.registry.context().store
    }

    /// Imports a project the client picked, refusing one whose root moved since its scan.
    pub async fn import(
        &self,
        projects: &dyn ProjectRoots,
        project: &str,
        expected_root: Option<&Path>,
    ) -> Result<ImportCounts, ImportError> {
        let project = projects
            .projects()
            .into_iter()
            .find(|candidate| candidate.id == project)
            .ok_or_else(|| ImportError::ProjectNotFound(project.to_owned()))?;
        if let Some(expected) = expected_root
            && comparison_key(&project.root) != comparison_key(expected)
        {
            return Err(ImportError::ProjectChanged(project.id));
        }
        self.import_project(&project).await
    }

    /// Imports recent sessions whose cwd is exactly the project root. Re-runs skip
    /// transcripts already recorded for the project without reading them.
    pub async fn import_project(
        &self,
        project: &ImportProject,
    ) -> Result<ImportCounts, ImportError> {
        let root = project.root.clone();
        let completed = self
            .store()
            .blocking(move |store| completed_sources(store, &root))
            .await?;
        let (source, root) = (self.source.clone(), project.root.clone());
        let mut outcomes =
            tokio::task::spawn_blocking(move || source.recent_threads(&root, &completed))
                .await
                .map_err(|error| ImportError::Scan(error.to_string()))?;
        let mut imported_threads = HashSet::new();
        let mut counts = ImportCounts::default();
        loop {
            let (rest, next) = tokio::task::spawn_blocking(move || {
                let next = outcomes.next();
                (outcomes, next)
            })
            .await
            .map_err(|error| ImportError::Scan(error.to_string()))?;
            outcomes = rest;
            let Some(outcome) = next else {
                return Ok(counts);
            };
            match outcome {
                RecentThread::Skipped => counts.skipped += 1,
                RecentThread::AlreadyImported { source } => {
                    imported_threads.insert(import_thread_id(&source));
                    counts.imported += 1;
                }
                RecentThread::Duplicate { source } => {
                    let id = import_thread_id(&source);
                    if imported_threads.contains(&id)
                        && let Ok(thread) = ThreadId::new(id)
                        && let Err(error) =
                            record_source(self.store(), &thread, &project.root, &source).await
                    {
                        tracing::debug!(%error, "could not record a duplicate transcript");
                    }
                }
                RecentThread::Importable { thread, source } => {
                    let (driver, session) = (thread.source, thread.session.clone());
                    match self.import_thread(project, thread, &source).await {
                        Ok(()) => {
                            imported_threads.insert(import_thread_id(&source));
                            counts.imported += 1;
                        }
                        Err(reason) => {
                            tracing::warn!(?driver, %session, %reason, "could not import an agent session");
                            counts.skipped += 1;
                        }
                    }
                }
            }
        }
    }

    async fn import_thread(
        &self,
        project: &ImportProject,
        thread: SessionThread,
        source: &ImportSource,
    ) -> Result<(), String> {
        if thread.source == Driver::Claude && !claude_session_id(&thread.session) {
            return Err("the Claude session cannot be resumed".into());
        }
        let id = ThreadId::new(import_thread_id(source)).map_err(|error| error.to_string())?;
        let lookup = id.clone();
        let existing = self
            .store()
            .blocking(move |store| store.shell(&lookup))
            .await
            .map_err(|error| error.to_string())?;
        if let Some((_, shell)) = existing {
            if shell.project != project.id {
                return Err(format!("the thread belongs to project {}", shell.project));
            }
            if shell.payload.get("imported") != Some(&serde_json::Value::Bool(true)) {
                return Err("the thread already has non-imported activity".into());
            }
            return record_source(self.store(), &id, &project.root, source)
                .await
                .map_err(|error| error.to_string());
        }
        // The commit itself rejects a native session another thread already owns.
        let command_id =
            CommandId::new(format!("{id}@{}", source.fingerprint())).map_err(|e| e.to_string())?;
        let committed = self
            .registry
            .dispatch(
                &id,
                command_id,
                import_command(project, &id, thread),
                CommandOrigin::Internal,
            )
            .await
            .map_err(|error| error.to_string())?;
        match committed.reply {
            Reply::Thread(_) | Reply::Ignored => {
                record_source(self.store(), &id, &project.root, source)
                    .await
                    .map_err(|error| error.to_string())
            }
            Reply::Rejected { reason } => Err(reason),
            other => Err(format!("unexpected reply {other:?}")),
        }
    }

    /// The first-run import into every registered project. Runs once per store;
    /// later starts return `None` without scanning.
    pub async fn first_run(
        &self,
        projects: &dyn ProjectRoots,
    ) -> Result<Option<ImportCounts>, ImportError> {
        if self.store().blocking(first_run_done).await? {
            return Ok(None);
        }
        let mut total = ImportCounts::default();
        for project in projects.projects() {
            match self.import_project(&project).await {
                Ok(counts) => total += counts,
                Err(error) => {
                    tracing::warn!(project = %project.id, %error, "could not import a project's agent sessions");
                }
            }
        }
        mark_first_run(self.store(), self.registry.context().clock.now()).await?;
        Ok(Some(total))
    }

    /// Starts the first-run import in the background; the Host calls this after recovery.
    pub fn spawn_first_run(
        self: Arc<Self>,
        projects: Arc<dyn ProjectRoots>,
    ) -> tokio::task::JoinHandle<Result<Option<ImportCounts>, ImportError>> {
        tokio::spawn(async move { self.first_run(projects.as_ref()).await })
    }
}

#[cfg(test)]
mod tests;
