//! Host execution of every non-provider effect, plus the checkpoint baseline
//! taken before a provider turn starts.
mod checkpoint;
mod cleanup;
mod rollback;
mod send;
mod title;
mod workspace;

pub use checkpoint::*;
pub use rollback::*;
pub use send::*;
pub use workspace::*;

use crate::{
    ActorRegistry, CommandOrigin, Committed, EffectError, EffectHandlers, HostOperations,
    RuntimeError, SessionManager, Store, StoreError, with_session_handlers,
};
use agent_domain::{Command, CommandId, EffectBody, Input, RunId, State, ThreadId, Workspace};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// What the executors share.
#[derive(Clone)]
pub struct ExecutorContext {
    pub store: Store,
    pub registry: Arc<ActorRegistry>,
    pub sessions: Arc<SessionManager>,
    pub ops: Arc<dyn HostOperations>,
    pub workspaces: Arc<WorkspaceFence>,
}

/// Keeps a file restore's isolation check true until the restore ends: binding a
/// thread to a workspace waits while a restore runs, and a restore waits for
/// bindings in progress.
#[derive(Default)]
pub struct WorkspaceFence(RwLock<()>);
impl WorkspaceFence {
    pub(crate) async fn bind(&self) -> RwLockReadGuard<'_, ()> {
        self.0.read().await
    }
    pub(crate) async fn restore(&self) -> RwLockWriteGuard<'_, ()> {
        self.0.write().await
    }
}

/// Registers the session manager's handlers, a checkpoint baseline in front of
/// provider turn starts, and the executors of every other effect kind.
pub fn with_runtime_handlers(
    handlers: EffectHandlers,
    context: &ExecutorContext,
) -> EffectHandlers {
    let handlers = with_session_handlers(handlers, &context.sessions);
    let started =
        ["Provider.Start", "Provider.Compact"]
            .into_iter()
            .fold(handlers, |handlers, kind| {
                let inner = handlers.get(kind).cloned().expect("session handler");
                handlers.with(
                    kind,
                    Arc::new(BaselineBeforeStart {
                        context: context.clone(),
                        inner,
                    }),
                )
            });
    started
        .with(
            "CaptureCheckpoint",
            Arc::new(CaptureCheckpoint(context.clone())),
        )
        .with("Rollback", Arc::new(Rollback(context.clone())))
        .with(
            "PrepareWorkspace",
            Arc::new(PrepareWorkspace(context.clone())),
        )
        .with(
            "SendToThread",
            Arc::new(SendToThread::new(Arc::new(RegistryThreads {
                registry: context.registry.clone(),
                ops: context.ops.clone(),
                workspaces: context.workspaces.clone(),
            }))),
        )
        .with(
            "DeleteAttachments",
            Arc::new(cleanup::DeleteAttachments(context.clone())),
        )
        .with(
            "CleanupTerminals",
            Arc::new(cleanup::CleanupTerminals(context.clone())),
        )
        .with(
            "GenerateTitle",
            Arc::new(title::GenerateTitle(context.clone())),
        )
}

impl ExecutorContext {
    pub(crate) async fn state(&self, thread: &ThreadId) -> Result<Arc<State>, EffectError> {
        self.registry.state(thread).await.map_err(retry)
    }

    pub(crate) async fn input(
        &self,
        thread: &ThreadId,
        input: Input,
    ) -> Result<Committed, RuntimeError> {
        let mut retried = false;
        loop {
            let actor = self.registry.get_or_load(thread).await?;
            match actor.input(input.clone()).await {
                Err(RuntimeError::ActorStopped) if !retried => retried = true,
                result => return result,
            }
        }
    }

    pub(crate) async fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
    ) -> Result<Committed, RuntimeError> {
        self.registry
            .dispatch(thread, id, command, CommandOrigin::Internal)
            .await
    }

    /// The thread's working directory: its workspace, or its project's root.
    pub(crate) fn cwd(&self, state: &State) -> Option<String> {
        let thread = state.thread.as_ref()?;
        thread
            .workspace
            .as_ref()
            .map(|workspace| workspace.cwd.clone())
            .or_else(|| {
                self.ops
                    .project(&thread.project)
                    .map(|project| project.root)
            })
    }
}

pub(crate) fn retry(error: impl std::fmt::Display) -> EffectError {
    EffectError::Retryable(error.to_string())
}

/// The command id of a command an effect sends.
pub fn effect_command_id(effect_id: &str) -> CommandId {
    CommandId::new(format!("effect:{effect_id}")).expect("effect ids are nonempty")
}

/// A thread other than the one being checked, with what locates its workspace.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OtherThread {
    pub(crate) id: ThreadId,
    pub(crate) project: String,
    pub(crate) workspace: Option<Workspace>,
}

impl Store {
    /// Threads that are not deleted, archived ones included.
    pub(crate) fn live_threads(&self) -> Result<Vec<OtherThread>, StoreError> {
        self.read(|c| {
            let mut statement = c.prepare_cached(
                "SELECT thread_id, project, json_extract(payload, '$.workspace')
                 FROM thread_shells WHERE deleted = 0 ORDER BY thread_id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            rows.map(|row| {
                let (id, project, workspace) = row?;
                Ok(OtherThread {
                    id: crate::store::thread_id(id)?,
                    project,
                    workspace: workspace
                        .map(|workspace| serde_json::from_str(&workspace))
                        .transpose()?,
                })
            })
            .collect()
        })
    }

    /// Whether the rollback command's success was recorded.
    pub(crate) fn rolled_back(
        &self,
        thread: &ThreadId,
        command: &CommandId,
    ) -> Result<bool, StoreError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT 1 FROM facts WHERE thread_id = ?1 AND kind = 'RolledBack'
                     AND json_extract(payload, '$.RolledBack.command') = ?2",
                params![thread.as_str(), command.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
    }

    /// Runs whose checkpoint capture is still queued or running.
    pub(crate) fn capturing_runs(&self, thread: &ThreadId) -> Result<BTreeSet<RunId>, StoreError> {
        Ok(self
            .outbox(thread)?
            .into_iter()
            .filter(|row| {
                matches!(
                    row.status,
                    crate::EffectStatus::Pending | crate::EffectStatus::Running
                )
            })
            .filter_map(|row| match row.effect.body {
                EffectBody::CaptureCheckpoint { run, .. } => Some(run),
                _ => None,
            })
            .collect())
    }

    /// Every checkpoint scope directory the thread ever bound.
    pub(crate) fn checkpoint_scope_cwds(
        &self,
        thread: &ThreadId,
    ) -> Result<Vec<String>, StoreError> {
        self.read(|c| {
            let mut statement = c.prepare_cached(
                "SELECT DISTINCT json_extract(payload, '$.CheckpointScopeBound.scope.cwd')
                 FROM facts WHERE kind = 'CheckpointScopeBound' AND thread_id = ?1",
            )?;
            let rows = statement.query_map(params![thread.as_str()], |row| {
                row.get::<_, Option<String>>(0)
            })?;
            Ok(rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect())
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
