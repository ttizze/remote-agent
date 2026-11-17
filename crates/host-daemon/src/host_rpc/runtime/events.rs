use super::*;
use agent_protocol::{
    execution::{BackgroundTask, BackgroundTaskKind, ItemStatus},
    items::ItemBody,
    models::Turn,
};

pub(super) fn background(turn: &Turn) -> Vec<BackgroundTask> {
    turn.items
        .iter()
        .flatten()
        .filter(|item| item.status == ItemStatus::Running)
        .filter_map(|item| {
            let (kind, description) = match item.body() {
                ItemBody::CommandExecution { command, .. } => {
                    (BackgroundTaskKind::Command, command.clone())
                }
                ItemBody::Subagent { tool, prompt, .. } => (
                    BackgroundTaskKind::Subagent,
                    prompt.clone().unwrap_or_else(|| tool.clone()),
                ),
                _ => return None,
            };
            Some(BackgroundTask {
                id: item.id.to_string(),
                kind,
                description: Some(description),
            })
        })
        .collect()
}

enum CallbackOwner {
    Execution(SessionRef),
    External,
    Stale,
}

fn callback_owner(
    storage: &Storage,
    native: &SessionRef,
    execution: Uuid,
) -> Result<CallbackOwner, String> {
    let execution = execution.to_string();
    if let Some(target) = storage.attempt_owner(&execution, native)? {
        return Ok(CallbackOwner::Execution(target));
    }
    let Some(owner) = storage.attempt_thread_id(&execution)? else {
        return Ok(CallbackOwner::External);
    };
    // Native subagents can carry the parent execution token. A replaced source
    // of that same application attempt cannot become a new external turn.
    let child = storage
        .native_owner(native)?
        .as_ref()
        .map(|target| storage.thread_header(target))
        .transpose()?
        .flatten();
    let owner = SessionRef::new(None, owner).map_err(str::to_owned)?;
    let root = storage
        .thread_header(&owner)?
        .map(|thread| thread.lineage.root_thread_id);
    Ok(
        if child.is_some_and(|thread| {
            thread.lineage.relationship_to_parent.as_deref() == Some("subagent")
                && Some(&thread.lineage.root_thread_id) == root.as_ref()
        }) {
            CallbackOwner::External
        } else {
            CallbackOwner::Stale
        },
    )
}

impl Runtime {
    pub(in crate::host_rpc) async fn observe(
        service: &HostRpcService,
        event: AgentChange,
        execution: Option<Uuid>,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let native = match &event {
            AgentChange::Head { session, .. }
            | AgentChange::Session { session, .. }
            | AgentChange::Request { session, .. }
            | AgentChange::Renamed { session, .. }
            | AgentChange::TaskParent { session, .. } => Some(session.clone()),
            _ => None,
        };
        let source = if let Some(native) = &native {
            let held = runtime.resources.lock().unwrap().get(native).cloned();
            match held {
                Some(source) => Some(source),
                None => runtime
                    .storage
                    .native_owner(native)?
                    .map(|target| runtime.register_native(&target, native)),
            }
        } else {
            None
        };
        // Keep the same resource alive across the buffered fast path and its
        // fallback. A flush worker can retire an empty buffer between them.
        let source = scopeguard::guard(source, |source| {
            drop(source);
            if let Some(native) = &native {
                runtime.retire_source(native);
            }
        });
        let child_turn = match &event {
            AgentChange::Session { change, .. } => change.turn_id().map(|turn| {
                (
                    turn.to_string(),
                    matches!(
                        change,
                        SessionChange::Turn {
                            completed: false,
                            ..
                        }
                    ),
                )
            }),
            _ => None,
        };
        if Self::append_buffered_change(service, source.as_ref(), &event, execution).await? {
            return Ok(());
        }
        if let Some(execution) = execution
            && matches!(
                &event,
                AgentChange::Head { .. }
                    | AgentChange::Session { .. }
                    | AgentChange::Request { .. }
            )
            && let Some(native) = native.as_ref()
        {
            match callback_owner(&runtime.storage, native, execution)? {
                CallbackOwner::Execution(target) => {
                    Self::observe_execution(service, &target, event, execution).await?;
                    if let Some((turn, new_turn)) = child_turn {
                        runtime.storage.observe_native_child_turn(
                            native,
                            &turn,
                            new_turn,
                            &commands::at(),
                        )?;
                    }
                    return Ok(());
                }
                CallbackOwner::Stale => {
                    return if matches!(event, AgentChange::Request { .. }) {
                        Err("native request does not belong to this execution source".into())
                    } else {
                        Ok(())
                    };
                }
                CallbackOwner::External => {}
            }
        }
        match event {
            AgentChange::Session { session, change } => {
                let source = source.as_ref();
                if let (Some(source), SessionChange::Status { status }) = (source, &change) {
                    let provider = session.native_provider().map_err(str::to_owned)?;
                    let now = commands::at();
                    let command = Uuid::new_v4().to_string();
                    runtime.storage.observe_thread(
                        &source.target.application_id(),
                        &[],
                        &now,
                        |state| {
                            agent_protocol::orchestration::observe_native_status(
                                state,
                                provider.key(),
                                provider.driver(),
                                &session.id,
                                super::native_metadata::observed_status(*status),
                                agent_protocol::orchestration::Context {
                                    command_id: &command,
                                    now: &now,
                                    next_run_ordinal: state
                                        .next_run_ordinal()
                                        .ok_or("run ordinal exhausted")?,
                                    can_steer: false,
                                },
                            )
                        },
                    )?;
                    return Ok(());
                }
                if let Some(source) = source {
                    let _updates = source.updates.lock().await;
                    Self::external_change(service, source, &session, change).await?;
                    if let Some((turn, new_turn)) = child_turn {
                        runtime.storage.observe_native_child_turn(
                            &session,
                            &turn,
                            new_turn,
                            &commands::at(),
                        )?;
                    }
                }
            }
            AgentChange::Request {
                session,
                origin,
                request,
            } => {
                if let Some(source) = source.as_ref() {
                    Self::execution_request(
                        service,
                        &source.target,
                        &session,
                        origin,
                        request,
                        None,
                    )
                    .await?;
                }
            }
            change @ (AgentChange::Resolved { .. } | AgentChange::SourceClosed(_)) => {
                let (instance, native_id) = match change {
                    AgentChange::Resolved {
                        instance,
                        native_id,
                    } => (instance, Some(native_id)),
                    AgentChange::SourceClosed(instance) => (instance, None),
                    _ => unreachable!(),
                };
                let requests: Vec<_> = runtime
                    .requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, request)| {
                        request.origin.instance == instance
                            && native_id
                                .as_ref()
                                .is_none_or(|id| &request.origin.native_id == id)
                    })
                    .map(|(id, request)| (id.clone(), request.target.clone()))
                    .collect();
                for (id, target) in requests {
                    runtime.requests.lock().unwrap().remove(&id);
                    let now = commands::at();
                    runtime
                        .storage
                        .observe_thread(&target.id, &[], &now, |state| {
                            if native_id.is_some() {
                                agent_protocol::orchestration::resolve_request(
                                    state,
                                    id.as_str(),
                                    None,
                                    &now,
                                )
                            } else {
                                agent_protocol::orchestration::request_source_closed(
                                    state,
                                    id.as_str(),
                                    &now,
                                )
                            }
                        })?;
                }
            }
            AgentChange::Stopped { provider, error } => {
                Self::stopped(service, provider, &error).await?
            }
            AgentChange::Renamed {
                session,
                name: Some(name),
            } => {
                let target = source.as_ref().map(|source| &source.target);
                if let Some(target) = target {
                    let now = commands::at();
                    runtime.storage.observe_thread(
                        &target.application_id(),
                        &[],
                        &now,
                        |state| {
                            if native::external::provider(state, &session).is_none() {
                                return Ok(agent_protocol::orchestration::Decision::default());
                            }
                            Ok(agent_protocol::orchestration::titles::observe_native(
                                state, &name, &now,
                            ))
                        },
                    )?;
                }
            }
            AgentChange::TaskParent { session, parent } => {
                Self::native_parent(service, &parent, &session, execution, None).await?;
            }
            AgentChange::Renamed { name: None, .. } => {}
            AgentChange::Head {
                session,
                turn,
                head,
            } => {
                if let Some(source) = source.as_ref() {
                    Self::observe_native_cursor(service, source, &session, &turn, &head, None)
                        .await?;
                }
            }
        }
        Ok(())
    }
}
