use super::{ExecError, SessionManager, attempt_finished, operation};
use crate::{Durability, EffectError, EffectHandler, EffectHandlers, EffectJob};
use agent_domain::{Effect, EffectBody, EffectResult, ProviderCommand, State};
use futures_util::future::BoxFuture;
use std::sync::Arc;

/// Provider effects tied to a live process; a Host restart cancels them.
pub const PROCESS_BOUND_PROVIDER_KINDS: [&str; 7] = [
    "Provider.Start",
    "Provider.Steer",
    "Provider.Interrupt",
    "Provider.Respond",
    "Provider.Compact",
    "Provider.SetModel",
    "Provider.SetRuntimeMode",
];
pub const FORK_NATIVE_KIND: &str = "ForkNative";
pub const DETACH_SESSIONS_KIND: &str = "DetachSessions";

/// Registers the session manager's handlers: process-bound provider commands,
/// and the replay-safe native fork and session detach.
pub fn with_session_handlers(
    handlers: EffectHandlers,
    sessions: &Arc<SessionManager>,
) -> EffectHandlers {
    let bound: Arc<dyn EffectHandler> = Arc::new(SessionEffects {
        sessions: sessions.clone(),
        durability: Durability::ProcessBound,
    });
    let replay_safe: Arc<dyn EffectHandler> = Arc::new(SessionEffects {
        sessions: sessions.clone(),
        durability: Durability::ReplaySafe,
    });
    PROCESS_BOUND_PROVIDER_KINDS
        .iter()
        .fold(handlers, |handlers, kind| {
            handlers.with(*kind, bound.clone())
        })
        .with(FORK_NATIVE_KIND, replay_safe.clone())
        .with(DETACH_SESSIONS_KIND, replay_safe)
}

pub struct SessionEffects {
    sessions: Arc<SessionManager>,
    durability: Durability,
}

impl EffectHandler for SessionEffects {
    fn durability(&self) -> Durability {
        self.durability
    }

    /// A finished attempt needs no provider work, except a stop (which also ends
    /// retained background work) and a steer (which becomes a follow-up turn).
    fn should_run(&self, state: &State, effect: &Effect) -> bool {
        match (&effect.body, &effect.attempt) {
            (
                EffectBody::Provider(
                    ProviderCommand::Interrupt { .. } | ProviderCommand::Steer { .. },
                ),
                _,
            )
            | (_, None) => true,
            (_, Some(attempt)) => !attempt_finished(state, attempt),
        }
    }

    fn run(&self, job: EffectJob) -> BoxFuture<'_, Result<Option<EffectResult>, EffectError>> {
        Box::pin(async move {
            let outcome = match &job.effect.body {
                EffectBody::Provider(command) => self
                    .sessions
                    .execute(
                        &job.thread,
                        &job.effect.id,
                        job.effect.attempt.as_ref(),
                        command,
                    )
                    .await
                    .map(|()| None),
                EffectBody::ForkNative { instance, provider } => {
                    self.sessions
                        .fork_native(
                            &job.thread,
                            &job.effect.id,
                            job.effect.attempt.as_ref(),
                            instance,
                            provider,
                        )
                        .await
                }
                EffectBody::DetachSessions {
                    revoke_credentials,
                    instance,
                    ..
                } => {
                    match instance {
                        Some(instance) => {
                            self.sessions
                                .detach_instance(&job.thread, instance, *revoke_credentials)
                                .await
                        }
                        None => self.sessions.detach(&job.thread, *revoke_credentials).await,
                    }
                    Ok(None)
                }
                other => {
                    return Err(EffectError::Permanent(format!(
                        "the session manager does not run {other:?}"
                    )));
                }
            };
            match outcome {
                Ok(result) => Ok(result),
                Err(ExecError::Settle(result)) => Ok(Some(*result)),
                Err(ExecError::Retry(message)) => Err(EffectError::Retryable(message)),
            }
        })
    }

    fn failure(&self, effect: &Effect, error: &str) -> Option<EffectResult> {
        match &effect.body {
            EffectBody::Provider(command) => Some(EffectResult::ProviderFailed {
                attempt: effect.attempt.clone()?,
                operation: operation(command)?,
                message: error.into(),
                message_id: match command {
                    ProviderCommand::Steer { message, .. } => Some(message.clone()),
                    _ => None,
                },
                turn_completed: false,
                session_lost: false,
            }),
            EffectBody::ForkNative { .. } => Some(EffectResult::ForkFailed {
                attempt: effect.attempt.clone()?,
                message: error.into(),
            }),
            _ => None,
        }
    }
}
