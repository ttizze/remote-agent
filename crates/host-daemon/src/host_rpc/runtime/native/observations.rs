use super::*;
use agent_protocol::execution::{ExecutionError, TurnStatus};

pub(super) fn native_context(
    state: &model::State,
    attempt_id: &str,
    native_turn: &agent_protocol::ids::TurnId,
    now: &str,
    completed: bool,
) -> Result<Context, String> {
    let attempt = state
        .run_attempts
        .get(attempt_id)
        .ok_or("native attempt is missing")?;
    let turn = state
        .provider_turns
        .values()
        .filter(|turn| {
            turn.run_attempt_id.as_deref() == Some(attempt_id)
                && turn
                    .control_ref()
                    .and_then(|reference| reference.native_id.as_deref())
                    == Some(native_turn.as_str())
        })
        .max_by_key(|turn| turn.ordinal)
        .ok_or("native turn has not started")?;
    let native_turn_id = turn
        .control_ref()
        .and_then(|reference| reference.native_id.clone())
        .ok_or("native turn has no reference")?;
    Ok(Context {
        thread_id: state.thread.id.clone(),
        run_id: Some(attempt.run_id.clone()),
        root_node_id: attempt.root_node_id.clone(),
        provider_thread_id: attempt.provider_thread_id.clone(),
        provider_turn_id: turn.id.clone(),
        native_turn_id,
        native_thread_id: attempt
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.clone())
            .ok_or("native attempt source is missing")?,
        provider: ProviderKind::from_instance_id(&attempt.provider_instance_id)?,
        prompt_actor: state.thread.native_prompt_actor(),
        now: now.into(),
        completed,
    })
}

fn failure(error: Option<&ExecutionError>) -> model::ProviderFailure {
    model::ProviderFailure {
        class: model::ProviderFailure::class_for(
            &error
                .map(|error| error.category.clone())
                .unwrap_or_default(),
        )
        .into(),
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
        let _requests = scopeguard::guard(target, |target| {
            service.inner.conversation.prune_requests(target)
        });
        match event {
            AgentChange::Session { session, change } => {
                let source = service.inner.conversation.register_native(target, &session);
                let _updates = source.updates.lock().await;
                Self::execution_change(service, &source, &session, change, execution).await
            }
            AgentChange::Head {
                session,
                turn,
                head,
            } => {
                let source = service.inner.conversation.register_native(target, &session);
                Self::observe_native_cursor(
                    service,
                    &source,
                    &session,
                    &turn,
                    &head,
                    Some(execution),
                )
                .await
            }
            AgentChange::Request {
                session,
                origin,
                request,
            } => {
                Self::execution_request(service, target, &session, origin, request, Some(execution))
                    .await
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
            let reference = control_reference(
                native.native_provider().map_err(str::to_owned)?,
                turn.as_str(),
            );
            let events = storage.observe_thread(&source.target.id, &[], &now, |state| {
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
            service.schedule_setup_events(&events);
        }
        let state = storage.thread_state(&source.target.id, &[])?;
        let foreground = state.accepts_observation(&attempt_id, &source.session_id);
        if !state.native_thread_matches(&attempt_id, driver, &native.id)
            || (!foreground
                && (!state.accepts_background_observation(&attempt_id, &source.session_id)
                    || !matches!(
                        change,
                        SessionChange::Item { .. }
                            | SessionChange::Text { .. }
                            | SessionChange::Background { .. }
                    )))
        {
            return Ok(());
        }
        match change {
            SessionChange::Turn { turn, completed } => {
                if completed {
                    Self::flush_text(service, source)?;
                }
                for item in turn.items.iter().flatten() {
                    Self::execution_item(
                        service,
                        source,
                        native,
                        &attempt_id,
                        &turn.id,
                        item,
                        completed,
                    )
                    .await?;
                }
                if completed {
                    let image_context = native_context(&state, &attempt_id, &turn.id, &now, true)?;
                    Self::project_generated_images(
                        service,
                        source,
                        &attempt_id,
                        &image_context.provider_turn_id,
                    )?;
                    let completion = match turn.status {
                        TurnStatus::Failed => {
                            model::Completion::Failed(failure(turn.error.as_ref()))
                        }
                        TurnStatus::Interrupted => model::Completion::Interrupted,
                        _ => model::Completion::Completed,
                    };
                    if state
                        .run_attempts
                        .get(&attempt_id)
                        .and_then(|attempt| attempt.provider_turn_id.as_deref())
                        != Some(image_context.provider_turn_id.as_str())
                    {
                        storage.observe_thread(&source.target.id, &[], &now, |state| {
                            Ok(model::finish_provider_turn(
                                state,
                                &image_context.provider_turn_id,
                                &source.session_id,
                                completion,
                                &now,
                            ))
                        })?;
                        return Ok(());
                    }
                    if matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted) {
                        source.cancel_background(execution);
                    } else {
                        source
                            .finish_foreground(execution, super::super::events::background(&turn));
                    }
                    Self::execution_background(service, source, execution)?;
                    let tails = service
                        .inner
                        .conversation
                        .outputs
                        .checkpoints(&source.target, &attempt_id);
                    let events = storage.observe_thread(&source.target.id, &[], &now, |state| {
                        let decision = model::complete(
                            state,
                            &attempt_id,
                            &source.session_id,
                            completion,
                            executor::context(state, &command_id, &now),
                        )?;
                        Ok(decision.with_command_outputs(Some(&tails)))
                    })?;
                    Self::finish_output_events(service, &events);
                }
            }
            SessionChange::Item { turn_id, item } => {
                Self::execution_item(service, source, native, &attempt_id, &turn_id, &item, false)
                    .await?
            }
            SessionChange::Text {
                turn_id,
                item_id,
                field,
                delta,
                ..
            } => {
                let context = native_context(&state, &attempt_id, &turn_id, &now, false)?;
                if !foreground {
                    let id = super::text_item_id(
                        context.provider,
                        &context.native_turn_id,
                        item_id.as_str(),
                        field,
                    );
                    let Some(item) = state.turn_items.get(&id) else {
                        return Ok(());
                    };
                    if model::observe_item(&state, &attempt_id, &source.session_id, item.clone())?
                        .changes
                        .is_empty()
                    {
                        return Ok(());
                    }
                }
                Self::ensure_text_item(
                    service,
                    source,
                    Some(&attempt_id),
                    &context,
                    item_id.as_str(),
                    field,
                )?;
                Self::text_delta(
                    service,
                    source,
                    native,
                    super::super::text::Owner::Attempt(execution),
                    super::super::text::Delta {
                        turn: &context.native_turn_id,
                        item: item_id.as_str(),
                        field,
                        text: &delta,
                    },
                )?;
            }
            SessionChange::Background { mut tasks } => {
                if !foreground {
                    let known = source.background_owners();
                    tasks.retain(|task| {
                        known
                            .iter()
                            .any(|(_, tasks)| tasks.iter().any(|saved| saved.id == task.id))
                    });
                }
                source.roster(execution, tasks);
                Self::execution_background(service, source, execution)?;
            }
            SessionChange::Error { turn_id, error } => {
                let context = native_context(&state, &attempt_id, &turn_id, &now, true)?;
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
                Self::commit_artifacts(
                    service,
                    source,
                    Some(&attempt_id),
                    &context.native_thread_id,
                    vec![artifact],
                    &now,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn ensure_text_item(
        service: &HostRpcService,
        source: &resources::Source,
        attempt_id: Option<&str>,
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
                    message_id: super::item_identity(
                        "message",
                        context.provider,
                        &context.native_turn_id,
                        native_id,
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
            &context.native_thread_id,
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
        turn: &agent_protocol::ids::TurnId,
        item: &Item,
        completed: bool,
    ) -> Result<(), String> {
        // The parent's own user echo has already been committed at admission.
        if matches!(item.body(), ItemBody::UserMessage { .. }) {
            return Ok(());
        }
        let now = commands::at();
        let state = service
            .inner
            .conversation
            .storage
            .thread_state(&source.target.id, &[])?;
        let context = native_context(&state, attempt_id, turn, &now, completed)?;
        Self::canonical_native_item(service, source, native, Some(attempt_id), &context, item)
            .await?;
        if matches!(item.body(), ItemBody::ImageGeneration { .. })
            && state
                .provider_turns
                .get(&context.provider_turn_id)
                .is_some_and(|turn| turn.status != model::ProviderTurnStatus::Running)
        {
            Self::project_generated_images(service, source, attempt_id, &context.provider_turn_id)?;
        }
        Ok(())
    }

    pub(super) async fn canonical_native_item(
        service: &HostRpcService,
        source: &resources::Source,
        native: &SessionRef,
        attempt_id: Option<&str>,
        context: &Context,
        item: &Item,
    ) -> Result<(), String> {
        let full = if item.is_deferred() {
            match Self::read_native_body(
                service,
                &op::ReadItem {
                    thread_id: native.clone(),
                    turn_id: context.native_turn_id.clone().into(),
                    item_id: item.id.clone(),
                },
            )
            .await
            {
                Ok(item) => Some(item),
                // Native transcripts/output files can lag a terminal
                // notification. Keep its status and an explicitly partial
                // tool body, so this read cannot discard subsequent work.
                Err(_)
                    if matches!(
                        item.body(),
                        ItemBody::CommandExecution { .. }
                            | ItemBody::ToolCall { .. }
                            | ItemBody::FileChange { .. }
                    ) =>
                {
                    None
                }
                Err(error) => return Err(error.to_string()),
            }
        } else {
            None
        };
        let item = full.as_ref().unwrap_or(item);
        if matches!(item.body(), ItemBody::Subagent { .. }) {
            return Self::subagent_item(service, source, native, attempt_id, context, item).await;
        }
        let artifacts = if let ItemBody::ImageGeneration {
            saved_path,
            data,
            revised_prompt,
        } = item.body()
        {
            let mut status = super::item_status(item.status, context.completed);
            let mut saved_path = saved_path.clone();
            let mut failure = None;
            if status == model::TurnItemStatus::Completed
                && saved_path.is_none()
                && let Some(data) = data
            {
                let files = service.files().clone();
                let data = data.clone();
                let owner = source.target.application_id();
                let id = item.id.to_string();
                let stored = tokio::task::spawn_blocking(move || {
                    files.persist_generated_image(&owner, &id, &data)
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
                match stored {
                    Ok(path) => saved_path = Some(path),
                    Err(error) => {
                        status = model::TurnItemStatus::Failed;
                        failure = Some(error);
                    }
                }
            }
            let terminal = status.is_terminal();
            let output = terminal.then(|| {
                let mut output = json!({"status":status});
                if let Some(path) = saved_path {
                    output["savedPath"] = path.into();
                }
                if let Some(failure) = failure {
                    output["failure"] = failure.into();
                }
                output
            });
            let mut artifact = context.artifact(
                item.id.as_str(),
                Some(item.id.as_str()),
                status,
                model::NodeKind::ToolCall,
                model::TurnItemBody::DynamicTool {
                    tool_name: Some("image_gen.imagegen".into()),
                    input: revised_prompt
                        .as_ref()
                        .map_or_else(|| json!({}), |prompt| json!({"prompt":prompt})),
                    output,
                    output_omitted: None,
                    viewed_image_path: None,
                },
            );
            artifact.item.title = Some("Generate image".into());
            vec![artifact]
        } else {
            context.item(item)?
        };
        Self::commit_artifacts(
            service,
            source,
            attempt_id,
            &context.native_thread_id,
            artifacts,
            &context.now,
        )
    }

    pub(super) fn commit_artifacts(
        service: &HostRpcService,
        source: &resources::Source,
        attempt_id: Option<&str>,
        native_thread_id: &str,
        artifacts: Vec<Artifact>,
        now: &str,
    ) -> Result<(), String> {
        let mut staged_output = HashMap::new();
        let artifacts = artifacts
            .into_iter()
            .map(|mut artifact| {
                let (item, output) = service
                    .inner
                    .conversation
                    .outputs
                    .prepare_native_item(artifact.item);
                if let Some(output) = output {
                    staged_output.insert(item.id.clone(), output);
                }
                artifact.item = item;
                artifact
            })
            .collect::<Vec<_>>();
        let item_ids = artifacts
            .iter()
            .map(|artifact| artifact.item.id.clone())
            .collect::<Vec<_>>();
        let events = service.inner.conversation.storage.observe_thread(
            &source.target.id,
            &item_ids,
            now,
            |state| {
                let mut decision = model::Decision::default();
                for mut artifact in artifacts {
                    if state.thread.created_by == model::Actor::Agent
                        && state.thread.creation_source == model::CreationSource::Provider
                        && state.thread.lineage.relationship_to_parent.as_deref()
                            == Some("subagent")
                        && let model::TurnItemBody::UserMessage { created_by, .. } =
                            &mut artifact.item.body
                    {
                        *created_by = model::Actor::Agent;
                    }
                    if let Some(node) = state.nodes.get(&artifact.node.id) {
                        artifact.node.started_at =
                            node.started_at.clone().or(artifact.node.started_at);
                    }
                    if let Some(item) = state.turn_items.get(&artifact.item.id) {
                        artifact.item.started_at =
                            item.started_at.clone().or(artifact.item.started_at);
                    }
                    let observed = match (attempt_id, artifact.plan) {
                        (Some(attempt), Some(plan)) => model::observe_plan(
                            &state.apply(&decision),
                            attempt,
                            &source.session_id,
                            plan,
                            artifact.item,
                        )?,
                        (Some(attempt), None) => model::observe_item(
                            &state.apply(&decision),
                            attempt,
                            &source.session_id,
                            artifact.item,
                        )?,
                        (None, Some(plan)) => {
                            let turn = artifact
                                .item
                                .provider_turn_id
                                .clone()
                                .ok_or("native plan has no turn")?;
                            model::observe_external_plan(
                                &state.apply(&decision),
                                &turn,
                                native_thread_id,
                                plan,
                                artifact.item,
                            )?
                        }
                        (None, None) => {
                            let turn = artifact
                                .item
                                .provider_turn_id
                                .clone()
                                .ok_or("native item has no turn")?;
                            model::observe_external_item(
                                &state.apply(&decision),
                                &turn,
                                native_thread_id,
                                artifact.item,
                            )?
                        }
                    };
                    if !observed.changes.is_empty() {
                        decision.changes.push(model::Change::Node(artifact.node));
                        decision.changes.extend(observed.changes);
                    }
                }
                Ok(decision)
            },
        )?;
        for event in events {
            let model::Change::TurnItem(item) = event.event.change else {
                continue;
            };
            source.replace_text(&item.id.clone().into());
            if let Some(attempt_id) = attempt_id
                && item.status == model::TurnItemStatus::Completed
                && let model::TurnItemBody::DynamicTool {
                    tool_name: Some(name),
                    output: Some(output),
                    ..
                } = &item.body
                && name == "image_gen.imagegen"
                && output
                    .get("savedPath")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| !path.trim().is_empty())
                && output.get("failure").is_none_or(serde_json::Value::is_null)
                && let Some(turn) = &item.provider_turn_id
            {
                source.record_generated_image(
                    Uuid::parse_str(attempt_id).map_err(|error| error.to_string())?,
                    turn,
                    &item.id,
                );
            }

            if let model::TurnItemBody::CommandExecution { output, .. } = &item.body {
                let key = records::OutputKey {
                    session: source.target.clone(),
                    item: item.id.clone().into(),
                };
                if let Some((raw, truncated)) = staged_output.remove(&item.id) {
                    let scope = attempt_id
                        .or(item.provider_turn_id.as_deref())
                        .ok_or("command output has no execution scope")?;
                    service.inner.conversation.outputs.write(
                        scope,
                        key,
                        output::Change::Replace {
                            text: &raw,
                            truncated,
                        },
                    )?;
                } else {
                    service
                        .inner
                        .conversation
                        .outputs
                        .finish(&key, output.as_deref().unwrap_or_default());
                }
            }
        }
        Ok(())
    }

    pub(in crate::host_rpc::runtime) fn finish_output_events(
        service: &HostRpcService,
        events: &[model::StoredEvent],
    ) {
        for event in events {
            let model::Change::TurnItem(item) = &event.event.change else {
                continue;
            };
            if item.status.is_terminal()
                && let model::TurnItemBody::CommandExecution { output, .. } = &item.body
            {
                service.inner.conversation.outputs.finish(
                    &records::OutputKey {
                        session: SessionRef {
                            provider: None,
                            id: event.event.thread_id.clone(),
                        },
                        item: item.id.clone().into(),
                    },
                    output.as_deref().unwrap_or_default(),
                );
            }
        }
    }

    fn project_generated_images(
        service: &HostRpcService,
        source: &resources::Source,
        attempt_id: &str,
        turn: &str,
    ) -> Result<(), String> {
        let attempt = Uuid::parse_str(attempt_id).map_err(|error| error.to_string())?;
        let pending = source.generated_images(attempt, turn);
        if pending.is_empty() {
            return Ok(());
        }
        let storage = &service.inner.conversation.storage;
        let mut bodies = pending.clone();
        bodies.extend(storage.assistant_item_ids(&source.target, turn)?);
        let now = commands::at();
        storage.observe_thread(&source.target.id, &bodies, &now, |state| {
            model::project_generated_images(
                state,
                attempt_id,
                &source.session_id,
                turn,
                &pending,
                &now,
            )
        })?;
        source.clear_generated_images(attempt, turn);
        Ok(())
    }

    fn execution_background(
        service: &HostRpcService,
        source: &resources::Source,
        attempt: Uuid,
    ) -> Result<(), String> {
        let now = commands::at();
        let owners = source.background_owners();
        service
            .inner
            .conversation
            .storage
            .observe_thread(&source.target.id, &[], &now, |state| {
                let id = attempt.to_string();
                if !state.accepts_observation(&id, &source.session_id)
                    && !state.accepts_background_observation(&id, &source.session_id)
                {
                    return Ok(model::Decision::default());
                }
                let attempt = &state.run_attempts[&id];
                let thread = state
                    .provider_threads
                    .get(&attempt.provider_thread_id)
                    .ok_or("background provider thread is missing")?;
                // Keep the provider's start order; a task reported twice appears once.
                let mut tasks: Vec<model::PendingBackgroundTask> = Vec::new();
                for (owner, pending) in owners {
                    let id = owner.to_string();
                    let Some(owner) = state
                        .run_attempts
                        .get(&id)
                        .filter(|owner| owner.provider_thread_id == thread.id)
                    else {
                        continue;
                    };
                    if !state.accepts_observation(&id, &source.session_id)
                        && !state.accepts_background_observation(&id, &source.session_id)
                    {
                        continue;
                    }
                    for task in pending {
                        let item = state.turn_items.values().find(|item| {
                            item.run_id.as_ref() == Some(&owner.run_id)
                                && item
                                    .native_item_ref
                                    .as_ref()
                                    .and_then(|reference| reference.native_id.as_deref())
                                    == Some(task.id.as_str())
                        });
                        if item.is_some_and(|item| {
                            !matches!(
                                item.status,
                                model::TurnItemStatus::Pending
                                    | model::TurnItemStatus::Running
                                    | model::TurnItemStatus::Waiting
                            )
                        }) {
                            continue;
                        }
                        if tasks.iter().any(|saved| saved.task_id == task.id) {
                            continue;
                        }
                        tasks.push(model::PendingBackgroundTask {
                            kind: match task.kind {
                                agent_protocol::execution::BackgroundTaskKind::Command => "command",
                                agent_protocol::execution::BackgroundTaskKind::Subagent => {
                                    "subagent"
                                }
                                agent_protocol::execution::BackgroundTaskKind::Monitor => "monitor",
                                agent_protocol::execution::BackgroundTaskKind::BackgroundTask => {
                                    "background_task"
                                }
                            }
                            .into(),
                            task_id: task.id.clone(),
                            command: task.description.clone(),
                            item_id: item.map(|item| item.id.clone()),
                            source_thread_id: None,
                            description: task.description,
                            parent_task_id: None,
                            native_item_id: Some(task.id),
                            awaited: None,
                            provider_instance_id: Some(owner.provider_instance_id.clone()),
                            child_thread_id: None,
                        });
                    }
                }
                let pending_background_tasks = tasks;
                if thread.pending_background_tasks == pending_background_tasks {
                    return Ok(model::Decision::default());
                }
                Ok(model::Decision {
                    changes: vec![model::Change::ProviderThread(model::ProviderThread {
                        pending_background_tasks,
                        updated_at: now.clone(),
                        ..thread.clone()
                    })],
                    ..Default::default()
                })
            })
            .map(|_| ())
    }
}
