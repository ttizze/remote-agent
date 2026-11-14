//! Resource execution for committed conversation decisions. Provider I/O never
//! runs inside a conversation receiver or a database transaction.
use super::{
    owners::{Effect, Owners, Write},
    service::HostRpcService,
    storage::Storage,
};
use crate::providers::{AgentChange, Failure, RequestOrigin, SubmissionTarget};
use agent_protocol::{
    conversation::{
        self as decision, Action, Attempt, Command, Context, Event, Observation, Outcome, records,
    },
    ids::{ClientInputId, RequestId},
    operations as op,
    session::{PreparationStage, ProviderKind, SessionChange, SessionRef},
    sync::Cursor,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use uuid::Uuid;

mod bodies;
mod commands;
mod events;
mod executor;
mod imports;
pub(in crate::host_rpc) mod list;
mod native;
#[cfg(test)]
pub(in crate::host_rpc) mod native_fixture;
pub(in crate::host_rpc) mod output;
mod preparation;
pub(in crate::host_rpc) mod recovery;
#[cfg(test)]
mod replay;
mod resources;
mod rpc;
pub(in crate::host_rpc) mod stream;
#[cfg(test)]
mod t3_contract;
#[cfg(test)]
mod tests;
mod text;

pub(super) struct Runtime {
    pub storage: Arc<Storage>,
    pub owners: Arc<Owners>,
    effects: Mutex<Option<mpsc::UnboundedReceiver<Effect>>>,
    resources: Mutex<HashMap<SessionRef, Arc<resources::Source>>>,
    preparations: Mutex<HashMap<SessionRef, Arc<preparation::Preparation>>>,
    catalog_access: HashMap<ProviderKind, tokio::sync::RwLock<()>>,
    requests: Mutex<HashMap<RequestId, resources::PendingRequest>>,
    outputs: Arc<output::Outputs>,
    recovered: Mutex<HashMap<SessionRef, Arc<tokio::sync::OnceCell<()>>>>,
    updating_recovery: bool,
    /// A report committed by a newer update in this process supersedes old readers.
    /// The persisted updating marker also covers startup recovery and has a different lifetime.
    pub(in crate::host_rpc) update_report: AsyncMutex<bool>,
}

pub(super) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn execution_error(error: Failure) -> agent_protocol::execution::ExecutionError {
    error
        .execution
        .map(|error| *error)
        .unwrap_or(agent_protocol::execution::ExecutionError {
            message: error.message,
            ..Default::default()
        })
}

pub(in crate::host_rpc) fn storage_failure(error: String) -> Failure {
    Failure::new("conversation_storage_failed", error)
}

impl Runtime {
    pub(super) fn new(
        storage: Arc<Storage>,
        providers: impl IntoIterator<Item = ProviderKind>,
    ) -> Self {
        let updating_recovery = storage.updating().unwrap_or(false);
        let (owners, effects) = Owners::new(storage.clone());
        Self {
            storage,
            owners,
            effects: Mutex::new(Some(effects)),
            resources: Default::default(),
            preparations: Default::default(),
            catalog_access: providers
                .into_iter()
                .map(|provider| (provider, Default::default()))
                .collect(),
            requests: Default::default(),
            outputs: Default::default(),
            recovered: Default::default(),
            updating_recovery,
            update_report: Default::default(),
        }
    }

    pub(super) fn cancel_preparations(&self) {
        for preparation in self.preparations.lock().unwrap().values() {
            if let Some((_, cancel)) = &*preparation.active.lock().unwrap() {
                cancel.cancel();
            }
        }
    }

    fn recovery(&self, target: &SessionRef) -> Arc<tokio::sync::OnceCell<()>> {
        self.recovered
            .lock()
            .unwrap()
            .entry(target.clone())
            .or_default()
            .clone()
    }

    pub(super) fn start(service: &HostRpcService) {
        let Some(mut effects) = service.inner.conversation.effects.lock().unwrap().take() else {
            return;
        };
        let owner = Arc::downgrade(&service.inner);
        service.restore_setup_expiry();
        Self::start_list_updates(service);
        Self::start_recovery(service);
        Self::start_effects(service);
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            while let Some(effect) = effects.recv().await {
                while let Some(result) = workers.try_join_next() {
                    if let Err(error) = result {
                        tracing::error!(target: "agent", operation="conversation.executor", %error);
                    }
                }
                let Some(inner) = owner.upgrade() else {
                    break;
                };
                workers.spawn(async move {
                    let service = HostRpcService { inner };
                    if let Err(error) = Self::execute(&service, effect).await {
                        tracing::error!(target: "agent", operation="conversation.executor", %error);
                    }
                });
            }
            workers.abort_all();
        });
    }

    fn start_list_updates(service: &HostRpcService) {
        let owner = Arc::downgrade(&service.inner);
        let mut changed = service.inner.conversation.storage.changes();
        tokio::spawn(async move {
            let mut replica = records::Replica::new(agent_protocol::sync::Scope::List, 0);
            loop {
                let Some(inner) = owner.upgrade() else { break };
                let storage = inner.conversation.storage.clone();
                let cached = replica.cursor();
                let next = tokio::task::spawn_blocking(move || storage.list(cached))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
                    .and_then(|message| {
                        let published = !list::unchanged(&message);
                        replica
                            .apply(0, message)
                            .map(|next| (next, published))
                            .map_err(|error| error.to_string())
                    });
                let published = match next {
                    Ok((next, published)) => {
                        replica = next;
                        published
                    }
                    Err(error) => {
                        tracing::error!(target:"agent",operation="list.activity",%error);
                        replica = records::Replica::new(agent_protocol::sync::Scope::List, 0);
                        false
                    }
                };
                if published && let Some(apns) = inner.router.apns() {
                    let activity = records::list_rows_activity(replica.rows().values());
                    if inner.providers.startup_errors().next().is_none()
                        && inner
                            .conversation
                            .storage
                            .catalog_complete()
                            .unwrap_or(false)
                    {
                        apns.activate(activity.display)
                    } else {
                        apns.update(activity.display)
                    }
                }
                drop(inner);
                if changed.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    fn context(service: &HostRpcService, provider: Option<ProviderKind>) -> Context<'static> {
        Context {
            preparation: None,
            rewind_binding: None,
            imported_turns: &[],
            external_busy: false,
            now: now(),
            seed: Uuid::new_v4(),
            steer_ready: true,
            steers: provider
                .and_then(|provider| service.inner.providers.get(provider).ok())
                .is_some_and(|agent| agent.capabilities().steer),
        }
    }

    pub(super) async fn event(
        service: &HostRpcService,
        target: &SessionRef,
        event: Event,
    ) -> Result<(Outcome, Cursor), String> {
        let header = service.inner.conversation.storage.lookup(target)?;
        let provider = header
            .as_ref()
            .map(|header| header.state.provider)
            .or(target.provider);
        if let Some(header) = header {
            Self::recover(service, target, header.state.provider).await?;
        }
        let result = service
            .inner
            .conversation
            .owners
            .write(
                target,
                Write::Event(event, Self::context(service, provider), None),
            )
            .await?;
        service.inner.conversation.prune_requests(target);
        Ok(result)
    }

    async fn execute(service: &HostRpcService, effect: Effect) -> Result<(), String> {
        let Effect { target, action } = effect;
        let runtime = &service.inner.conversation;
        if !matches!(
            action,
            Action::Resume { .. } | Action::CancelPreparation { .. } | Action::WaitUntil(_)
        ) && !runtime.storage.state(&target)?.permits(&action)
        {
            return Ok(());
        }
        let native = match &action {
            Action::Deliver { attempt, .. }
            | Action::Resume { attempt, .. }
            | Action::Interrupt { attempt, .. }
            | Action::FenceDelivery { attempt, .. } => Some(
                SessionRef::new(attempt.provider, attempt.source.clone()).map_err(str::to_owned)?,
            ),
            _ => None,
        };
        let _retire = scopeguard::guard(native, |native| {
            if let Some(native) = native {
                runtime.retire_source(&native);
            }
        });
        match action {
            Action::Prepare {
                id,
                action,
                stage,
                cwd,
                provider,
            } => {
                Self::prepare_effect(service, &target, id, action, stage, cwd, provider).await?;
            }
            Action::Deliver {
                id,
                action,
                attempt,
                cwd,
                content,
                steer,
            } => {
                Self::deliver(service, &target, (id, action), attempt, cwd, content, steer).await?;
            }
            Action::Interrupt {
                ref attempt,
                action: token,
                background,
            } => {
                let source = runtime.source(&target, attempt);
                let _io = source.io.lock().await;
                if !runtime.storage.state(&target)?.permits(&action) {
                    return Ok(());
                }
                // Resume can still be waiting for the workspace lock. Keep Stop
                // pending until Resume cancels or the first native turn is bound.
                let Some(native) = runtime
                    .storage
                    .execution(&target, attempt.id)?
                    .and_then(|execution| execution.native_turn)
                else {
                    return Ok(());
                };
                let result = async {
                    service
                        .inner
                        .providers
                        .get(attempt.provider)?
                        .interrupt(&attempt.source, &native)
                        .await
                }
                .await;
                if background || result.is_err() {
                    let _updates = source.updates.lock().await;
                    let tasks = source.background(attempt.id);
                    if background && result.is_ok() && tasks.is_empty() {
                        Self::settle_command_outputs(
                            service,
                            &target,
                            attempt.id,
                            Some(native),
                            agent_protocol::execution::TurnStatus::Completed,
                        )
                        .await?;
                    }
                    Self::event(
                        service,
                        &target,
                        Event::InterruptResult {
                            attempt: attempt.id,
                            action: token,
                            background: tasks,
                            error: result.err().map(execution_error),
                        },
                    )
                    .await?;
                }
            }
            Action::Answer {
                attempt,
                id,
                action: token,
                answer,
            } => {
                Self::answer(service, &target, attempt, id, token, answer).await?;
            }
            Action::FenceDelivery {
                ref id,
                action: token,
                ref attempt,
            } => {
                let source = runtime.source(&target, attempt);
                let _io = source.io.lock().await;
                // Callback and write owners are locked until the fence commits.
                // Save their last buffered data while callbacks are still valid.
                let _updates = source.updates.lock().await;
                if !runtime
                    .storage
                    .input(&target, id)?
                    .is_some_and(|input| input.state.fencing(token, attempt))
                {
                    return Ok(());
                }
                Self::flush_text(service, &source).await?;
                Self::settle_command_outputs(
                    service,
                    &target,
                    attempt.id,
                    runtime
                        .storage
                        .execution(&target, attempt.id)?
                        .and_then(|execution| execution.native_turn),
                    agent_protocol::execution::TurnStatus::Interrupted,
                )
                .await?;
                let Some(unload) = runtime
                    .storage
                    .fence_delivery(&target, id, token, attempt)?
                else {
                    return Ok(());
                };
                source.cancel_background(attempt.id);
                drop(_updates);
                if unload {
                    let cleanup = async {
                        service
                            .inner
                            .providers
                            .get(attempt.provider)
                            .map_err(|error| error.to_string())?
                            .unload(&attempt.source)
                            .await
                            .map_err(|error| error.to_string())
                    }
                    .await;
                    if let Err(error) = cleanup {
                        tracing::warn!(target:"agent",operation="conversation.fence.unload",%error);
                    }
                }
                Self::event(
                    service,
                    &target,
                    Event::DeliveryFenced {
                        id: id.clone(),
                        action: token,
                    },
                )
                .await?;
            }
            Action::Resume {
                attempt,
                cwd,
                at,
                error,
            } => {
                Self::resume(service, &target, &attempt, &cwd, at, error).await?;
            }
            Action::WaitUntil(at) => {
                let delay = at.saturating_sub(now()).max(0) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                Self::event(service, &target, Event::Wake).await?;
            }
            Action::CancelPreparation { action, discard } => {
                let _retire = scopeguard::guard((), |_| runtime.retire_preparation(&target));
                let preparation = runtime.preparation(&target);
                if let Some((current, cancel)) = &*preparation.active.lock().unwrap()
                    && *current == action
                {
                    cancel.cancel();
                }
                // The same lock covers the gap between preparation stages and
                // prevents cleanup from racing checkout creation or setup I/O.
                let _io = preparation.io.lock().await;
                let workspace =
                    runtime
                        .storage
                        .header(&target)?
                        .details
                        .workspace
                        .filter(|workspace| {
                            workspace.setup.as_ref().is_some_and(|setup| {
                                setup.phase == agent_protocol::workspace::SetupPhase::Cancelled
                            })
                        });
                let terminal = workspace
                    .as_ref()
                    .filter(|workspace| {
                        discard
                            .as_ref()
                            .is_none_or(|(generation, _)| workspace.generation == *generation)
                    })
                    .and_then(|workspace| workspace.setup.as_ref())
                    .and_then(|setup| setup.terminal_handle.as_deref());
                if let Some((generation, checkout)) = discard {
                    service
                        .cleanup_setup_resources(
                            Some(checkout.path),
                            Some(checkout.branch),
                            terminal,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    service
                        .setup_update(
                            &target,
                            generation,
                            super::storage::workspace::SetupChange::Discarded,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                } else {
                    if let Some(handle) = terminal {
                        service
                            .cleanup_setup_resources(None, None, Some(handle))
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    if let Some(workspace) = workspace {
                        service.schedule_setup_expiry(target.clone(), *workspace);
                    }
                }
            }
        }
        Ok(())
    }
}
