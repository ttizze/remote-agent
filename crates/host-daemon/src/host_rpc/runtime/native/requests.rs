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
        RequestBody::Permission {
            description,
            details,
            choices,
        } => (
            origin.source.request_kind(body).into(),
            Kind::ApprovalRequest,
            Body::ApprovalRequest {
                request_id: request_id.into(),
                request_kind: origin.source.request_kind(body).into(),
                prompt: Some(description.clone()),
                reason: Some(details.clone()),
                options: approval_options(choices, origin),
            },
        ),
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
        RequestBody::Elicitation { .. } => {
            return Err("elicitation conversion has not been connected".into());
        }
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

impl Runtime {
    pub(in crate::host_rpc::runtime) async fn execution_request(
        service: &HostRpcService,
        target: &SessionRef,
        native: &SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
        execution: Uuid,
    ) -> Result<(), String> {
        if Some(origin.provider) != native.provider || !origin.source.is_alive() {
            return Err("request source is no longer live".into());
        }
        let runtime = &service.inner.conversation;
        let source = runtime.register_native(target, native);
        let _updates = source.updates.lock().await;
        let now = commands::at();
        let attempt_id = execution.to_string();
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
                    if !state.native_thread_matches(
                        &attempt_id,
                        origin.provider.driver(),
                        &native.id,
                    ) {
                        return Ok(model::Decision::default());
                    }
                    model::started(
                        state,
                        &attempt_id,
                        &source.session_id,
                        model::ProviderRef {
                            driver: origin.provider.driver().into(),
                            native_id: Some(turn_id.to_string()),
                            strength: model::NativeRefStrength::Strong,
                            fingerprint: None,
                            ordinal: None,
                        },
                        executor::context(state, &allocation, &now),
                    )
                })?;
        }
        let state = runtime.storage.thread_state(&target.id, &[])?;
        if !state.accepts_observation(&attempt_id, &source.session_id)
            || !state.native_thread_matches(&attempt_id, origin.provider.driver(), &native.id)
        {
            return Err("request has no accepting execution".into());
        }
        if runtime
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(id, pending)| {
                pending.attempt.id == execution
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
        let context = super::observations::native_context(&state, &attempt_id, &now, false)?;
        let request_id = model::derived_id(
            "runtime-request",
            &[
                "provider",
                origin.provider.driver(),
                "provider-turn",
                &context.provider_turn_id,
                "native-request",
                &native_request_id,
                &allocation,
            ],
        );
        let (kind, node_kind, body) = prompt(&request_id, &request.body, &origin)?;
        let native_item = match &request.target {
            agent_protocol::requests::RequestTarget::Turn { item_id, .. } => {
                item_id.as_ref().map(|id| id.as_str())
            }
            _ => None,
        };
        let mut artifact = context.artifact(
            &request_id,
            native_item,
            model::TurnItemStatus::Waiting,
            node_kind,
            body,
        );
        artifact.node.runtime_request_id = Some(request_id.clone());
        let record = model::RuntimeRequest {
            id: request_id.clone(),
            node_id: artifact.node.id.clone(),
            provider_turn_id: Some(context.provider_turn_id),
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
        let bytes = serde_json::to_vec(&request)
            .map_err(|error| error.to_string())?
            .len();
        if !runtime.retain_request(
            request_id.clone().into(),
            bytes,
            resources::PendingRequest {
                target: target.clone(),
                attempt: Attempt {
                    id: execution,
                    turn: context.native_turn_id.into(),
                    provider: origin.provider,
                    source: native.id.clone(),
                },
                origin,
                body: request.body,
            },
        )? {
            return Ok(());
        }
        let result = runtime
            .storage
            .observe_thread(&target.id, &[], &now, |state| {
                let mut decision = model::Decision {
                    changes: vec![model::Change::Node(artifact.node)],
                    ..Default::default()
                };
                let request = model::observe_request(
                    &state.apply(&decision),
                    &attempt_id,
                    &source.session_id,
                    record,
                    artifact.item,
                )?;
                decision.changes.extend(request.changes);
                Ok(decision)
            });
        if result.is_err() {
            runtime.requests.lock().unwrap().remove(request_id.as_str());
        }
        result.map(|_| ())
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
            let native = SessionRef::new(pending.attempt.provider, pending.attempt.source.clone())
                .map_err(|error| Failure::new("invalid_session", error))?;
            let _retire = scopeguard::guard(native, |native| runtime.retire_source(&native));
            let source = runtime.source(&pending.target, &pending.attempt);
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
mod tests {
    use super::*;
    use agent_protocol::requests::*;

    fn pending(body: RequestBody) -> resources::PendingRequest {
        resources::PendingRequest {
            target: SessionRef::new(None, "thread".into()).unwrap(),
            attempt: Attempt {
                id: Uuid::new_v4(),
                turn: "turn".into(),
                provider: ProviderKind::Codex,
                source: "native".into(),
            },
            origin: RequestOrigin {
                instance: Uuid::new_v4(),
                native_id: json!(1),
                provider: ProviderKind::Codex,
                source: Arc::new(super::super::super::native_fixture::AnswerResource),
            },
            body,
        }
    }
    #[test]
    fn tool_result_is_reconstructed_from_the_canonical_response_payload() {
        let pending = pending(RequestBody::ToolExecution {
            tool: "test".into(),
            namespace: None,
            arguments: json!({}),
        });
        // The native callback accepts the typed ToolContent representation.
        let content = vec![ToolContent::Text {
            text: "done".into(),
        }];
        let answers = std::collections::BTreeMap::from([(
            "result".into(),
            json!({"success":true,"content":content}),
        )]);
        assert_eq!(
            native_answer(&pending, None, Some(&answers)).unwrap(),
            Answer::ToolExecution {
                success: true,
                content
            }
        );
    }
    #[test]
    fn skip_is_delivered_and_choices_use_native_ids_only_after_label_validation() {
        let pending = pending(RequestBody::Question {
            questions: vec![Question {
                id: "question".into(),
                header: "Header".into(),
                prompt: "Choose".into(),
                secret: false,
                allow_free_text: true,
                multiple: true,
                choices: vec![QuestionChoice {
                    id: "opaque-native-choice".into(),
                    label: "First".into(),
                    description: String::new(),
                }],
            }],
        });
        assert_eq!(
            native_answer(&pending, None, Some(&Default::default())).unwrap(),
            Answer::SkipQuestions
        );
        let answers = std::collections::BTreeMap::from([("question".into(), json!(["First"]))]);
        let answer = native_answer(&pending, None, Some(&answers)).unwrap();
        validate_answer(&pending.body, &answer).unwrap();
        assert!(
            matches!(answer,Answer::Questions {answers} if answers["question"]==QuestionAnswer::MultipleChoices {choice_ids:vec!["opaque-native-choice".into()]})
        );
        let invalid = std::collections::BTreeMap::from([("question".into(), json!(["missing"]))]);
        assert!(native_answer(&pending, None, Some(&invalid)).is_err());
        let text =
            std::collections::BTreeMap::from([("question".into(), json!("opaque-native-choice"))]);
        assert!(
            matches!(native_answer(&pending,None,Some(&text)).unwrap(),Answer::Questions {answers} if answers["question"]==QuestionAnswer::FreeText {text:"opaque-native-choice".into()})
        );
    }
}
