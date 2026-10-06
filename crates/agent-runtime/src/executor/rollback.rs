use super::checkpoint::{SHARED_WORKSPACE_RESTORE_MESSAGE, restore_isolated};
use super::{ExecutorContext, retry};
use crate::{Durability, EffectError, EffectHandler, EffectJob, ExecError};
use agent_domain::{Effect, EffectBody, EffectResult, Input, RestoreFiles};
use futures_util::future::BoxFuture;

pub const ROLLBACK_FAILED_MESSAGE: &str = "The provider could not roll back this conversation. Try again; if it keeps failing, check the provider and server logs.";

/// One rollback request (T3 `CheckpointRollbackService`): the restored files are
/// staged with the originals kept aside, every provider rewinds to its absolute
/// head, then the files are kept and stale checkpoint refs deleted. A provider
/// failure puts the original files back.
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
            let Some(pending) = state
                .rollback
                .as_ref()
                .filter(|pending| &pending.command == command)
            else {
                return Ok(None);
            };
            let failed = |message: &str| {
                Ok(Some(EffectResult::RollbackFailed {
                    command: command.clone(),
                    message: message.into(),
                }))
            };
            let scope = match restore {
                Some(RestoreFiles { scope, .. }) => scope.clone(),
                None => state
                    .checkpoints
                    .iter()
                    .find(|checkpoint| checkpoint.id == pending.checkpoint)
                    .and_then(|checkpoint| checkpoint.scope.clone()),
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
                        if let Some(prepared) = prepared
                            && let Err(undo) = prepared.undo().await
                        {
                            tracing::error!(thread = %job.thread, %undo,
                                "could not put back files set aside for a failed rollback");
                        }
                        return Err(match error {
                            ExecError::Retry(message) => EffectError::Retryable(message),
                            ExecError::Settle(result) => EffectError::Retryable(format!(
                                "provider rollback failed: {result:?}"
                            )),
                        });
                    }
                }
            }
            if let Some(prepared) = prepared {
                prepared.commit().await.map_err(retry)?;
            }
            for instance in resets {
                context
                    .input(&job.thread, Input::NativeSessionReset { instance })
                    .await
                    .map_err(retry)?;
            }
            if !stale_file_refs.is_empty()
                && let Some(scope) = &scope
            {
                context
                    .ops
                    .delete_checkpoints(scope.cwd.clone(), stale_file_refs.clone())
                    .await
                    .map_err(retry)?;
            }
            Ok(Some(EffectResult::RollbackFinished {
                bindings,
                command: command.clone(),
            }))
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
