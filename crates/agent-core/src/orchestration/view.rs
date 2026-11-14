//! Disposable native display DTOs built directly from the canonical projection.
//! Grouping never invents persisted runs or changes the inherited row order.
use super::*;
use agent_protocol::{execution as display, items, models, operations as op, requests, session};

pub fn project(
    snapshot: &ThreadSnapshot,
    identity: &session::SessionRef,
    provider: session::ProviderKind,
    capabilities: session::Capabilities,
    project_root: Option<&str>,
    details: &BTreeMap<(String, String), Arc<model::TurnItem>>,
) -> Result<models::ThreadResponse, String> {
    let projection = &snapshot.projection;
    let cwd = projection.thread.worktree_path.as_deref().or(project_root);
    if identity.application_id() != projection.thread.id {
        return Err("conversation identity changed".into());
    }
    let id = identity.clone();
    let turns = timeline(projection, provider, cwd, details, capabilities.fork)?;
    let mut queue = op::InputQueue::default();
    let mut preparations = Vec::new();
    let mut submissions = BTreeMap::new();
    let mut runs: Vec<_> = projection.runs.iter().collect();
    runs.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    for run in runs {
        let Some(item)=projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::UserMessage {message_id,..} if message_id==&run.user_message_id)) else {continue;};
        let submission = submission(&id, run, item, projection, provider)?;
        let uncertain = delivery_uncertain(projection, &item.id);
        let delivery = match run.status {
            model::RunStatus::Queued => {
                queue.held |= run.queue_held;
                queue.inputs.push(submission);
                session::SubmissionDelivery::Accepted { turn_id: None }
            }
            model::RunStatus::Preparing | model::RunStatus::Starting => {
                preparations.push(session::SessionPreparation {
                    submission,
                    status: if uncertain {
                        session::PreparationStatus::Unknown
                    } else if run.status == model::RunStatus::Preparing {
                        session::PreparationStatus::Preparing
                    } else {
                        session::PreparationStatus::Sending
                    },
                    stage: if run.status == model::RunStatus::Preparing {
                        session::PreparationStage::Workspace
                    } else {
                        session::PreparationStage::Provider
                    },
                });
                if uncertain {
                    session::SubmissionDelivery::Unknown
                } else {
                    session::SubmissionDelivery::Sending
                }
            }
            _ => session::SubmissionDelivery::Accepted {
                turn_id: Some(run.id.clone().into()),
            },
        };
        submissions.insert(run.user_message_id.clone().into(), delivery);
    }
    for item in &projection.turn_items {
        let model::TurnItemBody::UserMessage {
            message_id,
            input_intent,
            ..
        } = &item.body
        else {
            continue;
        };
        if input_intent != "steer" || !delivery_uncertain(projection, &item.id) {
            continue;
        }
        let run = projection
            .runs
            .iter()
            .find(|run| Some(&run.id) == item.run_id.as_ref())
            .ok_or("unconfirmed input's run is missing")?;
        preparations.push(session::SessionPreparation {
            submission: submission(&id, run, item, projection, provider)?,
            status: session::PreparationStatus::Unknown,
            stage: session::PreparationStage::Provider,
        });
        submissions.insert(
            message_id.clone().into(),
            session::SubmissionDelivery::Unknown,
        );
    }
    let active = projection
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                model::RunStatus::Preparing
                    | model::RunStatus::Starting
                    | model::RunStatus::Running
                    | model::RunStatus::Waiting
            )
        })
        .max_by_key(|run| run.ordinal);
    let background: Vec<_> = projection
        .provider_threads
        .iter()
        .flat_map(|thread| &thread.pending_background_tasks)
        .map(|task| display::BackgroundTask {
            id: task.task_id.clone(),
            kind: match task.kind.as_str() {
                "command" => display::BackgroundTaskKind::Command,
                "subagent" => display::BackgroundTaskKind::Subagent,
                "monitor" => display::BackgroundTaskKind::Monitor,
                _ => display::BackgroundTaskKind::BackgroundTask,
            },
            description: task.description.clone().or_else(|| task.command.clone()),
        })
        .collect();
    let selection = &projection.thread.model_selection;
    Ok(models::ThreadResponse {
        thread: models::Thread {
            id: Some(id),
            active_provider: Some(provider),
            name: Some(projection.thread.title.clone()),
            cwd: cwd.map(str::to_owned),
            git_branch: projection.thread.branch.clone(),
            parent_id: projection
                .thread
                .lineage
                .parent_thread_id
                .as_ref()
                .map(|id| session::SessionRef::new(None, id.clone()).map_err(str::to_owned))
                .transpose()?,
            project_id: projection
                .thread
                .project_id
                .clone()
                .map(models::ProjectMembership::Assigned)
                .unwrap_or(models::ProjectMembership::Unassigned {}),
            can_accept_direct_input: Some(
                !(projection.thread.lineage.relationship_to_parent.as_deref() == Some("subagent")
                    && projection.thread.creation_source == model::CreationSource::Provider),
            ),
            capabilities: Some(capabilities),
            steerable: capabilities.steer && active.is_some(),
            status: active
                .map(|run| match run.status {
                    model::RunStatus::Preparing | model::RunStatus::Starting => {
                        display::SessionStatus::Preparing
                    }
                    _ => display::SessionStatus::Running,
                })
                .unwrap_or(display::SessionStatus::Idle),
            updated_at: millis(Some(&projection.updated_at)).map(|at| at as f64 / 1000.0),
            turns: Some(turns),
            background,
            queue,
            preparations,
            submissions,
            requests: request_views(projection)?,
            history_has_more: Some(snapshot.has_more_history),
            history_cursor: snapshot.history_cursor.clone(),
            history_read_state: Some(session::HistoryReadState::new(
                session::HistoryReadKind::Complete,
                Vec::new(),
            )),
            ..Default::default()
        },
        model: Some(models::ModelRef {
            provider,
            id: selection.model.clone(),
        }),
        effort: selection
            .text_option(if provider == session::ProviderKind::Claude {
                "effort"
            } else {
                "reasoningEffort"
            })
            .map(str::to_owned),
        service_tier: selection.text_option("serviceTier").map(str::to_owned),
    })
}

fn millis(value: Option<&str>) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value?)
        .ok()?
        .timestamp_millis()
        .try_into()
        .ok()
}

fn driver_kind(driver: &str) -> Result<session::ProviderKind, String> {
    match driver {
        "codex" => Ok(session::ProviderKind::Codex),
        "claudeAgent" => Ok(session::ProviderKind::Claude),
        "dsh" => Ok(session::ProviderKind::DeepSeek),
        _ => Err("unsupported provider driver".into()),
    }
}

fn timeline(
    projection: &ThreadProjection,
    provider: session::ProviderKind,
    cwd: Option<&str>,
    details: &BTreeMap<(String, String), Arc<model::TurnItem>>,
    can_fork: bool,
) -> Result<Vec<Arc<models::Turn>>, String> {
    let mut turns: Vec<Arc<models::Turn>> = Vec::new();
    let mut previous = None;
    for row in &projection.visible_turn_items {
        let key = (
            row.source_thread_id.as_str(),
            row.item
                .run_id
                .as_deref()
                .or(row.item.provider_turn_id.as_deref()),
        );
        let imported_input = row.item.run_id.is_none()
            && row.item.provider_turn_id.is_none()
            && matches!(row.item.body, model::TurnItemBody::UserMessage { .. });
        if previous != Some(key) || imported_input {
            let local = row.visibility == Visibility::Local;
            let run = local
                .then(|| {
                    projection
                        .runs
                        .iter()
                        .find(|run| Some(run.id.as_str()) == row.item.run_id.as_deref())
                })
                .flatten();
            turns.push(Arc::new(models::Turn {
                id: row
                    .item
                    .run_id
                    .clone()
                    .or_else(|| row.item.provider_turn_id.clone())
                    .unwrap_or_else(|| {
                        format!("history:{}:{}", row.source_thread_id, row.source_item_id)
                    })
                    .into(),
                provider: if local {
                    Some(
                        run.and_then(|run| run.provider_thread_id.as_ref())
                            .and_then(|id| {
                                projection
                                    .provider_threads
                                    .iter()
                                    .find(|thread| &thread.id == id)
                            })
                            .map(|thread| driver_kind(&thread.driver))
                            .transpose()?
                            .unwrap_or(provider),
                    )
                } else {
                    row.item
                        .native_item_ref
                        .as_ref()
                        .and_then(|reference| driver_kind(&reference.driver).ok())
                },
                supports_fork: Some(local && run.is_some() && can_fork),
                status: run
                    .map(|run| run_status(run.status))
                    .unwrap_or(display::TurnStatus::Completed),
                items: Some(Vec::new()),
                started_at: run
                    .and_then(|run| millis(run.started_at.as_deref()))
                    .map(|at| at as f64 / 1000.0),
                started_at_ms: run.and_then(|run| millis(run.started_at.as_deref())),
                completed_at_ms: run.and_then(|run| millis(run.completed_at.as_deref())),
                ..Default::default()
            }));
            previous = Some(key);
        }
        let turn = Arc::make_mut(turns.last_mut().expect("row allocated its display group"));
        if matches!(&row.item.body,model::TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
        {
            turn.queued = true;
        }
        if row.visibility != Visibility::Local
            && matches!(
                row.item.status,
                model::TurnItemStatus::Running | model::TurnItemStatus::Waiting
            )
        {
            turn.status = display::TurnStatus::Running;
        }
        if let model::TurnItemBody::Error { failure, .. } = &row.item.body
            && row.item.status == model::TurnItemStatus::Failed
            && failure.class != "delivery_uncertain"
        {
            turn.error = Some(failure_view(failure));
        }
        let detail = details
            .get(&(row.source_thread_id.clone(), row.source_item_id.clone()))
            .filter(|detail| same_detail_revision(&row.item, detail));
        turn.items.as_mut().unwrap().extend(item_view(
            detail.map_or(row.item.as_ref(), |detail| detail.as_ref()),
            provider,
            cwd,
            detail.is_some(),
        )?);
    }
    turns.retain(|turn| {
        turn.items.as_ref().is_some_and(|items| !items.is_empty()) || turn.error.is_some()
    });
    Ok(turns)
}

fn run_status(status: model::RunStatus) -> display::TurnStatus {
    match status {
        model::RunStatus::Preparing
        | model::RunStatus::Queued
        | model::RunStatus::Starting
        | model::RunStatus::Running
        | model::RunStatus::Waiting => display::TurnStatus::Running,
        model::RunStatus::Failed => display::TurnStatus::Failed,
        model::RunStatus::Interrupted | model::RunStatus::Cancelled => {
            display::TurnStatus::Interrupted
        }
        model::RunStatus::Completed | model::RunStatus::RolledBack => {
            display::TurnStatus::Completed
        }
    }
}

fn failure_view(failure: &model::ProviderFailure) -> display::ExecutionError {
    display::ExecutionError {
        message: failure.message.clone(),
        provider_code: failure.code.clone(),
        category: match failure.class.as_str() {
            "usage_limit" => display::ErrorCategory::UsageLimit,
            "transport_error" => display::ErrorCategory::Network,
            "permission_error" => display::ErrorCategory::Policy,
            "validation_error" => display::ErrorCategory::InvalidInput,
            _ => display::ErrorCategory::Other,
        },
        ..Default::default()
    }
}

pub(crate) fn attachment_parts(
    attachments: &[serde_json::Value],
) -> Result<Vec<items::MessagePart>, String> {
    attachments
        .iter()
        .map(|attachment| {
            let id = attachment
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("attachment ID is missing")?;
            let name = attachment
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("attachment name is missing")?;
            let image = attachment.get("type").and_then(serde_json::Value::as_str) == Some("image");
            let extension = agent_protocol::paths::attachment_extension(
                name,
                image
                    .then(|| {
                        attachment
                            .get("mimeType")
                            .and_then(serde_json::Value::as_str)
                    })
                    .flatten(),
            );
            let source = format!(
                "{}{id}.{extension}",
                agent_protocol::paths::ATTACHMENT_SCHEME
            );
            Ok(
                if attachment.get("type").and_then(serde_json::Value::as_str) == Some("image") {
                    items::MessagePart::Image { source }
                } else {
                    items::MessagePart::Attachment {
                        name: name.into(),
                        path: source,
                    }
                },
            )
        })
        .collect()
}

pub(crate) fn context_parts(
    context: Option<&serde_json::Value>,
) -> Result<Vec<items::MessagePart>, String> {
    let Some(context) = context else {
        return Ok(vec![]);
    };
    context
        .get("records")
        .and_then(serde_json::Value::as_array)
        .ok_or("message context records are missing")?
        .iter()
        .map(|record| {
            let text = |field| {
                record
                    .get(field)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or("message context field is missing".to_owned())
            };
            match record.get("kind").and_then(serde_json::Value::as_str) {
                Some("mention") => Ok(items::MessagePart::Attachment {
                    name: text("label")?,
                    path: text("path")?,
                }),
                // T3 stores skills by name. The native executor resolves their current catalog path.
                Some("skill") => Ok(items::MessagePart::Invocation {
                    name: text("name")?,
                    path: String::new(),
                }),
                _ => Err("unknown message context kind".into()),
            }
        })
        .collect()
}

fn delivery_uncertain(projection: &ThreadProjection, item_id: &str) -> bool {
    projection.turn_items.iter().any(|error| {
        error.parent_item_id.as_deref()==Some(item_id) && error.status==model::TurnItemStatus::Failed
            && matches!(&error.body,model::TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")
    })
}

fn submission(
    id: &session::SessionRef,
    run: &model::Run,
    item: &model::TurnItem,
    projection: &ThreadProjection,
    fallback: session::ProviderKind,
) -> Result<op::Submission, String> {
    let model::TurnItemBody::UserMessage {
        message_id,
        text,
        attachments,
        context,
        ..
    } = &item.body
    else {
        return Err("run input is not a user message".into());
    };
    let provider = run
        .provider_thread_id
        .as_ref()
        .and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|thread| &thread.id == id)
        })
        .map(|thread| driver_kind(&thread.driver))
        .transpose()?
        .unwrap_or(fallback);
    let mut input = vec![op::Input::Text { text: text.clone() }];
    for part in attachment_parts(attachments)? {
        input.push(match part {
            items::MessagePart::Image { source } => op::Input::LocalImage { path: source },
            items::MessagePart::Attachment { path, name } => op::Input::Mention { path, name },
            _ => unreachable!(),
        });
    }
    for part in context_parts(context.as_ref())? {
        input.push(match part {
            items::MessagePart::Attachment { name, path } => op::Input::Mention { name, path },
            items::MessagePart::Invocation { name, path } => op::Input::Skill { name, path },
            _ => unreachable!("context contains mentions or skills"),
        });
    }
    Ok(op::Submission {
        thread_id: id.clone(),
        client_user_message_id: message_id.clone().into(),
        input,
        model: Some(models::ModelRef {
            provider,
            id: run.model_selection.model.clone(),
        }),
        effort: run
            .model_selection
            .text_option(if provider == session::ProviderKind::Claude {
                "effort"
            } else {
                "reasoningEffort"
            })
            .map(str::to_owned),
        service_tier: run
            .model_selection
            .text_option("serviceTier")
            .map(str::to_owned),
        follow_up: op::FollowUp::Queue,
    })
}

fn request_views(
    projection: &ThreadProjection,
) -> Result<BTreeMap<agent_protocol::ids::RequestId, Arc<requests::Request>>, String> {
    projection.runtime_requests.iter().filter(|request|request.status==model::RequestStatus::Pending).map(|request|{
        let item=projection.turn_items.iter().find(|item|item.node_id.as_ref()==Some(&request.node_id) && (
            matches!(&item.body,model::TurnItemBody::UserInputRequest {request_id,..}|model::TurnItemBody::ApprovalRequest {request_id,..} if request_id==&request.id)
            || matches!(&item.body,model::TurnItemBody::DynamicTool {..}) && matches!(request.kind.as_str(),"dynamic_tool_call"|"mcp-elicitation"))).ok_or("pending request card is missing")?;
        let body=match &item.body {
            model::TurnItemBody::UserInputRequest {questions,..}=>requests::RequestBody::Question {questions:questions.iter().map(|question|requests::Question {
                id:question.id.clone(),header:question.header.clone(),prompt:question.question.clone(),secret:false,
                allow_free_text:question.allow_custom_answer.unwrap_or(true),multiple:question.multi_select.unwrap_or(false),
                choices:question.options.iter().map(|option|requests::QuestionChoice {id:option.value.clone().unwrap_or_else(||option.label.clone()),label:option.label.clone(),description:option.description.clone()}).collect(),
            }).collect()},
            model::TurnItemBody::ApprovalRequest {prompt,reason,options,..}=> {
                let description=prompt.clone().unwrap_or_else(||"Approval required".into());
                let details=reason.clone().unwrap_or_default();
                let choices=options.iter().map(|option|requests::Choice {id:serde_json::to_value(option.decision).unwrap().as_str().unwrap().into(),label:option.label.clone(),description:option.warning.clone().unwrap_or_default(),
                    effect:match option.decision {model::ApprovalDecision::Accept=>requests::ChoiceEffect::Allow,model::ApprovalDecision::AcceptForSession|model::ApprovalDecision::AcceptAlways=>requests::ChoiceEffect::AllowLasting,_=>requests::ChoiceEffect::Deny}}).collect();
                if request.kind=="permission" {requests::RequestBody::Permission {description,details,choices}}
                else {requests::RequestBody::Approval {kind:match request.kind.as_str() {"file-change"=>requests::ApprovalKind::FileChange,"file-read"=>requests::ApprovalKind::Tool,_=>requests::ApprovalKind::Command},description,details,choices}}
            },
            model::TurnItemBody::DynamicTool {tool_name,input,..} if request.kind=="dynamic_tool_call"=>requests::RequestBody::ToolExecution {tool:tool_name.clone().unwrap_or_else(||"tool".into()),namespace:None,arguments:input.clone()},
            model::TurnItemBody::DynamicTool {..}=>return Err("elicitation prompt conversion is unavailable".into()),
            _=>unreachable!(),
        };
        let id:agent_protocol::ids::RequestId=request.id.clone().into();
        Ok((id.clone(),Arc::new(requests::Request {id,target:requests::RequestTarget::Turn {turn_id:item.run_id.clone().unwrap_or_else(||format!("history:{}",item.thread_id)).into(),item_id:Some(item.id.clone().into())},delivery:session::RequestDelivery::Awaiting,body})))
    }).collect()
}

fn item_view(
    item: &model::TurnItem,
    provider: session::ProviderKind,
    cwd: Option<&str>,
    has_detail: bool,
) -> Result<Vec<Arc<items::Item>>, String> {
    use model::TurnItemBody as Body;
    let body = match &item.body {
        Body::UserMessage {
            text,
            attachments,
            context,
            ..
        } => {
            let mut content = attachment_parts(attachments)?;
            content.extend(context_parts(context.as_ref())?);
            items::ItemBody::UserMessage {
                text: Some(text.clone()),
                content,
            }
        }
        Body::AssistantMessage { text, phase, .. } => items::ItemBody::AssistantText {
            text: text.clone(),
            phase: match phase.as_deref() {
                Some("commentary") => items::AssistantPhase::Commentary,
                Some("final_answer") => items::AssistantPhase::Final,
                _ => items::AssistantPhase::Unknown,
            },
        },
        Body::Reasoning { text, .. } => items::ItemBody::Reasoning {
            content: vec![text.clone()],
            summary: Vec::new(),
        },
        Body::WebSearch { patterns, .. } => items::ItemBody::WebSearch {
            query: patterns.join(" | "),
            action: Some(items::WebSearchAction::Search {
                query: patterns.first().cloned(),
                queries: patterns.clone(),
            }),
        },
        Body::Notification {
            source,
            outcome,
            summary,
            ..
        } => items::ItemBody::Notification {
            reports: vec![display::BackgroundReport {
                kind: match source.get("kind").and_then(serde_json::Value::as_str) {
                    Some("command" | "background_command") => display::BackgroundTaskKind::Command,
                    Some("subagent" | "delegated_task") => display::BackgroundTaskKind::Subagent,
                    Some("monitor") => display::BackgroundTaskKind::Monitor,
                    _ => display::BackgroundTaskKind::BackgroundTask,
                },
                label: Some(summary.clone()),
                outcome: match outcome.as_str() {
                    "completed" => display::BackgroundOutcome::Completed,
                    "failed" => display::BackgroundOutcome::Failed,
                    "cancelled" => display::BackgroundOutcome::Cancelled,
                    "updated" => display::BackgroundOutcome::Updated,
                    _ => display::BackgroundOutcome::Unknown,
                },
                exit_code: None,
            }],
        },
        Body::CommandExecution {
            input,
            output,
            exit_code,
            output_omitted,
            ..
        } => items::ItemBody::CommandExecution {
            command: input.clone(),
            cwd: cwd.map(str::to_owned),
            output: output.clone().unwrap_or_default(),
            exit_code: *exit_code,
            output_omitted: output_omitted == &Some(true),
        },
        Body::FileChange {
            file_name,
            changes,
            diff_str,
        } => items::ItemBody::FileChange {
            changes: if changes.is_empty() {
                vec![items::FileChange {
                    path: file_name.clone(),
                    kind: items::FileChangeKind::Unknown,
                    diff: diff_str.clone(),
                    proposal: None,
                }]
            } else {
                changes
                    .iter()
                    .enumerate()
                    .map(|(index, change)| items::FileChange {
                        path: change
                            .get("oldPath")
                            .or_else(|| change.get("path"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(file_name)
                            .into(),
                        kind: match change.get("operation").and_then(serde_json::Value::as_str) {
                            Some("add" | "create") => items::FileChangeKind::Add,
                            Some("delete") => items::FileChangeKind::Delete,
                            _ => items::FileChangeKind::Update {
                                move_path: change
                                    .get("oldPath")
                                    .and_then(serde_json::Value::as_str)
                                    .map(|_| {
                                        change
                                            .get("path")
                                            .and_then(serde_json::Value::as_str)
                                            .unwrap_or(file_name)
                                            .to_owned()
                                    }),
                            },
                        },
                        // T3 carries the first change's diff, not one diff per
                        // metadata entry in changes.
                        diff: (index == 0).then(|| diff_str.clone()).flatten(),
                        proposal: None,
                    })
                    .collect()
            },
            output: String::new(),
        },
        Body::ProposedPlan { markdown, .. } => items::ItemBody::Plan {
            text: markdown.clone(),
        },
        Body::TodoList {
            steps, explanation, ..
        } => items::ItemBody::Plan {
            text: explanation
                .iter()
                .cloned()
                .chain(steps.iter().map(|step| {
                    format!(
                        "- [{}] {}",
                        if step.status == "completed" { "x" } else { " " },
                        step.text
                    )
                }))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Body::Subagent {
            origin,
            prompt,
            result,
            child_thread_id,
            ..
        } => items::ItemBody::Subagent {
            tool: origin.clone(),
            prompt: Some(prompt.clone()),
            model: None,
            effort: None,
            sender: None,
            receivers: child_thread_id
                .as_ref()
                .map(|id| session::SessionRef::new(None, id.clone()).map_err(str::to_owned))
                .transpose()?
                .into_iter()
                .collect(),
            states: Vec::new(),
            agent_id: child_thread_id.clone(),
            result: result.clone().map(serde_json::Value::String),
        },
        Body::DynamicTool {
            tool_name,
            input,
            output,
            ..
        } => items::ItemBody::ToolCall {
            kind: items::ToolKind::Dynamic,
            tool: tool_name.clone().unwrap_or_else(|| "Tool".into()),
            server: None,
            namespace: None,
            resource_uri: None,
            plugin_id: None,
            arguments: input.clone(),
            result: output.clone(),
            error: None,
            content: Vec::new(),
            success: None,
            duration_ms: None,
        },
        Body::Compaction { .. } => items::ItemBody::Compaction {},
        Body::SystemNotice { message }
        | Body::RunInterruptRequest { message }
        | Body::RunInterruptResult { message } => items::ItemBody::Hook {
            fragments: vec![message.clone()],
        },
        Body::Handoff { summary, .. } => items::ItemBody::Custom {
            provider,
            kind: "context_handoff".into(),
            value: serde_json::to_value(summary).unwrap(),
        },
        Body::UserInputRequest { .. }
        | Body::ApprovalRequest { .. }
        | Body::Checkpoint { .. }
        | Body::Fork { .. }
        | Body::Error { .. } => return Ok(Vec::new()),
    };
    let status = match item.status {
        model::TurnItemStatus::Idle
        | model::TurnItemStatus::Pending
        | model::TurnItemStatus::Running
        | model::TurnItemStatus::Waiting => display::ItemStatus::Running,
        model::TurnItemStatus::Completed => display::ItemStatus::Completed,
        model::TurnItemStatus::Failed => display::ItemStatus::Failed,
        model::TurnItemStatus::Cancelled | model::TurnItemStatus::Interrupted => {
            display::ItemStatus::Interrupted
        }
    };
    let mut view = items::Item::new(item.id.clone().into(), status, body);
    if !has_detail && needs_detail(item) {
        view.defer();
    }
    if let Body::UserMessage { message_id, .. } = &item.body {
        view.client_input_id = Some(message_id.clone().into());
    }
    let mut views = vec![Arc::new(view)];
    if let Body::AssistantMessage { attachments, .. } = &item.body {
        for (attachment, part) in attachments.iter().zip(attachment_parts(attachments)?) {
            if let items::MessagePart::Image { source } = part {
                let id = attachment
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("image attachment ID is missing")?;
                views.push(Arc::new(items::Item::new(
                    format!("{}:attachment:{id}", item.id).into(),
                    status,
                    items::ItemBody::ImageGeneration {
                        saved_path: Some(source),
                        data: None,
                        revised_prompt: None,
                    },
                )));
            }
        }
    }
    Ok(views)
}

fn needs_detail(item: &model::TurnItem) -> bool {
    match &item.body {
        model::TurnItemBody::CommandExecution { output_omitted, .. } => {
            *output_omitted == Some(true)
        }
        model::TurnItemBody::DynamicTool {
            input,
            output_omitted,
            ..
        } => {
            *output_omitted == Some(true)
                || input.get("truncated") == Some(&serde_json::Value::Bool(true))
        }
        model::TurnItemBody::FileChange { diff_str, .. } => diff_str.is_none(),
        _ => false,
    }
}
