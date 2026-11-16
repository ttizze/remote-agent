use super::*;
use agent_protocol::requests::{Answer, QuestionAnswer, RequestBody};

fn prompt(
    request_id: &str,
    body: &RequestBody,
    origin: &RequestOrigin,
) -> Result<(String, model::NodeKind, model::TurnItemBody), String> {
    use model::{NodeKind as Kind, TurnItemBody as Body};
    if let RequestBody::Question { questions } = body {
        for question in questions {
            let mut labels = std::collections::HashSet::new();
            if question
                .choices
                .iter()
                .any(|choice| !labels.insert(&choice.label))
            {
                return Err("native question has duplicate choice labels".into());
            }
        }
    }
    Ok(match body {
        RequestBody::Question { questions } => (
            "user_input".into(),
            Kind::UserInputRequest,
            Body::UserInputRequest {
                request_id: request_id.into(),
                questions: questions
                    .iter()
                    .map(|question| model::UserInputQuestion {
                        id: question.id.clone(),
                        header: question.header.clone(),
                        question: question.prompt.clone(),
                        is_secret: question.secret.then_some(true),
                        options: question
                            .choices
                            .iter()
                            .map(|choice| model::UserInputOption {
                                label: choice.label.clone(),
                                description: choice.description.clone(),
                                value: Some(choice.label.clone()),
                                preview: None,
                            })
                            .collect(),
                        multi_select: Some(question.multiple),
                        allow_custom_answer: Some(question.allow_free_text),
                        initial_answer: None,
                        required: None,
                    })
                    .collect(),
            },
        ),
        RequestBody::Approval {
            description,
            details,
            choices,
            ..
        }
        | RequestBody::Permission {
            description,
            details,
            choices,
        } => {
            let kind = origin.source.request_kind(body);
            (
                kind.into(),
                Kind::ApprovalRequest,
                Body::ApprovalRequest {
                    request_id: request_id.into(),
                    request_kind: kind.into(),
                    prompt: Some(description.clone()),
                    reason: Some(details.clone()),
                    options: approval_options(choices, origin),
                },
            )
        }
        RequestBody::ToolExecution {
            tool,
            namespace,
            arguments,
        } => (
            "dynamic_tool_call".into(),
            Kind::ToolCall,
            Body::DynamicTool {
                tool_name: Some(
                    namespace
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(tool.as_str()))
                        .collect::<Vec<_>>()
                        .join("."),
                ),
                input: arguments.clone(),
                output: None,
                output_omitted: None,
                viewed_image_path: None,
            },
        ),
        // The native form requires typed values, beyond an approval decision.
        // Keep that tool input on the canonical item, not the request metadata.
        RequestBody::Elicitation {
            server,
            message,
            input,
        } => (
            "mcp-elicitation".into(),
            Kind::ToolCall,
            Body::DynamicTool {
                tool_name: Some("mcp.elicitation".into()),
                input: json!({"server":server,"message":message,"input":input}),
                output: None,
                output_omitted: None,
                viewed_image_path: None,
            },
        ),
    })
}

fn approval_options(
    choices: &[agent_protocol::requests::Choice],
    origin: &RequestOrigin,
) -> Vec<model::ApprovalOption> {
    let mut options = Vec::<model::ApprovalOption>::new();
    for choice in choices {
        let decision = origin.source.approval_decision(choice);
        if !options.iter().any(|option| option.decision == decision) {
            options.push(model::ApprovalOption {
                decision,
                label: choice.label.clone(),
                warning: (!choice.description.is_empty()).then(|| choice.description.clone()),
            });
        }
    }
    options
}

fn request_artifact(
    state: &model::State,
    native: &SessionRef,
    attempt_id: Option<&str>,
    target: &agent_protocol::requests::RequestTarget,
    request_id: &str,
    kind: model::NodeKind,
    body: model::TurnItemBody,
    now: &str,
) -> Result<Artifact, String> {
    let provider =
        super::external::provider(state, native).ok_or("request source is no longer selected")?;
    let attempt = attempt_id
        .map(|id| {
            state
                .run_attempts
                .get(id)
                .ok_or("request attempt is missing")
        })
        .transpose()?;
    let (turn, native_item) = match target {
        agent_protocol::requests::RequestTarget::Turn { turn_id, item_id } => {
            let turn = state
                .provider_turns
                .values()
                .find(|turn| {
                    turn.provider_thread_id == provider.id
                        && turn.run_attempt_id.as_deref() == attempt_id
                        && turn
                            .control_ref()
                            .and_then(|reference| reference.native_id.as_deref())
                            == Some(turn_id.as_str())
                })
                .ok_or("request has no accepting native turn")?;
            (Some(turn), item_id.as_ref())
        }
        agent_protocol::requests::RequestTarget::Session => (None, None),
    };
    let node_id = model::derived_id("node", &[request_id]);
    let root = attempt
        .map(|attempt| attempt.root_node_id.clone())
        .or_else(|| turn.map(|turn| turn.node_id.clone()));
    let native_item = native_item.map(|id| model::ProviderRef {
        driver: provider.driver.clone(),
        native_id: Some(id.to_string()),
        strength: if native.provider == Some(ProviderKind::DeepSeek) {
            model::NativeRefStrength::Weak
        } else {
            model::NativeRefStrength::Strong
        },
        fingerprint: None,
        ordinal: None,
    });
    let node = model::ExecutionNode {
        id: node_id.clone(),
        thread_id: state.thread.id.clone(),
        run_id: attempt.map(|attempt| attempt.run_id.clone()),
        parent_node_id: root.clone(),
        root_node_id: root.unwrap_or_else(|| node_id.clone()),
        kind,
        status: model::NodeStatus::Waiting,
        counts_for_run: false,
        provider_thread_id: Some(provider.id.clone()),
        provider_turn_id: turn.map(|turn| turn.id.clone()),
        native_item_ref: native_item.clone(),
        runtime_request_id: Some(request_id.into()),
        checkpoint_scope_id: None,
        started_at: Some(now.into()),
        completed_at: None,
    };
    let item = model::TurnItem {
        detail_omitted: false,
        id: model::derived_id("turn-item", &[request_id]),
        thread_id: node.thread_id.clone(),
        run_id: node.run_id.clone(),
        node_id: Some(node_id),
        provider_thread_id: node.provider_thread_id.clone(),
        provider_turn_id: node.provider_turn_id.clone(),
        native_item_ref: native_item,
        parent_item_id: None,
        ordinal: 0,
        status: model::TurnItemStatus::Waiting,
        title: None,
        started_at: Some(now.into()),
        completed_at: None,
        updated_at: now.into(),
        body,
    };
    Ok(Artifact {
        node,
        item,
        plan: None,
    })
}

impl Runtime {
    pub(in crate::host_rpc::runtime) async fn execution_request(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
        execution: Option<Uuid>,
    ) -> Result<(), String> {
        if Some(origin.provider) != native.provider || !origin.source.is_alive() {
            return Err("request source is no longer live".into());
        }
        let runtime = &service.inner.conversation;
        let source = runtime.register_native(target, native);
        let _updates = source.updates.lock().await;
        let now = commands::at();
        let attempt_id = execution.map(|id| id.to_string());
        let allocation = Uuid::new_v4().to_string();
        let native_request_id = origin
            .native_id
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| origin.native_id.to_string());
        if let agent_protocol::requests::RequestTarget::Turn { turn_id, .. } = &request.target {
            runtime
                .storage
                .observe_thread(&target.id, &[], &now, |state| {
                    let reference = model::ProviderRef {
                        driver: origin.provider.driver().into(),
                        native_id: Some(turn_id.to_string()),
                        strength: if origin.provider == ProviderKind::DeepSeek {
                            model::NativeRefStrength::Weak
                        } else {
                            model::NativeRefStrength::Strong
                        },
                        fingerprint: None,
                        ordinal: None,
                    };
                    if let Some(attempt) = &attempt_id {
                        if !state.native_thread_matches(
                            attempt,
                            origin.provider.driver(),
                            &native.id,
                        ) {
                            return Ok(model::Decision::default());
                        }
                        model::started(
                            state,
                            attempt,
                            &source.session_id,
                            reference,
                            executor::context(state, &allocation, &now),
                        )
                    } else if let Some(provider) = super::external::provider(state, native) {
                        model::observe_external_turn(
                            state,
                            &provider.id,
                            &native.id,
                            reference,
                            model::ProviderTurnStatus::Running,
                            &now,
                        )
                    } else {
                        Ok(model::Decision::default())
                    }
                })?;
        }
        let state = runtime.storage.thread_state(&target.id, &[])?;
        if runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(id, pending)| {
                pending.native == *native
                    && pending.attempt_id == execution
                    && pending.origin.instance == origin.instance
                    && pending.origin.native_id == origin.native_id
                    && state
                        .runtime_requests
                        .get(id.as_str())
                        .is_some_and(|request| request.status == model::RequestStatus::Pending)
            })
        {
            return Ok(());
        }
        let request_id = model::derived_id(
            "runtime-request",
            &[
                "provider",
                origin.provider.driver(),
                "native-request",
                &native_request_id,
                &allocation,
            ],
        );
        let (kind, node_kind, body) = prompt(&request_id, &request.body, &origin)?;
        let artifact = request_artifact(
            &state,
            native,
            attempt_id.as_deref(),
            &request.target,
            &request_id,
            node_kind,
            body,
            &now,
        )?;
        let record = model::RuntimeRequest {
            id: request_id.clone(),
            node_id: artifact.node.id.clone(),
            provider_turn_id: artifact.item.provider_turn_id.clone(),
            kind,
            native_request_ref: Some(model::ProviderRef {
                driver: origin.provider.driver().into(),
                native_id: Some(native_request_id),
                strength: model::NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            status: model::RequestStatus::Pending,
            response_capability: model::ResponseCapability::Live {
                provider_session_id: source.session_id.clone(),
                supports_feedback: None,
            },
            created_at: now.clone(),
            resolved_at: None,
            decision: None,
            feedback: None,
            answers: None,
        };
        let capabilities = service
            .inner
            .providers
            .get(native.native_provider()?)
            .map(|agent| executor::provider_capabilities(agent.capabilities()))
            .unwrap_or_default();
        let cwd = state
            .thread
            .worktree_path
            .clone()
            .or(runtime.storage.thread_project_root(target)?)
            .unwrap_or_default();
        let bytes = serde_json::to_vec(&request)
            .map_err(|error| error.to_string())?
            .len();
        if !runtime.retain_request(
            request_id.clone().into(),
            bytes,
            resources::PendingRequest {
                target: target.clone(),
                native: native.clone(),
                attempt_id: execution,
                origin,
                body: request.body,
            },
        )? {
            return Ok(());
        }
        let result = runtime
            .storage
            .observe_thread(&target.id, &[], &now, |state| {
                let mut decision = if let Some(attempt) = &attempt_id {
                    if !state.native_thread_matches(
                        attempt,
                        native.provider.unwrap().driver(),
                        &native.id,
                    ) {
                        return Ok(model::Decision::default());
                    }
                    model::observe_request(
                        state,
                        attempt,
                        &source.session_id,
                        record,
                        artifact.item,
                    )?
                } else {
                    model::observe_external_request(
                        state,
                        artifact
                            .item
                            .provider_thread_id
                            .as_deref()
                            .ok_or("request has no provider thread")?,
                        &native.id,
                        &source.session_id,
                        record,
                        artifact.item.clone(),
                    )?
                };
                if !decision.changes.is_empty() {
                    decision
                        .changes
                        .insert(0, model::Change::Node(artifact.node));
                    if execution.is_none() {
                        let session = model::ProviderSession {
                            id: source.session_id.clone(),
                            driver: native.provider.unwrap().driver().into(),
                            provider_instance_id: native.provider.unwrap().key().into(),
                            status: model::ProviderSessionStatus::Ready,
                            cwd: cwd.clone(),
                            model: Some(state.thread.model_selection.model.clone()),
                            capabilities,
                            created_at: state
                                .provider_sessions
                                .get(&source.session_id)
                                .map_or_else(|| now.clone(), |saved| saved.created_at.clone()),
                            updated_at: now.clone(),
                            last_error: None,
                        };
                        decision
                            .changes
                            .push(model::Change::ProviderSessionAttached(session));
                    }
                }
                Ok(decision)
            });
        if !result.as_ref().is_ok_and(|events| !events.is_empty()) {
            runtime.requests.lock().unwrap().remove(request_id.as_str());
        }
        result.and_then(|events| {
            if events.is_empty() {
                Err("native execution no longer accepts requests".into())
            } else {
                Ok(())
            }
        })
    }

    pub(in crate::host_rpc::runtime) async fn respond_native(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        request_id: &str,
        decision: Option<model::ApprovalDecision>,
        answers: Option<&std::collections::BTreeMap<String, serde_json::Value>>,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        if runtime.storage.effect_state(effect, worker)?.is_none() {
            return Ok(());
        }
        let id: RequestId = request_id.into();
        let pending = runtime.requests.lock().unwrap().get(&id).cloned();
        let result = async {
            let pending = pending.as_ref().ok_or_else(|| {
                Failure::new(
                    "request_expired",
                    "native answer callback is no longer available",
                )
            })?;
            let _retire =
                scopeguard::guard(&pending.native, |native| runtime.retire_source(native));
            let source = runtime.register_native(&pending.target, &pending.native);
            let _io = source.io.lock().await;
            if runtime
                .storage
                .effect_state(effect, worker)
                .map_err(storage_failure)?
                .is_none()
            {
                return Ok(());
            }
            let answer = native_answer(pending, decision, answers)
                .map_err(|error| Failure::new("invalid_answer", error))?;
            service.require_not_updating()?;
            pending
                .origin
                .source
                .prepare(&pending.origin.native_id, &pending.body, &answer)
                .await?
                .await
        }
        .await;
        runtime.requests.lock().unwrap().remove(&id);
        if let Err(error) = result {
            let now = commands::at();
            runtime
                .storage
                .observe_effect(effect, worker, &now, |state| {
                    let mut decision =
                        model::response_failed(state, request_id, &error.message, &now);
                    decision.effects.push(model::Effect {
                        status: model::EffectStatus::Failed,
                        lease_owner: None,
                        lease_expires_at: None,
                        last_error: Some(error.message.clone()),
                        completed_at: Some(now.clone()),
                        updated_at: now.clone(),
                        ..state.effects[&effect.id].clone()
                    });
                    Ok(decision)
                })?;
            return Ok(());
        }
        Ok(())
    }
}

fn native_answer(
    pending: &resources::PendingRequest,
    decision: Option<model::ApprovalDecision>,
    answers: Option<&std::collections::BTreeMap<String, serde_json::Value>>,
) -> Result<Answer, String> {
    Ok(match &pending.body {
        RequestBody::Approval { choices, .. } | RequestBody::Permission { choices, .. } => {
            let choice = choices
                .iter()
                .find(|choice| Some(pending.origin.source.approval_decision(choice)) == decision)
                .ok_or("native approval choice is unavailable")?;
            if matches!(pending.body, RequestBody::Permission { .. }) {
                Answer::Permission {
                    choice_id: choice.id.clone(),
                }
            } else {
                Answer::Approval {
                    choice_id: choice.id.clone(),
                }
            }
        }
        RequestBody::Question { questions } => {
            let answers = answers.ok_or("question answers are missing")?;
            if answers.is_empty() {
                return Ok(Answer::SkipQuestions);
            }
            let mut result = std::collections::BTreeMap::new();
            for question in questions {
                let value = answers
                    .get(&question.id)
                    .ok_or("question answer is missing")?;
                let answer = match value {
                    serde_json::Value::Array(values) => QuestionAnswer::MultipleChoices {
                        choice_ids: values
                            .iter()
                            .map(|value| {
                                value
                                    .as_str()
                                    .and_then(|value| {
                                        question.choices.iter().find(|choice| choice.label == value)
                                    })
                                    .map(|choice| choice.id.clone())
                                    .ok_or("question choice is unavailable")
                            })
                            .collect::<Result<_, _>>()?,
                    },
                    serde_json::Value::String(value)
                        if question.choices.iter().any(|choice| choice.label == *value) =>
                    {
                        QuestionAnswer::SingleChoice {
                            choice_id: question
                                .choices
                                .iter()
                                .find(|choice| choice.label == *value)
                                .unwrap()
                                .id
                                .clone(),
                        }
                    }
                    serde_json::Value::String(value) => QuestionAnswer::FreeText {
                        text: value.clone(),
                    },
                    _ => return Err("question answer has an invalid type".into()),
                };
                result.insert(question.id.clone(), answer);
            }
            Answer::Questions { answers: result }
        }
        RequestBody::ToolExecution { .. } => {
            let result = answers
                .and_then(|answers| answers.get("result"))
                .ok_or("tool response is missing")?;
            Answer::ToolExecution {
                success: result
                    .get("success")
                    .and_then(serde_json::Value::as_bool)
                    .ok_or("tool result success is missing")?,
                content: serde_json::from_value(
                    result
                        .get("content")
                        .cloned()
                        .ok_or("tool result content is missing")?,
                )
                .map_err(|error| error.to_string())?,
            }
        }
        RequestBody::Elicitation { .. } => Answer::Elicitation {
            action: serde_json::from_value(
                answers
                    .and_then(|answers| answers.get("result"))
                    .cloned()
                    .ok_or("elicitation response is missing")?,
            )
            .map_err(|error| error.to_string())?,
        },
    })
}

#[cfg(test)]
mod tests;
