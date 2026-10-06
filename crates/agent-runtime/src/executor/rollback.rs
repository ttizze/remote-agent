use super::checkpoint::{SHARED_WORKSPACE_RESTORE_MESSAGE, restore_isolated};
use super::{ExecutorContext, retry};
use crate::{Durability, EffectError, EffectHandler, EffectJob, ExecError, PreparedRestore};
use agent_domain::{
    CheckpointScope, CommandId, Effect, EffectBody, EffectResult, Input, Reply, RestoreFiles,
    State, ThreadId, latest_executed_run, rollback_provider_changed,
};
use futures_util::future::BoxFuture;

pub use agent_domain::ROLLBACK_FAILED_MESSAGE;

/// One rollback request (T3 `CheckpointRollbackService`): the restored files are
/// staged with the originals kept aside and every provider rewinds to its absolute
/// head. Only once the rollback is recorded are the originals discarded and the
/// stale checkpoint refs deleted; any earlier failure puts the original files back.
pub(crate) struct Rollback(pub(crate) ExecutorContext);

impl EffectHandler for Rollback {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }

    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::Rollback {
                command,
                providers,
                restore,
                stale_file_refs,
            } = &job.effect.body
            else {
                return Err(EffectError::Permanent("not a rollback".into()));
            };
            let context = &self.0;
            let state = context.state(&job.thread).await?;
            let scope = rollback_scope(&state, restore.as_ref(), stale_file_refs);
            let Some(pending) = state
                .rollback
                .as_ref()
                .filter(|pending| &pending.command == command)
            else {
                // A previous attempt recorded the rollback and then stopped.
                if self.recorded(&job.thread, command).await? {
                    if restore.is_some()
                        && let Some(scope) = &scope
                    {
                        context
                            .ops
                            .finish_restore(scope.cwd.clone())
                            .await
                            .map_err(retry)?;
                    }
                    self.delete_stale_refs(scope.as_ref(), stale_file_refs)
                        .await?;
                }
                return Ok(None);
            };
            let failed = |message: &str| {
                Ok(Some(EffectResult::RollbackFailed {
                    command: command.clone(),
                    message: message.into(),
                }))
            };
            // T3 CheckpointRollbackService: the selection may have moved to
            // another instance while the rollback waited.
            if let Some(thread) = &state.thread
                && latest_executed_run(&state).map(|run| &run.selection.instance)
                    != Some(&thread.selection.instance)
            {
                return failed(&rollback_provider_changed(&pending.checkpoint, &thread.id));
            }
            let _fence = match restore {
                Some(_) => Some(context.workspaces.restore().await),
                None => None,
            };
            let prepared = match restore {
                Some(RestoreFiles { file_ref, .. }) => {
                    let Some(scope) = &scope else {
                        return failed(ROLLBACK_FAILED_MESSAGE);
                    };
                    if !restore_isolated(context, &job.thread, &state, &scope.cwd)
                        .await
                        .map_err(retry)?
                    {
                        return failed(SHARED_WORKSPACE_RESTORE_MESSAGE);
                    }
                    Some(
                        context
                            .ops
                            .prepare_restore(scope.cwd.clone(), file_ref.clone())
                            .await
                            .map_err(retry)?,
                    )
                }
                None => None,
            };
            let instances: Vec<String> = providers
                .iter()
                .map(|provider| provider.instance.clone())
                .collect();
            if instances
                .iter()
                .any(|instance| !pending.rewinding.contains(instance))
                && let Err(error) = context
                    .input(
                        &job.thread,
                        Input::RollbackRewindStarted {
                            command: command.clone(),
                            instances,
                        },
                    )
                    .await
            {
                return undo(&job.thread, prepared, retry(error)).await;
            }
            let mut bindings = Vec::new();
            let mut resets = Vec::new();
            for provider in providers {
                match context
                    .sessions
                    .rollback(&job.thread, &provider.instance, &provider.command)
                    .await
                {
                    Ok(Some(binding)) => bindings.push(binding),
                    Ok(None) => resets.push(provider.instance.clone()),
                    Err(error) => {
                        let error = match error {
                            ExecError::Retry(message) => EffectError::Retryable(message),
                            ExecError::Settle(result) => EffectError::Retryable(format!(
                                "provider rollback failed: {result:?}"
                            )),
                        };
                        return undo(&job.thread, prepared, error).await;
                    }
                }
            }
            for instance in resets {
                if let Err(error) = context
                    .input(&job.thread, Input::NativeSessionReset { instance })
                    .await
                {
                    return undo(&job.thread, prepared, retry(error)).await;
                }
            }
            let finished = EffectResult::RollbackFinished {
                bindings,
                command: command.clone(),
            };
            let recorded = match context.input(&job.thread, Input::Effect(finished)).await {
                Ok(committed) => committed.reply != Reply::Ignored,
                // The commit may have landed before the failure was reported.
                Err(error) => match self.recorded(&job.thread, command).await {
                    Ok(true) => true,
                    _ => return undo(&job.thread, prepared, retry(error)).await,
                },
            };
            if !recorded {
                return undo(
                    &job.thread,
                    prepared,
                    EffectError::Retryable("the rollback was not recorded".into()),
                )
                .await;
            }
            if let Some(prepared) = prepared {
                prepared.commit().await.map_err(retry)?;
            }
            self.delete_stale_refs(scope.as_ref(), stale_file_refs)
                .await?;
            Ok(None)
        })
    }

    fn failure(&self, effect: &Effect, _error: &str) -> Option<EffectResult> {
        let EffectBody::Rollback { command, .. } = &effect.body else {
            return None;
        };
        Some(EffectResult::RollbackFailed {
            command: command.clone(),
            message: ROLLBACK_FAILED_MESSAGE.into(),
        })
    }
}

impl Rollback {
    async fn recorded(&self, thread: &ThreadId, command: &CommandId) -> Result<bool, EffectError> {
        let (thread, command) = (thread.clone(), command.clone());
        self.0
            .store
            .blocking(move |store| store.rolled_back(&thread, &command))
            .await
            .map_err(retry)
    }

    async fn delete_stale_refs(
        &self,
        scope: Option<&CheckpointScope>,
        stale_file_refs: &[String],
    ) -> Result<(), EffectError> {
        if let Some(scope) = scope
            && !stale_file_refs.is_empty()
        {
            self.0
                .ops
                .delete_checkpoints(scope.cwd.clone(), stale_file_refs.to_vec())
                .await
                .map_err(retry)?;
        }
        Ok(())
    }
}

/// Puts the original files back after a failure before the rollback was recorded.
async fn undo(
    thread: &ThreadId,
    prepared: Option<Box<dyn PreparedRestore>>,
    error: EffectError,
) -> Result<Option<EffectResult>, EffectError> {
    if let Some(prepared) = prepared
        && let Err(undo) = prepared.undo().await
    {
        tracing::error!(%thread, %undo, "could not put back files set aside for a failed rollback");
    }
    Err(error)
}

/// The scope the rollback restores from, which also holds its stale refs.
fn rollback_scope(
    state: &State,
    restore: Option<&RestoreFiles>,
    stale_file_refs: &[String],
) -> Option<CheckpointScope> {
    match restore {
        Some(RestoreFiles { scope, .. }) => scope.clone(),
        None => state
            .rollback
            .as_ref()
            .and_then(|pending| {
                state
                    .checkpoints
                    .iter()
                    .find(|checkpoint| checkpoint.id == pending.checkpoint)
            })
            .or_else(|| {
                state
                    .checkpoints
                    .iter()
                    .find(|checkpoint| stale_file_refs.contains(&checkpoint.file_ref))
            })
            .and_then(|checkpoint| checkpoint.scope.clone()),
    }
}
