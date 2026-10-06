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

/// Closes the thread's terminals (T3 `ResourceCleanupService`).
pub(crate) struct CleanupTerminals(pub(crate) ExecutorContext);

impl EffectHandler for CleanupTerminals {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }
    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            self.0
                .ops
                .cleanup_terminals(job.thread.clone())
                .await
                .map_err(retry)?;
            Ok(None)
        })
    }
}
