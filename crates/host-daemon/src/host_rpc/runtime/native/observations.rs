use super::*;
use agent_protocol::execution::{ExecutionError, TurnStatus};

pub(super) fn native_context(
    state: &model::State,
    attempt_id: &str,
    now: &str,
    completed: bool,
) -> Result<Context, String> {
    let attempt = state
        .run_attempts
        .get(attempt_id)
        .ok_or("native attempt is missing")?;
    let turn = attempt
        .provider_turn_id
        .as_ref()
        .and_then(|id| state.provider_turns.get(id))
        .ok_or("native turn has not started")?;
    let native_turn_id = turn
        .native_turn_ref
        .as_ref()
        .and_then(|reference| reference.native_id.clone())
        .ok_or("native turn has no reference")?;
    Ok(Context {
        thread_id: state.thread.id.clone(),
        run_id: Some(attempt.run_id.clone()),
        root_node_id: attempt.root_node_id.clone(),
        provider_thread_id: attempt.provider_thread_id.clone(),
        provider_turn_id: turn.id.clone(),
        native_turn_id,
        provider: ProviderKind::from_instance_id(&attempt.provider_instance_id)?,
        now: now.into(),
        completed,
    })
}

fn failure(error: Option<&ExecutionError>) -> model::ProviderFailure {
    model::ProviderFailure {
        class: "provider".into(),
        message: error.map_or_else(
            || "Native execution failed".into(),
            |error| error.message.clone(),
        ),
        code: error.and_then(|error| error.provider_code.clone()),
        retryable: None,
        reset_at: None,
    }
}

impl Runtime {
    pub(in crate::host_rpc) async fn observe_execution(
        service: &HostRpcService,
        target: &SessionRef,
        event: AgentChange,
        execution: Uuid,
    ) -> Result<(), String> {
        match event {
            AgentChange::Session { session, change } => {
                let source = service.inner.conversation.register_native(target, &session);
                let _updates = source.updates.lock().await;
                Self::execution_change(service, &source, &session, change, execution).await
            }
            AgentChange::Renamed {
                name: Some(name), ..
            } => {
                let now = commands::at();
                service
                    .inner
                    .conversation
                    .storage
                    .observe_thread(&target.id, &[], &now, |state| {
                        Ok(model::Decision {
                            changes: vec![model::Change::Thread(model::Thread {
                                title: name,
                                updated_at: now.clone(),
                                ..state.thread.clone()
                            })],
                            ..Default::default()
                        })
                    })
                    .map(|_| ())
            }
            AgentChange::Request {
                session,
                origin,
                request,
            } => {
                Self::execution_request(service, target, &session, origin, request, execution).await
            }
            _ => Ok(()),
        }
    }

    async fn execution_change(
        service: &HostRpcService,
        source: &Arc<resources::Source>,
        native: &SessionRef,
        change: SessionChange,
        execution: Uuid,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let attempt_id = execution.to_string();
        let now = commands::at();
        let command_id = Uuid::new_v4().to_string();
        let driver = native.native_provider().map_err(str::to_owned)?.driver();
        if let Some(turn) = change.turn_id() {
            let reference = model::ProviderRef {
                driver: native
                    .native_provider()
                    .map_err(str::to_owned)?
                    .driver()
                    .into(),
                native_id: Some(turn.to_string()),
                strength: if native.provider == Some(ProviderKind::DeepSeek) {
                    model::NativeRefStrength::Weak
                } else {
                    model::NativeRefStrength::Strong
                },
                fingerprint: None,
                ordinal: None,
            };
            storage.observe_thread(&source.target.id, &[], &now, |state| {
                if !state.native_thread_matches(&attempt_id, driver, &native.id) {
                    return Ok(model::Decision::default());
                }
                model::started(
                    state,
                    &attempt_id,
                    &source.session_id,
                    reference,
                    executor::context(state, &command_id, &now),
                )
            })?;
        }
        let state = storage.thread_state(&source.target.id, &[])?;
        if !state.accepts_observation(&attempt_id, &source.session_id)
            || !state.native_thread_matches(&attempt_id, driver, &native.id)
        {
            return Ok(());
        }
        match change {
            SessionChange::Turn { turn, completed } => {
                if completed {
                    Self::flush_text(service, source).await?;
                }
                for item in turn.items.iter().flatten() {
                    Self::execution_item(
                        service,
                        source,
                        native,
                        &attempt_id,
                        Some(&turn.id),
                        item,
                        completed,
                    )
                    .await?;
                }
                if completed {
                    let tasks =
                        if matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted) {
                            source.cancel_background(execution);
                            Vec::new()
                        } else {
                            source.finish_foreground(
                                execution,
                                super::super::events::background(&turn),
                            )
                        };
                    Self::execution_background(service, source, execution, &tasks)?;
                    if tasks.is_empty() {
                        let completion = match turn.status {
                            TurnStatus::Failed => {
                                model::Completion::Failed(failure(turn.error.as_ref()))
                            }
                            TurnStatus::Interrupted => model::Completion::Interrupted,
                            _ => model::Completion::Completed,
                        };
                        storage.observe_thread(&source.target.id, &[], &now, |state| {
                            model::complete(
                                state,
                                &attempt_id,
                                &source.session_id,
                                completion,
                                executor::context(state, &command_id, &now),
                            )
                        })?;
                    }
                }
            }
            SessionChange::Item { turn_id, item } => {
                Self::execution_item(
                    service,
                    source,
                    native,
                    &attempt_id,
                    Some(&turn_id),
                    &item,
                    false,
                )
                .await?
            }
            SessionChange::Text {
                item_id,
                field,
                delta,
                ..
            } => {
                let context = native_context(&state, &attempt_id, &now, false)?;
                Self::ensure_text_item(
                    service,
                    source,
                    &attempt_id,
                    &context,
                    item_id.as_str(),
                    field,
                )?;
                let attempt = Attempt {
                    id: execution,
                    turn: context.native_turn_id.into(),
                    provider: context.provider,
                    source: native.id.clone(),
                };
                Self::text_delta(service, source, &attempt, item_id.as_str(), field, &delta)?;
            }
            SessionChange::Background { tasks } => {
                for (owner, tasks, native_ended) in source.roster(execution, tasks) {
                    Self::execution_background(service, source, owner, &tasks)?;
                    if native_ended && tasks.is_empty() {
                        storage.observe_thread(&source.target.id, &[], &now, |state| {
                            model::complete(
                                state,
                                &owner.to_string(),
                                &source.session_id,
                                model::Completion::Completed,
                                executor::context(state, &command_id, &now),
                            )
                        })?;
                    }
                }
            }
            SessionChange::Error { error, .. } => {
                let context = native_context(&state, &attempt_id, &now, true)?;
                let artifact = context.artifact(
                    &format!("error:{command_id}"),
                    None,
                    model::TurnItemStatus::Failed,
                    model::NodeKind::System,
                    model::TurnItemBody::Error {
                        failure: failure(Some(&error)),
                        retry: None,
                    },
                );
                Self::commit_artifacts(service, source, &attempt_id, vec![artifact], &now)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn ensure_text_item(
        service: &HostRpcService,
        source: &resources::Source,
        attempt_id: &str,
        context: &Context,
        native_id: &str,
        field: agent_protocol::session::TextField,
    ) -> Result<(), String> {
        use agent_protocol::session::TextField;
        let id = super::text_item_id(context.provider, &context.native_turn_id, native_id, field);
        if service
            .inner
            .conversation
            .storage
            .thread_item(&source.target, &id)?
            .is_some()
        {
            return Ok(());
        }
        let (key, kind, body) = match field {
            TextField::AssistantText => (
                native_id.into(),
                model::NodeKind::AssistantMessage,
                model::TurnItemBody::AssistantMessage {
                    message_id: model::derived_id(
                        "message",
                        &[
                            "provider",
                            context.provider.driver(),
                            "native-item",
                            native_id,
                        ],
                    ),
                    text: String::new(),
                    phase: None,
                    attachments: vec![],
                    streaming: true,
                },
            ),
            TextField::ReasoningSummary { index } => (
                json!([context.native_turn_id, native_id, "summary", index]).to_string(),
                model::NodeKind::Reasoning,
                model::TurnItemBody::Reasoning {
                    text: String::new(),
                    streaming: true,
                },
            ),
            TextField::ReasoningContent { index } => (
                json!([context.native_turn_id, native_id, "content", index]).to_string(),
                model::NodeKind::Reasoning,
                model::TurnItemBody::Reasoning {
                    text: String::new(),
                    streaming: true,
                },
            ),
            _ => return Ok(()),
        };
        Self::commit_artifacts(
            service,
            source,
            attempt_id,
            vec![context.artifact(
                &key,
                Some(native_id),
                model::TurnItemStatus::Running,
                kind,
                body,
            )],
            &context.now,
        )
    }

    async fn execution_item(
        service: &HostRpcService,
        source: &resources::Source,
        native: &SessionRef,
        attempt_id: &str,
        turn: Option<&agent_protocol::ids::TurnId>,
        item: &Item,
        completed: bool,
    ) -> Result<(), String> {
        // The parent's own user echo has already been committed at admission.
        if matches!(item.body(), ItemBody::UserMessage { .. }) {
            return Ok(());
        }
        let full = if item.is_deferred() {
            let turn = turn.ok_or("cannot read native body without a turn")?;
            Some(
                Self::read_native_body(
                    service,
                    &op::ReadItem {
                        thread_id: native.clone(),
                        turn_id: turn.clone(),
                        item_id: item.id.clone(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        let item = full.as_ref().unwrap_or(item);
        let now = commands::at();
        let state = service
            .inner
            .conversation
            .storage
            .thread_state(&source.target.id, &[])?;
        let context = native_context(&state, attempt_id, &now, completed)?;
        let artifacts = context.item(item)?;
        Self::commit_artifacts(service, source, attempt_id, artifacts, &now)
    }

    fn commit_artifacts(
        service: &HostRpcService,
        source: &resources::Source,
        attempt_id: &str,
        artifacts: Vec<Artifact>,
        now: &str,
    ) -> Result<(), String> {
        let item_ids = artifacts
            .iter()
            .map(|artifact| artifact.item.id.clone())
            .collect::<Vec<_>>();
        let events = service.inner.conversation.storage.observe_thread(
            &source.target.id,
            &item_ids,
            now,
            |state| {
                if !state.accepts_observation(attempt_id, &source.session_id) {
                    return Ok(model::Decision::default());
                }
                let mut decision = model::Decision::default();
                for mut artifact in artifacts {
                    if let Some(node) = state.nodes.get(&artifact.node.id) {
                        artifact.node.started_at =
                            node.started_at.clone().or(artifact.node.started_at);
                    }
                    if let Some(item) = state.turn_items.get(&artifact.item.id) {
                        artifact.item.started_at =
                            item.started_at.clone().or(artifact.item.started_at);
                    }
                    decision.changes.push(model::Change::Node(artifact.node));
                    let observed = if let Some(plan) = artifact.plan {
                        model::observe_plan(
                            &state.apply(&decision),
                            attempt_id,
                            &source.session_id,
                            plan,
                            artifact.item,
                        )?
                    } else {
                        model::observe_item(
                            &state.apply(&decision),
                            attempt_id,
                            &source.session_id,
                            artifact.item,
                        )?
                    };
                    decision.changes.extend(observed.changes);
                }
                Ok(decision)
            },
        )?;
        for event in events {
            let model::Change::TurnItem(item) = event.event.change else {
                continue;
            };
            source.replace_text(&item.id.clone().into());
            if let model::TurnItemBody::CommandExecution {
                output: Some(output),
                ..
            } = &item.body
            {
                service.inner.conversation.outputs.write(
                    Uuid::parse_str(attempt_id).map_err(|error| error.to_string())?,
                    records::OutputKey {
                        session: source.target.clone(),
                        item: item.id.into(),
                    },
                    output::Change::Replace {
                        text: output,
                        truncated: false,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn execution_background(
        service: &HostRpcService,
        source: &resources::Source,
        attempt: Uuid,
        tasks: &[agent_protocol::execution::BackgroundTask],
    ) -> Result<(), String> {
        let now = commands::at();
        service
            .inner
            .conversation
            .storage
            .observe_thread(&source.target.id, &[], &now, |state| {
                let Some(attempt) = state.run_attempts.get(&attempt.to_string()) else {
                    return Ok(model::Decision::default());
                };
                let thread = state
                    .provider_threads
                    .get(&attempt.provider_thread_id)
                    .ok_or("background provider thread is missing")?;
                let tasks = tasks
                    .iter()
                    .map(|task| model::PendingBackgroundTask {
                        kind: match task.kind {
                            agent_protocol::execution::BackgroundTaskKind::Command => "command",
                            agent_protocol::execution::BackgroundTaskKind::Subagent => "subagent",
                            agent_protocol::execution::BackgroundTaskKind::Monitor => "monitor",
                            agent_protocol::execution::BackgroundTaskKind::BackgroundTask => {
                                "background_task"
                            }
                        }
                        .into(),
                        task_id: task.id.clone(),
                        command: task.description.clone(),
                        item_id: None,
                        source_thread_id: None,
                        description: task.description.clone(),
                        parent_task_id: None,
                        native_item_id: Some(task.id.clone()),
                        awaited: Some(true),
                        provider_instance_id: Some(attempt.provider_instance_id.clone()),
                        child_thread_id: None,
                    })
                    .collect();
                Ok(model::Decision {
                    changes: vec![model::Change::ProviderThread(model::ProviderThread {
                        pending_background_tasks: tasks,
                        updated_at: now.clone(),
                        ..thread.clone()
                    })],
                    ..Default::default()
                })
            })
            .map(|_| ())
    }
}
