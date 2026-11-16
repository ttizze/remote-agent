use super::*;

#[derive(Clone, Copy)]
struct Spawn<'a> {
    item_id: &'a str,
    prompt: Option<&'a str>,
    reported_model: Option<&'a str>,
    native_task: Option<&'a str>,
    result: Option<&'a serde_json::Value>,
}

impl Runtime {
    pub(in crate::host_rpc::runtime) async fn native_parent(
        service: &HostRpcService,
        parent: &SessionRef,
        child_native: &SessionRef,
        execution: Option<Uuid>,
        reported_model: Option<&str>,
    ) -> Result<Option<(SessionRef, model::ProviderThread)>, String> {
        if child_native.provider != parent.provider || child_native == parent {
            return Ok(None);
        }
        let runtime = &service.inner.conversation;
        let Some(target) = runtime.storage.native_owner(parent)? else {
            return Ok(None);
        };
        let header = runtime
            .storage
            .thread_header(&target)?
            .ok_or("native parent header is missing")?;
        let provider = parent.native_provider().map_err(str::to_owned)?;
        let accepted_model = if let Some(execution) = execution {
            let state = runtime
                .storage
                .thread_state(&target.application_id(), &[])?;
            state
                .run_attempts
                .get(&execution.to_string())
                .filter(|attempt| {
                    state.native_thread_matches(&attempt.id, provider.driver(), &parent.id)
                })
                .and_then(|attempt| state.runs.get(&attempt.run_id))
                .map(|run| run.model_selection.clone())
        } else {
            None
        };
        let selected = accepted_model.as_ref().or_else(|| {
            (header.model_selection.instance_id == provider.key())
                .then_some(&header.model_selection)
        });
        let reported_model = reported_model.filter(|model| !model.trim().is_empty());
        let model = reported_model.or_else(|| selected.map(|selected| selected.model.as_str()));
        let cwd = header
            .worktree_path
            .clone()
            .or(runtime.storage.thread_project_root(&target)?);
        let response = agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(child_native.clone()),
                parent_id: Some(target),
                cwd,
                can_accept_direct_input: Some(false),
                ..Default::default()
            },
            model: model.map(|model| agent_protocol::models::ModelRef {
                provider,
                id: model.into(),
            }),
            ..Default::default()
        };
        let child = runtime
            .storage
            .native_owner(child_native)?
            .unwrap_or_else(|| SessionRef {
                provider: None,
                id: child_native.application_id(),
            });
        let (thread, native) =
            Self::native_header_records(service, &child, child_native, &response, &commands::at())
                .await
                .map_err(|error| error.to_string())?;
        let Some(native) = runtime.storage.observe_native_child(
            parent,
            execution,
            &thread,
            &native,
            reported_model,
            &commands::at(),
        )?
        else {
            return Ok(None);
        };
        runtime.register_native(&child, child_native);
        Ok(Some((child, native)))
    }
}

fn task_status(status: model::TurnItemStatus) -> model::SubagentStatus {
    use model::{SubagentStatus as S, TurnItemStatus as I};
    match status {
        I::Idle => S::Idle,
        I::Pending => S::Pending,
        I::Running => S::Running,
        I::Waiting => S::Waiting,
        I::Completed => S::Completed,
        I::Failed => S::Failed,
        I::Cancelled => S::Cancelled,
        I::Interrupted => S::Interrupted,
    }
}

fn reported_status(state: &agent_protocol::items::SubagentState) -> model::TurnItemStatus {
    use agent_protocol::execution::TurnStatus as T;
    match state.native_status.as_deref() {
        Some("pendingInit") => return model::TurnItemStatus::Pending,
        Some("shutdown") => return model::TurnItemStatus::Cancelled,
        Some("errored" | "notFound") => return model::TurnItemStatus::Failed,
        _ => {}
    }
    match state.status {
        T::Completed => model::TurnItemStatus::Completed,
        T::Failed => model::TurnItemStatus::Failed,
        T::Interrupted => model::TurnItemStatus::Interrupted,
        _ => model::TurnItemStatus::Running,
    }
}

fn result_text(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

impl Runtime {
    pub(super) async fn subagent_item(
        service: &HostRpcService,
        source: &resources::Source,
        native: &SessionRef,
        attempt: Option<&str>,
        context: &Context,
        item: &Item,
    ) -> Result<(), String> {
        let ItemBody::Subagent {
            tool,
            prompt,
            model,
            receivers,
            states,
            agent_id,
            result,
            ..
        } = item.body()
        else {
            unreachable!()
        };
        let spawn = matches!(
            tool.as_str(),
            "spawnAgent" | "spawn_agent" | "Agent" | "Task" | "subAgentActivity.started"
        );
        if !spawn {
            for state in states {
                Self::subagent_state(service, source, native, attempt, context, state)?;
            }
            if tool.starts_with("subAgentActivity.") {
                return Ok(());
            }
            return Self::collaboration_item(service, source, attempt, context, item);
        }
        let mut children = receivers.clone();
        for state in states {
            if !children.contains(&state.session) {
                children.push(state.session.clone());
            }
        }
        // Codex has not identified a child when the spawn call merely starts.
        if children.is_empty() && context.provider == ProviderKind::Codex {
            return Ok(());
        }
        let spawn = Spawn {
            item_id: item.id.as_str(),
            prompt: prompt.as_deref(),
            reported_model: model.as_deref(),
            native_task: agent_id.as_deref(),
            result: result.as_ref(),
        };
        if children.is_empty() {
            let status = super::item_status(item.status, context.completed);
            return Self::commit_subagent(
                service,
                &source.session_id,
                attempt,
                context,
                spawn,
                None,
                status,
            );
        }
        for child_native in children {
            let execution = attempt
                .map(uuid::Uuid::parse_str)
                .transpose()
                .map_err(|error| error.to_string())?;
            let Some((child, provider)) =
                Self::native_parent(service, native, &child_native, execution, model.as_deref())
                    .await?
            else {
                continue;
            };
            let state = states.iter().find(|state| state.session == child_native);
            let status = state.map(reported_status).unwrap_or_else(|| {
                if matches!(
                    item.status,
                    ItemStatus::Failed | ItemStatus::Declined | ItemStatus::Interrupted
                ) {
                    super::item_status(item.status, false)
                } else {
                    model::TurnItemStatus::Running
                }
            });
            let message = state
                .and_then(|state| state.message.as_ref())
                .map(|message| json!(message));
            Self::commit_subagent(
                service,
                &source.session_id,
                attempt,
                context,
                Spawn {
                    result: message.as_ref().or(result.as_ref()),
                    ..spawn
                },
                Some((&child, &provider, child_native.id.as_str())),
                status,
            )?;
        }
        Ok(())
    }

    fn subagent_state(
        service: &HostRpcService,
        source: &resources::Source,
        native: &SessionRef,
        attempt: Option<&str>,
        context: &Context,
        reported: &agent_protocol::items::SubagentState,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let Some(child) = storage.native_owner(&reported.session)? else {
            return Ok(());
        };
        let child_state = storage.thread_state(&child.application_id(), &[])?;
        let Some(provider) = super::external::provider(&child_state, &reported.session) else {
            return Ok(());
        };
        let id = model::derived_id(
            "node",
            &[
                "provider",
                context.provider.driver(),
                "native-child",
                &child.application_id(),
            ],
        );
        let item_id = model::derived_id("turn-item", &["subagent", &id]);
        storage
            .observe_thread(
                &context.thread_id,
                std::slice::from_ref(&item_id),
                &context.now,
                |state| {
                    let owns_source = attempt.map_or_else(
                        || super::external::provider(state, native).is_some(),
                        |attempt| {
                            state.owns_native_source(
                                attempt,
                                &source.session_id,
                                context.provider.driver(),
                                &native.id,
                            )
                        },
                    );
                    if !owns_source {
                        return Ok(model::Decision::default());
                    }
                    let Some(previous) = state.subagents.get(&id).filter(|task| {
                        task.child_thread_id.as_deref() == Some(&child.application_id())
                            && task.provider_thread_id.as_deref() == Some(&provider.id)
                    }) else {
                        return Ok(model::Decision::default());
                    };
                    if previous.driver != context.provider.driver() {
                        return Ok(model::Decision::default());
                    }
                    model::observe_subagent_progress(
                        state,
                        &id,
                        reported_status(reported),
                        reported.message.as_deref(),
                        false,
                        None,
                        &context.now,
                    )
                },
            )
            .map(|_| ())
    }

    fn commit_subagent(
        service: &HostRpcService,
        session_id: &str,
        attempt: Option<&str>,
        context: &Context,
        Spawn {
            item_id: native_item_id,
            prompt,
            reported_model,
            native_task,
            result,
        }: Spawn<'_>,
        child: Option<(&SessionRef, &model::ProviderThread, &str)>,
        status: model::TurnItemStatus,
    ) -> Result<(), String> {
        let key = child.map_or_else(
            || native_item_id.to_owned(),
            |(_, _, native)| format!("{native_item_id}:{native}"),
        );
        let id = child.map_or_else(
            || super::item_identity("node", context.provider, &context.native_turn_id, &key),
            |(child, _, _)| {
                model::derived_id(
                    "node",
                    &[
                        "provider",
                        context.provider.driver(),
                        "native-child",
                        &child.application_id(),
                    ],
                )
            },
        );
        let item_id = model::derived_id("turn-item", &["subagent", &id]);
        let storage = &service.inner.conversation.storage;
        storage
            .observe_thread(
                &context.thread_id,
                std::slice::from_ref(&item_id),
                &context.now,
                |state| {
                    if attempt.is_some_and(|attempt| {
                        !state.native_thread_matches(
                            attempt,
                            context.provider.driver(),
                            &context.native_thread_id,
                        )
                    }) {
                        return Ok(model::Decision::default());
                    }
                    let previous = state.subagents.get(&id);
                    let mut status = status;
                    if let Some(previous) = previous
                        && !matches!(
                            previous.status,
                            model::SubagentStatus::Idle
                                | model::SubagentStatus::Pending
                                | model::SubagentStatus::Running
                                | model::SubagentStatus::Waiting
                        )
                        && matches!(
                            status,
                            model::TurnItemStatus::Pending
                                | model::TurnItemStatus::Running
                                | model::TurnItemStatus::Waiting
                        )
                    {
                        return Ok(model::Decision::default());
                    }
                    if let Some(item) = state.turn_items.get(&item_id) {
                        status = if item.status == model::TurnItemStatus::Interrupted {
                            item.status
                        } else {
                            status
                        };
                    }
                    let mut artifact = context.artifact(
                        &key,
                        Some(&key),
                        status,
                        model::NodeKind::Subagent,
                        model::TurnItemBody::Subagent {
                            subagent_id: id.clone(),
                            origin: "provider_native".into(),
                            driver: context.provider.driver().into(),
                            provider_instance_id: context.provider.key().into(),
                            child_thread_id: child.map(|(child, _, _)| child.application_id()),
                            prompt: prompt.unwrap_or_default().into(),
                            result: result.map(result_text),
                            progress: None,
                        },
                    );
                    artifact.node.id = id.clone();
                    artifact.node.counts_for_run = false;
                    artifact.item.id = item_id.clone();
                    artifact.item.node_id = Some(id.clone());
                    if let Some((_, provider, _)) = child {
                        artifact.node.provider_thread_id = Some(provider.id.clone());
                        artifact.item.provider_thread_id = Some(provider.id.clone());
                    }
                    artifact.node.started_at = previous
                        .and_then(|task| task.started_at.clone())
                        .or(artifact.node.started_at);
                    artifact.item.started_at = artifact.node.started_at.clone();
                    let task = model::Subagent {
                        id,
                        thread_id: context.thread_id.clone(),
                        run_id: context.run_id.clone(),
                        parent_node_id: context.root_node_id.clone(),
                        origin: "provider_native".into(),
                        created_by: "agent".into(),
                        driver: context.provider.driver().into(),
                        provider_instance_id: context.provider.key().into(),
                        provider_thread_id: child.map(|(_, provider, _)| provider.id.clone()),
                        child_thread_id: child.map(|(child, _, _)| child.application_id()),
                        native_task_ref: Some(model::ProviderRef {
                            driver: context.provider.driver().into(),
                            native_id: Some(native_task.unwrap_or(&key).into()),
                            strength: if context.provider == ProviderKind::DeepSeek {
                                model::NativeRefStrength::Weak
                            } else {
                                model::NativeRefStrength::Strong
                            },
                            fingerprint: None,
                            ordinal: None,
                        }),
                        title: previous.and_then(|task| task.title.clone()),
                        model: reported_model
                            .filter(|model| !model.is_empty())
                            .map(str::to_owned)
                            .or_else(|| previous.and_then(|task| task.model.clone())),
                        status: task_status(status),
                        started_at: artifact.node.started_at.clone(),
                        completed_at: artifact.node.completed_at.clone(),
                        updated_at: context.now.clone(),
                    };
                    let mut resources = state.clone();
                    if let Some((_, provider, _)) = child {
                        resources
                            .provider_threads
                            .insert(provider.id.clone(), provider.clone());
                    }
                    let mut observed = if let Some(attempt) = attempt {
                        model::observe_subagent(
                            &resources,
                            attempt,
                            session_id,
                            task,
                            artifact.item,
                        )?
                    } else {
                        model::observe_external_subagent(
                            &resources,
                            &context.provider_turn_id,
                            &context.native_thread_id,
                            task,
                            artifact.item,
                        )?
                    };
                    if !observed.changes.is_empty() {
                        observed
                            .changes
                            .insert(0, model::Change::Node(artifact.node));
                    }
                    Ok(observed)
                },
            )
            .map(|_| ())
    }

    fn collaboration_item(
        service: &HostRpcService,
        source: &resources::Source,
        attempt: Option<&str>,
        context: &Context,
        item: &Item,
    ) -> Result<(), String> {
        let ItemBody::Subagent {
            tool,
            prompt,
            model,
            effort,
            sender,
            receivers,
            states,
            agent_id,
            result,
        } = item.body()
        else {
            unreachable!()
        };
        let aliases = service.inner.conversation.storage.native_owners(
            sender
                .iter()
                .chain(receivers)
                .chain(states.iter().map(|state| &state.session)),
        )?;
        let public = |native: &SessionRef| aliases.get(native).unwrap_or(native).application_id();
        let input = json!({"tool":tool,"prompt":prompt,"model":model,"effort":effort,"senderThreadId":sender.as_ref().map(public),"receiverThreadIds":receivers.iter().map(public).collect::<Vec<_>>(),"agentsStates":states.iter().map(|state|(public(&state.session),json!({"status":state.status,"message":state.message}))).collect::<serde_json::Map<_,_>>(),"agentId":agent_id});
        let artifact = context.artifact(
            item.id.as_str(),
            Some(item.id.as_str()),
            super::item_status(item.status, context.completed),
            model::NodeKind::ToolCall,
            model::TurnItemBody::DynamicTool {
                tool_name: Some(format!("{}.collaboration", context.provider.driver())),
                input,
                output: result.clone(),
                output_omitted: None,
                viewed_image_path: None,
            },
        );
        Self::commit_artifacts(
            service,
            source,
            attempt,
            &context.native_thread_id,
            vec![artifact],
            &context.now,
        )
    }
}
