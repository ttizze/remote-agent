use super::*;
use commands::effect;

/// The request stores its response capability and disposition. Its questions
/// or approval prompt are owned by the corresponding turn item.
pub fn observe_request(
    state: &State,
    attempt_id: &str,
    session_id: &str,
    request: RuntimeRequest,
    item: TurnItem,
) -> Result<Decision, String> {
    if !state.accepts_observation(attempt_id, session_id) {
        return Ok(Decision::default());
    }
    if state
        .runtime_requests
        .get(&request.id)
        .is_some_and(|saved| saved.status != RequestStatus::Pending)
    {
        return Ok(Decision::default());
    }
    validate_prompt(&request, &item, session_id)?;
    let mut decision = observe_item(state, attempt_id, session_id, item)?;
    decision.changes.push(Change::RuntimeRequest(request));
    Ok(decision)
}

pub fn observe_external_request(
    state: &State,
    provider_thread_id: &str,
    native_thread_id: &str,
    session_id: &str,
    request: RuntimeRequest,
    item: TurnItem,
) -> Result<Decision, String> {
    let Some(provider) = native_observation::selected(state, provider_thread_id, native_thread_id)
    else {
        return Ok(Decision::default());
    };
    if state.active_run().is_some()
        || state.has_uncertain_input()
        || state
            .runtime_requests
            .get(&request.id)
            .is_some_and(|saved| saved.status != RequestStatus::Pending)
    {
        return Ok(Decision::default());
    }
    validate_prompt(&request, &item, session_id)?;
    if item.thread_id != state.thread.id
        || item.run_id.is_some()
        || item.provider_thread_id.as_deref() != Some(provider_thread_id)
        || state.turn_items.get(&item.id).is_some_and(|saved| {
            saved.run_id != item.run_id
                || saved.provider_turn_id != item.provider_turn_id
                || saved.provider_thread_id != item.provider_thread_id
        })
    {
        return Err("request does not belong to this native thread".into());
    }
    let mut decision = if let Some(turn_id) = item.provider_turn_id.clone() {
        if !state
            .provider_turns
            .get(&turn_id)
            .is_some_and(|turn| turn.status == ProviderTurnStatus::Running)
        {
            return Ok(Decision::default());
        }
        observe_external_item(state, &turn_id, native_thread_id, item)?
    } else {
        execution::commit_item(state, provider_thread_id, item)
    };
    if !decision.changes.is_empty() {
        if provider.provider_session_id.as_deref() != Some(session_id) {
            decision
                .changes
                .push(Change::ProviderThread(ProviderThread {
                    provider_session_id: Some(session_id.into()),
                    updated_at: request.created_at.clone(),
                    ..provider.clone()
                }));
        }
        decision.changes.push(Change::RuntimeRequest(request));
    }
    Ok(decision)
}

fn validate_prompt(
    request: &RuntimeRequest,
    item: &TurnItem,
    session_id: &str,
) -> Result<(), String> {
    let owns_prompt = match &item.body {
        TurnItemBody::UserInputRequest { request_id, .. } => {
            request.kind == "user_input" && request_id == &request.id
        }
        TurnItemBody::ApprovalRequest {
            request_id,
            request_kind,
            ..
        } => request_id == &request.id && request_kind == &request.kind,
        TurnItemBody::DynamicTool { .. } => {
            request.kind == "dynamic_tool_call" || request.kind == "mcp-elicitation"
        }
        _ => false,
    };
    if request.status != RequestStatus::Pending
        || !owns_prompt
        || item.node_id.as_ref() != Some(&request.node_id)
        || request.provider_turn_id != item.provider_turn_id
        || !matches!(&request.response_capability,ResponseCapability::Live {provider_session_id,..} if provider_session_id==session_id)
    {
        return Err("request and prompt do not belong to this provider session".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RequestResponse {
    Approval {
        decision: ApprovalDecision,
        feedback: Option<String>,
    },
    UserInput {
        answers: BTreeMap<String, Value>,
    },
    Tool {
        result: Value,
    },
}

fn validate_response(item: &TurnItem, response: &RequestResponse) -> Result<(), String> {
    match (&item.body, response) {
        (
            TurnItemBody::ApprovalRequest { options, .. },
            RequestResponse::Approval { decision, .. },
        ) => {
            if !options.iter().any(|option| option.decision == *decision) {
                return Err("approval choice is unavailable".into());
            }
        }
        (
            TurnItemBody::UserInputRequest { questions, .. },
            RequestResponse::UserInput { answers },
        ) => {
            if answers.is_empty() {
                return Ok(());
            }
            if answers
                .keys()
                .any(|id| !questions.iter().any(|question| &question.id == id))
            {
                return Err("answer contains an unknown question".into());
            }
            for question in questions {
                let Some(answer) = answers.get(&question.id) else {
                    if question.required.unwrap_or(true) {
                        return Err("every required question needs an answer".into());
                    }
                    continue;
                };
                if question.required == Some(false)
                    && answer.as_str().is_some_and(|value| value.trim().is_empty())
                {
                    continue;
                }
                let values = match answer {
                    Value::String(value) => vec![value.as_str()],
                    Value::Array(values) if question.multi_select == Some(true) => values
                        .iter()
                        .map(|value| value.as_str().ok_or("question choices must be strings"))
                        .collect::<Result<Vec<_>, _>>()?,
                    _ => return Err("question answer has an invalid type".into()),
                };
                let mut unique = std::collections::BTreeSet::new();
                if values.is_empty()
                    || values.iter().any(|value| {
                        value.trim().is_empty()
                            || !unique.insert(*value)
                            || question.allow_custom_answer == Some(false)
                                && !question.options.iter().any(|option| {
                                    option.value.as_deref().unwrap_or(&option.label) == *value
                                })
                    })
                {
                    return Err("question answer is not an offered choice".into());
                }
            }
        }
        (TurnItemBody::DynamicTool { .. }, RequestResponse::Tool { .. }) => {}
        _ => return Err("response does not match the request".into()),
    }
    Ok(())
}

pub fn respond(
    state: &State,
    request_id: &str,
    response: RequestResponse,
    driver: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let request = state
        .runtime_requests
        .get(request_id)
        .filter(|request| request.status == RequestStatus::Pending)
        .ok_or("request is no longer pending")?;
    let item = state
        .request_item(&request.id)
        .ok_or("request prompt is missing")?;
    validate_response(item, &response)?;
    if let RequestResponse::Approval {
        decision,
        feedback: Some(feedback),
    } = &response
    {
        if *decision != ApprovalDecision::Decline
            || feedback.trim().is_empty()
            || !matches!(
                request.response_capability,
                ResponseCapability::Live {
                    supports_feedback: Some(true),
                    ..
                }
            )
        {
            return Err(
                "feedback requires a supported live approval and a decline decision".into(),
            );
        }
    }
    if request.response_capability == ResponseCapability::Message {
        return message_response(state, request, item, response, driver, context);
    }
    let ResponseCapability::Live {
        provider_session_id,
        ..
    } = &request.response_capability
    else {
        return Err("request cannot be answered through a live callback".into());
    };
    let session = state
        .provider_sessions
        .get(provider_session_id)
        .filter(|session| {
            state.attached_provider_sessions.contains(&session.id)
                && matches!(
                    session.status,
                    ProviderSessionStatus::Ready
                        | ProviderSessionStatus::Running
                        | ProviderSessionStatus::Waiting
                )
        })
        .ok_or("request's provider session is unavailable")?;
    if state.effects.values().any(|effect| {
        matches!(&effect.request,EffectRequest::RuntimeRequestRespond {request_id:pending,..} if pending==request_id)
            && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
    }) {
        return Err("request response is already in flight".into());
    }
    let resolution = resolve_request(state, request_id, Some(&response), context.now)?;
    let (decision, feedback, answers) = match response {
        RequestResponse::Approval { decision, feedback } => (Some(decision), feedback, None),
        RequestResponse::UserInput { answers } => (None, None, Some(answers)),
        RequestResponse::Tool { result } => (
            None,
            None,
            Some(BTreeMap::from([("result".into(), result)])),
        ),
    };
    Ok(Decision {
        changes: resolution.changes,
        effects: vec![effect(
            context,
            &state.thread.id,
            EffectRequest::RuntimeRequestRespond {
                provider_session_id: session.id.clone(),
                request_id: request.id.clone(),
                decision,
                feedback,
                answers,
            },
        )],
    })
}

fn message_response(
    state: &State,
    request: &RuntimeRequest,
    item: &TurnItem,
    response: RequestResponse,
    driver: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let (TurnItemBody::UserInputRequest { questions, .. }, RequestResponse::UserInput { answers }) =
        (&item.body, &response)
    else {
        return Err("only a question can be answered by a message".into());
    };
    let replies: Vec<_> = questions
        .iter()
        .filter_map(|question| {
            let answer = answers.get(&question.id)?;
            let text = match answer {
                Value::String(text) => text.trim().to_owned(),
                Value::Array(values) => values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => return None,
            };
            (!text.is_empty()).then(|| format!("{}\n{}", question.question, text))
        })
        .collect();
    if replies.is_empty() {
        return Err("enter an answer before sending".into());
    }
    let mut decision = admit(
        state,
        Input {
            message_id: format!("async-answer:{}", request.id),
            created_by: Actor::User,
            creation_source: CreationSource::Server,
            text: replies.join("\n\n"),
            attachments: Vec::new(),
            context: None,
            model_selection: state.thread.model_selection.clone(),
            driver: driver.into(),
            workspace_preparation: None,
            fork_origin: None,
        },
        Admission::Steer,
        context,
    )?;
    decision
        .changes
        .extend(resolve_request(state, &request.id, Some(&response), context.now)?.changes);
    Ok(decision)
}

/// Native resolution and the result of our callback converge on the same
/// terminal record. A late callback result cannot make a resolved request live.
pub fn resolve_request(
    state: &State,
    request_id: &str,
    response: Option<&RequestResponse>,
    now: &str,
) -> Result<Decision, String> {
    let Some(request) = state
        .runtime_requests
        .get(request_id)
        .filter(|request| request.status == RequestStatus::Pending)
    else {
        return Ok(Decision::default());
    };
    let mut request = request.clone();
    request.status = RequestStatus::Resolved;
    request.resolved_at = Some(now.into());
    match response {
        Some(RequestResponse::Approval { decision, feedback }) => {
            request.decision = Some(*decision);
            request.feedback = feedback.clone();
        }
        Some(RequestResponse::UserInput { answers }) => {
            request.answers =
                Some(serde_json::to_value(answers).map_err(|error| error.to_string())?)
        }
        Some(RequestResponse::Tool { .. }) | None => {}
    }
    let cancelled = matches!(
        response,
        Some(RequestResponse::Approval {
            decision: ApprovalDecision::Decline | ApprovalDecision::Cancel,
            ..
        })
    );
    let mut changes = Vec::new();
    if let Some(node) = state.nodes.get(&request.node_id) {
        changes.push(Change::Node(ExecutionNode {
            status: if cancelled {
                NodeStatus::Cancelled
            } else {
                NodeStatus::Completed
            },
            completed_at: Some(now.into()),
            ..node.clone()
        }));
    }
    if let Some(item) = state.request_item(&request.id) {
        changes.push(Change::TurnItem(TurnItem {
            status: if cancelled {
                TurnItemStatus::Cancelled
            } else {
                TurnItemStatus::Completed
            },
            completed_at: Some(now.into()),
            updated_at: now.into(),
            ..item.clone()
        }));
    }
    changes.push(Change::RuntimeRequest(request));
    Ok(Decision {
        changes,
        ..Default::default()
    })
}

/// T3 records the answer before attempting its callback. A transport failure
/// adds a timeline error, preserving that answer and its resolved disposition.
pub fn response_failed(state: &State, request_id: &str, message: &str, now: &str) -> Decision {
    let Some(node) = state
        .runtime_requests
        .get(request_id)
        .and_then(|request| state.nodes.get(&request.node_id))
    else {
        return Decision::default();
    };
    let failure = TurnItem {
        detail_omitted: false,
        id: format!("turn-item:runtime-request-delivery-failure:{request_id}"),
        thread_id: node.thread_id.clone(),
        run_id: node.run_id.clone(),
        node_id: Some(node.id.clone()),
        provider_thread_id: node.provider_thread_id.clone(),
        provider_turn_id: node.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: TurnItemStatus::Failed,
        title: Some("Answer delivery not confirmed".into()),
        started_at: Some(now.into()),
        completed_at: Some(now.into()),
        updated_at: now.into(),
        body: TurnItemBody::Error {
            failure: ProviderFailure {
                class: "transport_error".into(),
                message: message.into(),
                code: None,
                retryable: None,
                reset_at: None,
            },
            retry: None,
        },
    };
    Decision {
        changes: vec![Change::TurnItem(failure)],
        ..Default::default()
    }
}

pub fn request_source_closed(
    state: &State,
    request_id: &str,
    now: &str,
) -> Result<Decision, String> {
    let Some(request) = state.runtime_requests.get(request_id).filter(|request| {
        request.status == RequestStatus::Pending
            && matches!(request.response_capability, ResponseCapability::Live { .. })
    }) else {
        return Ok(Decision::default());
    };
    if request.kind == "user_input" {
        return Ok(Decision {
            changes: vec![Change::RuntimeRequest(RuntimeRequest {
                response_capability: ResponseCapability::Message,
                ..request.clone()
            })],
            ..Default::default()
        });
    }
    let mut decision = resolve_request(state, request_id, None, now)?;
    for change in &mut decision.changes {
        match change {
            Change::RuntimeRequest(request) => request.status = RequestStatus::Expired,
            Change::Node(node) => node.status = NodeStatus::Cancelled,
            Change::TurnItem(item) => item.status = TurnItemStatus::Cancelled,
            _ => {}
        }
    }
    Ok(decision)
}
