//! Scripted transport fixtures describe canonical records independently of the
//! Host's writer and reducer. Native display fixtures remain convenient inputs.
use agent_protocol::{
    execution::{ItemStatus, TurnStatus},
    items::{ItemBody, MessagePart},
    models::ThreadResponse,
    orchestration::{self as model, projection::*},
    session::ProviderKind,
};
use serde_json::{Value, json};
use std::sync::Arc;

const NOW: &str = "2026-11-14T00:00:00.000Z";

pub(crate) fn snapshot(response: ThreadResponse, sequence: u64) -> ThreadSnapshot {
    let thread = &response.thread;
    let identity = thread.id.as_ref().expect("fixture thread identity");
    let provider = thread
        .active_provider
        .or(identity.provider)
        .unwrap_or(ProviderKind::Codex);
    let id = identity.application_id();
    // Only external native discoveries infer a project from their native root.
    // App-created IDs need the Host's explicit membership; an unregistered
    // folder does not invent a project absent from the canonical catalog.
    let project = match &thread.project_id {
        agent_protocol::models::ProjectMembership::Unknown {} if identity.provider.is_some() => {
            thread
                .cwd
                .as_ref()
                .map(|cwd| model::derived_id("project", &[cwd]))
        }
        _ => thread.project_id.as_deref().map(str::to_owned),
    };
    let selected_model = selection(
        provider,
        response.model.as_ref(),
        response.effort.as_deref(),
        response.service_tier.as_deref(),
    );
    let mut projection:ThreadProjection=serde_json::from_value(json!({
        "thread":{"id":id,"createdBy":"user","creationSource":if thread.can_accept_direct_input==Some(false) {"provider"} else {"mobile"},"projectId":project,"title":thread.name.clone().unwrap_or_default(),"providerInstanceId":provider.key(),"modelSelection":selected_model,"runtimeMode":"full-access","interactionMode":"default","branch":thread.git_branch,"worktreePath":thread.cwd,"activeProviderThreadId":null,"lineage":{"parentThreadId":thread.parent_id.as_ref().map(|parent|parent.application_id()),"relationshipToParent":thread.parent_id.as_ref().map(|_|"subagent"),"rootThreadId":id},"forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,"archivedAt":null,"settledOverride":null,"settledAt":null,"lastVisitedAt":null,"deletedAt":null},
        "runs":[],"attempts":[],"nodes":[],"subagents":[],"providerSessions":[],"providerThreads":[],"providerTurns":[],"runtimeRequests":[],"plans":[],"turnItems":[],"checkpointScopes":[],"checkpoints":[],"contextHandoffs":[],"contextTransfers":[],"visibleTurnItems":[],"updatedAt":NOW
    })).unwrap();
    for (ordinal, turn) in thread.turns.iter().flatten().enumerate() {
        let selected = turn.provider.unwrap_or(provider);
        let message = turn
            .items
            .iter()
            .flatten()
            .find(|item| matches!(item.body(), ItemBody::UserMessage { .. }))
            .map(|item| {
                item.client_input_id
                    .as_ref()
                    .map_or(item.id.as_str(), |id| id.as_str())
            })
            .unwrap_or("fixture-input");
        let status = match turn.status {
            TurnStatus::Running => model::RunStatus::Running,
            TurnStatus::Failed => model::RunStatus::Failed,
            TurnStatus::Interrupted | TurnStatus::Unknown => model::RunStatus::Interrupted,
            TurnStatus::Completed => model::RunStatus::Completed,
        };
        let mut saved = run(
            &id,
            turn.id.as_str(),
            message,
            ordinal as u64 + 1,
            status,
            &selected_model,
        );
        saved.fresh_context = turn.fresh_context;
        projection.runs.push(saved);
        for item in turn.items.iter().flatten() {
            let item = item_record(&id, Some(turn.id.as_str()), item, selected, turn.queued);
            push_item(&mut projection, item, true);
        }
    }
    for (position, input) in thread.queue.inputs.iter().enumerate() {
        let run_id = format!("queued:{}", input.client_user_message_id);
        let mut queued = run(
            &id,
            &run_id,
            input.client_user_message_id.as_str(),
            projection.runs.len() as u64 + 1,
            model::RunStatus::Queued,
            &selection(
                provider,
                input.model.as_ref(),
                input.effort.as_deref(),
                input.service_tier.as_deref(),
            ),
        );
        queued.queue_position = Some(position as u64 + 1);
        queued.queue_held = thread.queue.held;
        projection.runs.push(queued);
        let mut native = agent_protocol::items::Item::new(
            input.client_user_message_id.as_str().into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: agent_protocol::operations::Input::message_parts(&input.input),
            },
        );
        native.client_input_id = Some(input.client_user_message_id.clone());
        let item = item_record(&id, Some(&run_id), &native, provider, true);
        push_item(&mut projection, item, false);
    }
    for preparation in &thread.preparations {
        let input = &preparation.submission;
        let run_id = format!("prepared:{}", input.client_user_message_id);
        let status = match preparation.status {
            agent_protocol::session::PreparationStatus::Preparing => model::RunStatus::Preparing,
            agent_protocol::session::PreparationStatus::Sending
            | agent_protocol::session::PreparationStatus::Unknown => model::RunStatus::Starting,
            agent_protocol::session::PreparationStatus::Interrupted => model::RunStatus::Cancelled,
            agent_protocol::session::PreparationStatus::Failed { .. } => model::RunStatus::Failed,
        };
        projection.runs.push(run(
            &id,
            &run_id,
            input.client_user_message_id.as_str(),
            projection.runs.len() as u64 + 1,
            status,
            &selection(
                provider,
                input.model.as_ref(),
                input.effort.as_deref(),
                input.service_tier.as_deref(),
            ),
        ));
        let mut native = agent_protocol::items::Item::new(
            input.client_user_message_id.as_str().into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: agent_protocol::operations::Input::message_parts(&input.input),
            },
        );
        native.client_input_id = Some(input.client_user_message_id.clone());
        let item = item_record(&id, Some(&run_id), &native, provider, false);
        let parent = item.id.clone();
        push_item(&mut projection, item, true);
        if preparation.status == agent_protocol::session::PreparationStatus::Unknown {
            let error: model::TurnItem=serde_json::from_value(json!({"id":format!("unknown:{parent}"),"threadId":id,"runId":run_id,
                "nodeId":null,"providerThreadId":null,"providerTurnId":null,"nativeItemRef":null,"parentItemId":parent,
                "ordinal":0,"status":"failed","title":null,"startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,
                "type":"error","failure":{"class":"delivery_uncertain","message":"lost native acknowledgement","code":null,"retryable":false,"resetAt":null}})).unwrap();
            push_item(&mut projection, error, true);
        }
    }
    add_requests(&mut projection, &thread.requests);
    if !thread.background.is_empty() {
        projection.provider_threads.push(serde_json::from_value(json!({
            "id":"fixture-provider-thread","driver":provider.driver(),"providerInstanceId":provider.key(),
            "providerSessionId":null,"appThreadId":id,"ownerNodeId":null,"nativeThreadRef":null,"nativeConversationHeadRef":null,
            "status":"idle","firstRunOrdinal":null,"lastRunOrdinal":null,"handoffIds":[],"forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,
            "pendingBackgroundTasks":thread.background.iter().map(|task|json!({"taskId":task.id,"kind":match task.kind {agent_protocol::execution::BackgroundTaskKind::Subagent=>"subagent",agent_protocol::execution::BackgroundTaskKind::Command=>"command",agent_protocol::execution::BackgroundTaskKind::Monitor=>"monitor",agent_protocol::execution::BackgroundTaskKind::BackgroundTask=>"background_task"},"description":task.description})).collect::<Vec<_>>()
        })).unwrap());
    }
    let latest = projection.runs.iter().map(|run| run.ordinal).max();
    ThreadSnapshot {
        snapshot_sequence: sequence,
        projection,
        history_cursor: thread.history_cursor.clone(),
        has_more_history: thread.history_has_more == Some(true) || thread.history_cursor.is_some(),
        latest_local_turn_ordinal: latest,
    }
}

fn selection(
    provider: ProviderKind,
    model: Option<&agent_protocol::models::ModelRef>,
    effort: Option<&str>,
    tier: Option<&str>,
) -> Value {
    let mut options = vec![];
    if let Some(effort) = effort {
        options.push(json!({"id":if provider==ProviderKind::Claude {"effort"} else {"reasoningEffort"},"value":effort}));
    }
    if let Some(tier) = tier {
        options.push(json!({"id":"serviceTier","value":tier}));
    }
    json!({"instanceId":provider.key(),"model":model.map_or("default", |model|model.id.as_str()),"options":options})
}

fn add_requests(
    projection: &mut ThreadProjection,
    requests: &std::collections::BTreeMap<
        agent_protocol::ids::RequestId,
        Arc<agent_protocol::requests::Request>,
    >,
) {
    use agent_protocol::requests::{ApprovalKind, ChoiceEffect, RequestBody};
    for (id, request) in requests {
        let (kind, body) = match &request.body {
            RequestBody::Permission {
                description,
                details,
                choices,
            }
            | RequestBody::Approval {
                description,
                details,
                choices,
                ..
            } => {
                let kind = match &request.body {
                    RequestBody::Permission { .. } => "permission",
                    RequestBody::Approval {
                        kind: ApprovalKind::FileChange,
                        ..
                    } => "file-change",
                    _ => "command",
                };
                let options = choices
                    .iter()
                    .map(|choice| model::ApprovalOption {
                        decision: match choice.effect {
                            ChoiceEffect::Allow => model::ApprovalDecision::Accept,
                            ChoiceEffect::AllowLasting => model::ApprovalDecision::AcceptForSession,
                            ChoiceEffect::Deny => model::ApprovalDecision::Decline,
                        },
                        label: choice.label.clone(),
                        warning: Some(choice.description.clone()),
                    })
                    .collect();
                (
                    kind,
                    model::TurnItemBody::ApprovalRequest {
                        request_id: id.to_string(),
                        request_kind: kind.into(),
                        prompt: Some(description.clone()),
                        reason: Some(details.clone()),
                        options,
                    },
                )
            }
            RequestBody::Question { questions } => (
                "user_input",
                model::TurnItemBody::UserInputRequest {
                    request_id: id.to_string(),
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
                                    value: Some(choice.id.clone()),
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
            _ => panic!(
                "request fixture requires a canonical card: {:?}",
                request.body
            ),
        };
        let node = format!("request-node:{id}");
        let run = match &request.target {
            agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => {
                Some(turn_id.to_string())
            }
            _ => None,
        };
        projection.runtime_requests.push(model::RuntimeRequest {
            id: id.to_string(),
            node_id: node.clone(),
            provider_turn_id: None,
            kind: kind.into(),
            native_request_ref: None,
            status: if request.delivery == agent_protocol::session::RequestDelivery::Awaiting {
                model::RequestStatus::Pending
            } else {
                model::RequestStatus::Resolved
            },
            response_capability: model::ResponseCapability::Live {
                provider_session_id: "fixture-provider-session".into(),
                supports_feedback: None,
            },
            created_at: NOW.into(),
            resolved_at: None,
            decision: None,
            feedback: None,
            answers: None,
        });
        push_item(
            projection,
            model::TurnItem {
                detail_omitted: false,
                id: format!("request-item:{id}"),
                thread_id: projection.thread.id.clone(),
                run_id: run,
                node_id: Some(node),
                provider_thread_id: None,
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal: 0,
                status: model::TurnItemStatus::Waiting,
                title: None,
                started_at: Some(NOW.into()),
                completed_at: None,
                updated_at: NOW.into(),
                body,
            },
            false,
        );
    }
}

fn run(
    thread: &str,
    id: &str,
    message: &str,
    ordinal: u64,
    status: model::RunStatus,
    selection: &Value,
) -> model::Run {
    serde_json::from_value(json!({"id":id,"threadId":thread,"ordinal":ordinal,"providerInstanceId":selection["instanceId"],"modelSelection":selection,"providerThreadId":null,"userMessageId":message,"rootNodeId":null,"activeAttemptId":null,"status":status,"queuePosition":null,"queueHeld":false,"requestedAt":NOW,"startedAt":NOW,"completedAt":if matches!(status,model::RunStatus::Running|model::RunStatus::Starting|model::RunStatus::Preparing|model::RunStatus::Queued) {None} else {Some(NOW)},"checkpointId":null,"contextHandoffId":null})).unwrap()
}

fn push_item(projection: &mut ThreadProjection, mut item: model::TurnItem, visible: bool) {
    if let Some(existing) = projection
        .turn_items
        .iter_mut()
        .find(|existing| existing.id == item.id)
    {
        item.ordinal = existing.ordinal;
        let item = Arc::new(item);
        *existing = item.clone();
        if let Some(visible) = projection
            .visible_turn_items
            .iter_mut()
            .find(|visible| visible.source_item_id == item.id)
        {
            visible.item = item;
        }
        return;
    }
    item.ordinal = projection.turn_items.len() as u64 + 1;
    let item = Arc::new(item);
    projection.turn_items.push(item.clone());
    if visible {
        projection.visible_turn_items.push(ProjectedTurnItem {
            position: projection.visible_turn_items.len() as u64 + 1,
            visibility: Visibility::Local,
            source_thread_id: projection.thread.id.clone(),
            source_item_id: item.id.clone(),
            item,
        });
    }
}

pub(crate) fn item_record(
    thread: &str,
    run: Option<&str>,
    item: &agent_protocol::items::Item,
    provider: ProviderKind,
    queued: bool,
) -> model::TurnItem {
    let body = body(item, queued);
    let status = match item.status {
        ItemStatus::Running | ItemStatus::Unknown => model::TurnItemStatus::Running,
        ItemStatus::Completed => model::TurnItemStatus::Completed,
        ItemStatus::Failed => model::TurnItemStatus::Failed,
        ItemStatus::Declined => model::TurnItemStatus::Cancelled,
        ItemStatus::Interrupted => model::TurnItemStatus::Interrupted,
    };
    model::TurnItem {
        detail_omitted: false,
        id: item
            .client_input_id
            .as_ref()
            .map_or_else(|| item.id.to_string(), ToString::to_string),
        thread_id: thread.into(),
        run_id: run.map(str::to_owned),
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: Some(model::ProviderRef {
            driver: provider.driver().into(),
            native_id: Some(item.id.to_string()),
            strength: model::NativeRefStrength::Strong,
            fingerprint: None,
            ordinal: None,
        }),
        parent_item_id: None,
        ordinal: 0,
        status,
        title: None,
        started_at: Some(NOW.into()),
        completed_at: (status != model::TurnItemStatus::Running).then(|| NOW.into()),
        updated_at: NOW.into(),
        body,
    }
}

fn body(item: &agent_protocol::items::Item, queued: bool) -> model::TurnItemBody {
    use model::TurnItemBody as Body;
    match item.body() {
        ItemBody::UserMessage {text,content}=>{
            let text=text.clone().unwrap_or_else(||content.iter().filter_map(|part|if let MessagePart::Text {text}=part {Some(text.as_str())} else {None}).collect::<Vec<_>>().join("\n"));
            let mut attachments=vec![];
            let mut records=vec![];
            for part in content {
                match part {
                    MessagePart::Image {source} if agent_protocol::paths::is_absolute_host_path(source) || agent_protocol::paths::attachment_file_name(source).is_some()=>{
                        let name=source.rsplit(['/', '\\']).next().unwrap_or("image.png");
                        let id=uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL,source.as_bytes()).to_string();
                        attachments.push(json!({"type":"image","id":format!("fixture-{id}-png"),"name":name,"mimeType":"image/png","sizeBytes":0}));
                    }
                    MessagePart::Attachment {name,path}=>records.push(json!({"version":1,"kind":"mention","label":name,"path":path})),
                    MessagePart::Invocation {name,..}=>records.push(json!({"version":1,"kind":"skill","label":name,"name":name})),
                    MessagePart::Context {context}=>records.push(context.record()),
                    _=>{}
                }
            }
            Body::UserMessage {created_by:model::Actor::User,creation_source:model::CreationSource::Mobile,scheduled_task_id:None,message_id:item.client_input_id.as_ref().map_or_else(||item.id.to_string(),ToString::to_string),input_intent:if queued {"queued_turn"} else {"turn_start"}.into(),text,attachments,context:(!records.is_empty()).then(||json!({"version":1,"records":records}))}
        }
        ItemBody::AssistantText {text,phase}=>Body::AssistantMessage {message_id:item.id.to_string(),text:text.clone(),phase:match phase {agent_protocol::items::AssistantPhase::Final=>Some("final_answer".into()),agent_protocol::items::AssistantPhase::Commentary=>Some("commentary".into()),_=>None},attachments:vec![],streaming:item.status==ItemStatus::Running},
        ItemBody::Reasoning {summary,content}=>Body::Reasoning {text:summary.iter().chain(content).cloned().collect::<Vec<_>>().join("\n"),streaming:item.status==ItemStatus::Running},
        ItemBody::CommandExecution {command,output,exit_code,output_omitted,..}=>Body::CommandExecution {input:command.clone(),output:(!item.is_deferred()).then(||output.clone()),exit_code:*exit_code,output_omitted:(item.is_deferred()||*output_omitted).then_some(true),output_indicates_failure:None},
        ItemBody::FileChange {changes,..}=>Body::FileChange {file_name:changes.first().map_or_else(String::new,|change|change.path.clone()),changes:changes.iter().map(|change|json!({"operation":match change.kind {agent_protocol::items::FileChangeKind::Add=>"add",agent_protocol::items::FileChangeKind::Delete=>"delete",_=>"update"},"path":change.path})).collect(),diff_str:changes.first().and_then(|change|change.diff.clone())},
        ItemBody::Plan {text}=>Body::ProposedPlan {plan_id:item.id.to_string(),markdown:text.clone(),streaming:item.status==ItemStatus::Running},
        ItemBody::ToolCall {tool,arguments,result,error,..}=>Body::DynamicTool {tool_name:Some(tool.clone()),input:arguments.clone(),output:result.clone().or(error.clone()),output_omitted:item.is_deferred().then_some(true),viewed_image_path:None},
        ItemBody::ImageGeneration {saved_path,data,revised_prompt}=>Body::DynamicTool {tool_name:Some("image_gen.imagegen".into()),input:revised_prompt.as_ref().map_or(Value::Null,|prompt|json!({"prompt":prompt})),output:Some(json!({"status":"completed","savedPath":saved_path.clone().or_else(||data.as_ref().map(|data|format!("data:image/png;base64,{data}")))})),output_omitted:item.is_deferred().then_some(true),viewed_image_path:None},
        ItemBody::ImageView {path}=>Body::DynamicTool {tool_name:Some("view_image".into()),input:json!({"path":path}),output:None,output_omitted:None,viewed_image_path:Some(path.clone())},
        ItemBody::WebSearch {query,..}=>Body::WebSearch {patterns:vec![query.clone()],results:vec![]},
        ItemBody::Subagent {receivers,prompt,result,..}=>Body::Subagent {subagent_id:item.id.to_string(),origin:"native".into(),driver:receivers.first().and_then(|id|id.provider).unwrap_or(ProviderKind::Codex).driver().into(),provider_instance_id:receivers.first().and_then(|id|id.provider).unwrap_or(ProviderKind::Codex).key().into(),child_thread_id:receivers.first().map(|id|id.application_id()),prompt:prompt.clone().unwrap_or_default(),result:result.as_ref().map(ToString::to_string),progress:None},
        ItemBody::Compaction {}=>Body::Compaction {driver:None,summary:None,before_token_count:None,after_token_count:None},
        _=>Body::SystemNotice {message:serde_json::to_string(item.body()).unwrap()},
    }
}
