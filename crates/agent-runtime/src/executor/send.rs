use super::effect_command_id;
use crate::{
    ActorRegistry, CommandOrigin, Committed, Durability, EffectError, EffectHandler, EffectJob,
    HostOperations, RuntimeError,
};
use agent_domain::{Command, CommandId, EffectBody, EffectResult, ThreadId};
use futures_util::future::BoxFuture;
use std::sync::Arc;

/// The target threads of `SendToThread`.
pub trait ThreadCommands: Send + Sync {
    fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Committed, RuntimeError>>;
    /// The current restart-continuation setting of the thread's project.
    fn continue_after_restart(&self, thread: ThreadId)
    -> BoxFuture<'_, Result<bool, RuntimeError>>;
}

pub struct RegistryThreads {
    pub registry: Arc<ActorRegistry>,
    pub ops: Arc<dyn HostOperations>,
}

impl ThreadCommands for RegistryThreads {
    fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Committed, RuntimeError>> {
        Box::pin(async move {
            self.registry
                .dispatch(&thread, id, command, CommandOrigin::Internal)
                .await
        })
    }
    fn continue_after_restart(
        &self,
        thread: ThreadId,
    ) -> BoxFuture<'_, Result<bool, RuntimeError>> {
        Box::pin(async move {
            let state = self.registry.state(&thread).await?;
            Ok(state
                .thread
                .as_ref()
                .is_some_and(|thread| self.ops.continue_after_restart(&thread.project)))
        })
    }
}

/// A command from one thread's state machine to another thread (or itself). The
/// command id `effect:{effect}` makes a retried delivery return the first result.
pub struct SendToThread {
    threads: Arc<dyn ThreadCommands>,
}

impl SendToThread {
    pub fn new(threads: Arc<dyn ThreadCommands>) -> Self {
        Self { threads }
    }

    async fn deliver(
        &self,
        effect_id: &str,
        target: &ThreadId,
        command: &Command,
    ) -> Result<(), RuntimeError> {
        let command = match command {
            // The effect was recorded with `enabled: true`; the setting may have changed.
            Command::ContinueRestart { source, .. } => Command::ContinueRestart {
                source: source.clone(),
                enabled: self.threads.continue_after_restart(target.clone()).await?,
            },
            other => other.clone(),
        };
        self.threads
            .dispatch(target.clone(), effect_command_id(effect_id), command)
            .await
            .map(|_| ())
    }
}

impl EffectHandler for SendToThread {
    fn durability(&self) -> Durability {
        Durability::ReplaySafe
    }

    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let EffectBody::SendToThread { thread, command } = &job.effect.body else {
                return Err(EffectError::Permanent("not a thread command".into()));
            };
            let Err(error) = self.deliver(&job.effect.id, thread, command).await else {
                return Ok(None);
            };
            // A continuation that can never run settles its delegation as declined.
            if !job.will_retry
                && let Command::ContinueRestart { source, .. } = &**command
            {
                let declined = CommandId::new(format!("effect:{}:declined", job.effect.id))
                    .expect("effect ids are nonempty");
                let command = Command::ContinueRestart {
                    source: source.clone(),
                    enabled: false,
                };
                if let Err(declined) = self
                    .threads
                    .dispatch(thread.clone(), declined, command)
                    .await
                {
                    tracing::warn!(%thread, error = %declined, "could not decline a failed restart continuation");
                }
            }
            Err(EffectError::Retryable(error.to_string()))
        })
    }
}
