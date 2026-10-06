use super::{WorkspaceFence, effect_command_id};
use crate::{
    ActorRegistry, CommandOrigin, Committed, Durability, EffectError, EffectHandler, EffectJob,
    HostOperations, RuntimeError,
};
use agent_domain::{Command, CommandId, Effect, EffectBody, EffectResult, Reply, ThreadId};
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
    pub workspaces: Arc<WorkspaceFence>,
}

impl ThreadCommands for RegistryThreads {
    fn dispatch(
        &self,
        thread: ThreadId,
        id: CommandId,
        command: Command,
    ) -> BoxFuture<'_, Result<Committed, RuntimeError>> {
        Box::pin(async move {
            // A created thread binds its workspace, which a file restore must see.
            let _binding = match &command {
                Command::AcceptFork { .. } | Command::AcceptDelegation { .. } => {
                    Some(self.workspaces.bind().await)
                }
                _ => None,
            };
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
/// A rejected command is not delivered: a rejected thread creation fails for good,
/// a rejected continuation is retried, and either one that never lands is
/// reported to the sending thread (`ThreadCommandFailed`).
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
    ) -> Result<Reply, RuntimeError> {
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
            .map(|committed| committed.reply)
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
            let reason = match self.deliver(&job.effect.id, thread, command).await {
                Ok(Reply::Rejected { reason }) => reason,
                Ok(_) => return Ok(None),
                Err(error) => return Err(EffectError::Retryable(error.to_string())),
            };
            match &**command {
                Command::AcceptFork { .. } | Command::AcceptDelegation { .. } => {
                    Err(EffectError::Permanent(reason))
                }
                Command::ContinueRestart { .. } => Err(EffectError::Retryable(reason)),
                // Other commands are the target's to accept or refuse.
                _ => {
                    tracing::debug!(%thread, %reason, "a thread refused a command from another thread");
                    Ok(None)
                }
            }
        })
    }

    fn failure(&self, effect: &Effect, error: &str) -> Option<EffectResult> {
        let EffectBody::SendToThread { thread, command } = &effect.body else {
            return None;
        };
        matches!(
            **command,
            Command::AcceptFork { .. }
                | Command::AcceptDelegation { .. }
                | Command::ContinueRestart { .. }
        )
        .then(|| EffectResult::ThreadCommandFailed {
            thread: thread.clone(),
            command: command.clone(),
            reason: error.to_owned(),
        })
    }
}
