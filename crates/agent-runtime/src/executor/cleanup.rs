use super::{ExecutorContext, retry};
use crate::{Durability, EffectError, EffectHandler, EffectJob};
use agent_domain::{EffectBody, EffectResult};
use futures_util::future::BoxFuture;

/// Deletes the thread's message attachments (T3 `attachment.cleanup`).
pub(crate) struct DeleteAttachments(pub(crate) ExecutorContext);

impl EffectHandler for DeleteAttachments {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::DeleteAttachments { paths } = &job.effect.body else {
                return Err(EffectError::Permanent("not an attachment cleanup".into()));
            };
            if !paths.is_empty() {
                self.0
                    .ops
                    .delete_attachments(job.thread.clone(), paths.clone())
                    .await
                    .map_err(retry)?;
            }
            Ok(None)
        })
    }
}

/// Closes the terminals of the thread's directory unless another live thread
/// works in the same directory.
pub(crate) struct CleanupTerminals(pub(crate) ExecutorContext);

impl EffectHandler for CleanupTerminals {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let context = &self.0;
            let state = context.state(&job.thread).await?;
            let Some(cwd) = context.cwd(&state) else {
                return Ok(None);
            };
            let others = context
                .store
                .blocking(|store| store.live_threads())
                .await
                .map_err(retry)?;
            let shared = others.iter().any(|other| {
                other.id != job.thread
                    && other
                        .workspace
                        .as_ref()
                        .map(|workspace| workspace.cwd.clone())
                        .or_else(|| context.ops.project(&other.project).map(|p| p.root))
                        .is_some_and(|other| other == cwd)
            });
            if !shared {
                context.ops.cleanup_terminals(cwd).await;
            }
            Ok(None)
        })
    }
}
