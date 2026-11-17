use super::*;
use agent_protocol::{
    orchestration as model,
    requests::{Answer, QuestionAnswer},
};
use serde_json::{Value, json};

pub(super) struct MessageContent {
    pub(super) text: String,
    pub(super) attachments: Vec<Value>,
    pub(super) context: Option<Value>,
}

/// The canonical event timestamp.
pub(in crate::host_rpc) fn at() -> String {
    chrono::DateTime::from_timestamp_millis(now())
        .expect("system time is in chrono's range")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) fn selection(
    base: &model::ModelSelection,
    requested: &records::Selection,
) -> model::ModelSelection {
    let changed = requested
        .model
        .as_ref()
        .is_some_and(|model| model.provider.key() != base.instance_id || model.id != base.model);
    let mut selected = if let Some(model) = &requested.model {
        model::ModelSelection {
            instance_id: model.provider.key().into(),
            model: model.id.clone(),
            options: if changed {
                vec![]
            } else {
                base.options.clone()
            },
        }
    } else {
        base.clone()
    };
    for (id, value) in [
        (
            if selected.instance_id == "claude" {
                "effort"
            } else {
                "reasoningEffort"
            },
            &requested.effort,
        ),
        ("serviceTier", &requested.service_tier),
    ] {
        if let Some(value) = value {
            selected.options.retain(|option| option.id != id);
            selected.options.push(model::ProviderOptionSelection {
                id: id.into(),
                value: model::ProviderOptionSelectionValue::Text(value.clone()),
            });
        }
    }
    selected
}

impl Runtime {
    pub(super) async fn default_selection(
        service: &HostRpcService,
        kind: ProviderKind,
        requested: &records::Selection,
    ) -> Result<model::ModelSelection, Failure> {
        if let Some(model) = &requested.model {
            service.creation_agent(kind, Some(model))?;
            return Ok(selection(
                &model::ModelSelection {
                    instance_id: kind.key().into(),
                    model: model.id.clone(),
                    options: vec![],
                },
                requested,
            ));
        }
        service.creation_agent(kind, None)?;
        let mut cursor = None;
        let mut first = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = service
                .inner
                .providers
                .models(&op::ListModels {
                    provider: kind,
                    limit: 100,
                    cursor,
                })
                .await?;
            for item in page.data {
                first.get_or_insert_with(|| item.model.clone());
                if item.is_default == Some(true) {
                    return Ok(selection(
                        &model::ModelSelection {
                            instance_id: kind.key().into(),
                            model: item.model.id,
                            options: vec![],
                        },
                        requested,
                    ));
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                break;
            };
            if !seen.insert(next.clone()) {
                return Err(Failure::new(
                    "invalid_models",
                    "model cursor did not advance",
                ));
            }
        }
        let first = first
            .ok_or_else(|| Failure::new("model_unavailable", "provider has no available model"))?;
        Ok(selection(
            &model::ModelSelection {
                instance_id: kind.key().into(),
                model: first.id,
                options: vec![],
            },
            requested,
        ))
    }

    /// A chat created without a model defers its default to the first input,
    /// so creating it never launches a provider CLI to read the catalog.
    async fn deferred_default(
        service: &HostRpcService,
        session: &SessionRef,
        command: &Command,
    ) -> Result<Option<model::ModelSelection>, Failure> {
        let Command::Submit { content, .. } = command else {
            return Ok(None);
        };
        if content.model.is_some() {
            return Ok(None);
        }
        let Some(thread) = service
            .inner
            .conversation
            .storage
            .thread_header(session)
            .map_err(storage_failure)?
            .filter(|thread| thread.model_selection.is_deferred())
        else {
            return Ok(None);
        };
        let kind = ProviderKind::from_instance_id(&thread.model_selection.instance_id)
            .map_err(|error| Failure::new("model_unavailable", error))?;
        let default =
            Self::default_selection(service, kind, &records::Selection::default()).await?;
        Ok(Some(model::ModelSelection {
            model: default.model,
            ..thread.model_selection
        }))
    }

    pub(super) async fn message_content(
        service: &HostRpcService,
        thread_id: &str,
        message_id: &str,
        parts: &[op::Input],
    ) -> Result<MessageContent, Failure> {
        let files = service.files().clone();
        let thread = thread_id.to_owned();
        let message = message_id.to_owned();
        let parts = parts.to_vec();
        tokio::task::spawn_blocking(move||{
            let mut texts=Vec::new();
            let mut attachments=Vec::new();
            let mut records=Vec::new();
            for (index,part) in parts.into_iter().enumerate() {
                let context_id=Uuid::new_v5(&Uuid::NAMESPACE_URL,format!("context:{thread}:{message}:{index}").as_bytes()).to_string();
                match part {
                    op::Input::Text {text}=>texts.push(text),
                    op::Input::LocalImage {path}=>{
                        let name=path.rsplit(['/', '\\']).next().unwrap_or("image");
                        attachments.push(files.persist_attachment(&thread,&message,index,&path,name,true).map_err(|error|error.to_string())?);
                    }
                    op::Input::Mention {path,name} if files.is_uploaded_attachment(&path)=>{
                        attachments.push(files.persist_attachment(&thread,&message,index,&path,&name,false).map_err(|error|error.to_string())?);
                    }
                    op::Input::Mention {path,name}=>records.push(json!({"version":1,"contextId":context_id,"kind":"mention","label":name,"path":path})),
                    op::Input::Skill {name,..}=>records.push(json!({"version":1,"contextId":context_id,"kind":"skill","label":name,"name":name})),
                    op::Input::Context {context}=>{
                        context.validate()?;
                        records.push(context.record());
                    }
                }
            }
            Ok::<_,String>(MessageContent {text:texts.join("\n"),attachments,context:(!records.is_empty()).then(||json!({"version":1,"records":records}))})
        }).await.map_err(|error|storage_failure(error.to_string()))?.map_err(|error|Failure::new("attachment_failed",error))
    }

    /// A paired client's command.
    pub(in crate::host_rpc) async fn change(
        service: &HostRpcService,
        request: &records::Change,
    ) -> Result<records::Acknowledged, Failure> {
        Self::change_from(service, request, Origin::Client).await
    }

    pub(in crate::host_rpc) async fn change_from(
        service: &HostRpcService,
        request: &records::Change,
        origin: Origin<'_>,
    ) -> Result<records::Acknowledged, Failure> {
        let runtime = &service.inner.conversation;
        if let Some(receipt) = runtime
            .storage
            .thread_receipt(
                &request.operation.to_string(),
                &request.session.application_id(),
                command_kind(&request.command),
            )
            .map_err(storage_failure)?
        {
            return Ok(receipt);
        }
        let _workspace = if matches!(
            request.command,
            Command::Submit { .. }
                | Command::ResumeQueue
                | Command::SteerInput(_)
                | Command::RetryInput { .. }
                | Command::ResolveUnknown { retry: true, .. }
                | Command::Answer { .. }
                | Command::Rewind(_)
        ) {
            let guard = service.inner.worktree_access.read().await;
            service.require_not_updating()?;
            Some(guard)
        } else {
            None
        };
        Self::ensure_imported(service, &request.session).await?;
        let thread_id = request.session.application_id();
        let content = match &request.command {
            Command::Submit { id, content } => {
                Some(Self::message_content(service, &thread_id, id.as_str(), &content.parts).await?)
            }
            Command::EditInput { id, parts } => {
                Some(Self::message_content(service, &thread_id, id.as_str(), parts).await?)
            }
            _ => None,
        };
        let deferred = Self::deferred_default(service, &request.session, &request.command).await?;
        let operation = request.operation.to_string();
        let item_ids = match &request.command {
            Command::RetryInput { id, .. } | Command::RemoveInput(id) => runtime
                .storage
                .input_item_ids(&request.session, id.as_str())
                .map_err(storage_failure)?,
            _ => Vec::new(),
        };
        let now = at();
        let kind = command_kind(&request.command);
        let fork_origin = if matches!(request.command, Command::Submit { .. }) {
            runtime
                .storage
                .fork_origin(&request.session)
                .map_err(storage_failure)?
        } else {
            None
        };
        let capabilities: std::collections::BTreeMap<_, _> = service
            .inner
            .providers
            .all()
            .into_iter()
            .map(|(kind, agent)| {
                let caps = agent.capabilities();
                (kind, (caps.steer, caps.fork))
            })
            .collect();
        let spec = super::super::storage::commands::Command {
            id: &operation,
            thread_id: &thread_id,
            kind,
            now: &now,
            item_ids: &item_ids,
        };
        let tails = runtime.outputs.checkpoints_by_thread().remove(&thread_id);
        let committed = runtime
            .storage
            .thread_command(spec, None, |state| {
                let kind = ProviderKind::from_instance_id(&state.thread.provider_instance_id)?;
                let (can_steer, _) = capabilities.get(&kind).copied().unwrap_or((false, false));
                let context = model::Context {
                    command_id: &operation,
                    now: &now,
                    next_run_ordinal: state.next_run_ordinal().ok_or("run ordinal exhausted")?,
                    can_steer,
                };
                let selected = match &request.command {
                    Command::Submit { content, .. } => {
                        content.model.as_ref().map_or(kind, |model| model.provider)
                    }
                    _ => kind,
                };
                let fork_origin = fork_origin
                    .as_ref()
                    .filter(|_| capabilities.get(&selected).is_some_and(|(_, fork)| *fork));
                decide(
                    &request.command,
                    state,
                    content.as_ref(),
                    deferred.as_ref(),
                    context,
                    fork_origin,
                    origin,
                )
                .map(|decision| decision.with_command_outputs(tails.as_ref()))
            })
            .map_err(storage_failure)?;
        Self::finish_output_events(service, &committed.events);
        runtime.prune_requests(&request.session);
        let receipt = committed.receipt;
        if receipt.error.is_none() {
            if matches!(request.command, Command::Interrupt { .. }) {
                Self::cancel_prepared_input(service, &request.session).map_err(storage_failure)?;
                Self::cancel_workspace(service, &request.session).map_err(storage_failure)?;
            } else {
                Self::schedule_workspace(service, &request.session).map_err(storage_failure)?;
            }
        }
        Ok(receipt)
    }
}

fn command_kind(command: &Command) -> &'static str {
    match command {
        Command::Submit { .. } => "message.dispatch",
        Command::ResumeQueue => "queue.resume",
        Command::RemoveInput(_) => "queued-run.cancel",
        Command::EditInput { .. } => "queued-run.edit",
        Command::MoveInput { .. } => "queued-run.reorder",
        Command::SteerInput(_) => "queued-message.promote-to-steer",
        Command::Interrupt { .. } => "run.interrupt",
        Command::Answer { .. } => "runtime-request.respond",
        Command::Rename(_) => "thread.metadata.update",
        Command::Rewind(_) => "checkpoint.rollback",
        Command::RetryInput { .. } => "prepared-run.retry",
        Command::ResolveUnknown { .. } => "provider-turn.resolve",
    }
}

fn input_run<'a>(state: &'a model::State, id: &ClientInputId) -> Result<&'a model::Run, String> {
    state
        .runs
        .values()
        .find(|run| run.user_message_id == id.as_str())
        .ok_or_else(|| "input no longer exists".into())
}

/// Who submits a message. A Host automation is a T3 scheduled task: it is not
/// human input, so it never advances the thread's authored recency.
#[derive(Debug, Clone, Copy)]
pub(in crate::host_rpc) enum Origin<'a> {
    Client,
    Automation(&'a str),
}

fn decide(
    command: &Command,
    state: &model::State,
    content: Option<&MessageContent>,
    deferred: Option<&model::ModelSelection>,
    context: model::Context<'_>,
    fork_origin: Option<&model::ProviderThreadFork>,
    origin: Origin<'_>,
) -> Result<model::Decision, String> {
    match command {
        Command::Submit {
            id,
            content: options,
        } => {
            let content = content.ok_or("message was not prepared")?;
            let selected = selection(
                deferred.unwrap_or(&state.thread.model_selection),
                &records::Selection {
                    model: options.model.clone(),
                    effort: options.effort.clone(),
                    service_tier: options.service_tier.clone(),
                },
            );
            model::admit(
                state,
                model::Input {
                    message_id: id.to_string(),
                    created_by: match origin {
                        Origin::Client => model::Actor::User,
                        Origin::Automation(_) => model::Actor::System,
                    },
                    creation_source: match origin {
                        Origin::Client => model::CreationSource::Mobile,
                        Origin::Automation(_) => model::CreationSource::Server,
                    },
                    scheduled_task_id: match origin {
                        Origin::Client => None,
                        Origin::Automation(id) => Some(id.to_owned()),
                    },
                    text: content.text.clone(),
                    attachments: content.attachments.clone(),
                    context: content.context.clone(),
                    driver: ProviderKind::from_instance_id(&selected.instance_id)?
                        .driver()
                        .into(),
                    model_selection: selected,
                    workspace_preparation: state
                        .thread
                        .workspace_launch
                        .as_ref()
                        .filter(|workspace| {
                            workspace.setup.as_ref().is_some_and(|setup| {
                                setup.can_cancel
                                    || setup.phase
                                        == agent_protocol::workspace::SetupPhase::Cancelled
                                    || setup.phase == agent_protocol::workspace::SetupPhase::Failed
                            })
                        })
                        .and_then(|workspace| {
                            workspace
                                .strategy
                                .as_ref()
                                .and_then(model::WorkspacePreparation::for_strategy)
                        }),
                    fork_origin: fork_origin.cloned(),
                    fresh_context: options.fresh_context,
                },
                if options.follow_up == op::FollowUp::Steer {
                    model::Admission::Steer
                } else {
                    model::Admission::Queue
                },
                context,
            )
        }
        Command::ResumeQueue => {
            let mut decision = model::hold_queue(state, false);
            let unheld = state.apply(&decision);
            if unheld.active_run().is_none()
                && !unheld.native_work_blocks_delivery()
                && let Some(run) = unheld
                    .runs
                    .values()
                    .filter(|run| run.status == model::RunStatus::Queued)
                    .min_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal))
            {
                let start = model::activate_queued(&unheld, &run.id, context)?;
                decision.changes.extend(start.changes);
                decision.effects.extend(start.effects);
            }
            Ok(decision)
        }
        Command::RemoveInput(id) => {
            model::remove_input(state, &input_run(state, id)?.id, context.now)
        }
        Command::EditInput { id, .. } => {
            let content = content.ok_or("message was not prepared")?;
            model::edit_queued(
                state,
                &input_run(state, id)?.id,
                content.text.clone(),
                content.attachments.clone(),
                content.context.clone(),
                context.now,
            )
        }
        Command::MoveInput { id, before } => model::move_queued(
            state,
            &input_run(state, id)?.id,
            before
                .as_ref()
                .map(|id| input_run(state, id).map(|run| run.id.as_str()))
                .transpose()?,
        ),
        Command::SteerInput(id) => model::promote_queued(state, &input_run(state, id)?.id, context),
        Command::Interrupt { turn: Some(turn) } => {
            match state.runs.get(turn.as_str()).or_else(|| {
                state
                    .runs
                    .values()
                    .find(|run| run.user_message_id == turn.as_str())
            }) {
                Some(run) => model::interrupt(state, &run.id, context),
                None => model::interrupt_provider_turn(state, turn.as_str(), context),
            }
        }
        Command::Interrupt { turn: None } => match state.active_run() {
            Some(run) => model::interrupt(state, &run.id, context),
            None => Ok(model::hold_queue(state, true)),
        },
        Command::Rename(title) => model::titles::rename(state, title, context.now),
        Command::Rewind(run) => model::rewind(state, run.as_str(), context),
        Command::Answer { id, answer } => model::respond(
            state,
            id.as_str(),
            response(answer)?,
            ProviderKind::from_instance_id(&state.thread.provider_instance_id)?.driver(),
            context,
        ),
        Command::RetryInput { id, work_locally } => {
            model::retry_input(state, &input_run(state, id)?.id, *work_locally, context)
        }
        Command::ResolveUnknown { id, retry: false } => {
            model::cancel_uncertain(state, id.as_str(), context)
        }
        Command::ResolveUnknown { .. } => {
            Err("execution recovery is not available for this run".into())
        }
    }
}

fn response(answer: &Answer) -> Result<model::RequestResponse, String> {
    match answer {
        Answer::Approval { choice_id } | Answer::Permission { choice_id } => {
            Ok(model::RequestResponse::Approval {
                decision: serde_json::from_value(Value::String(choice_id.clone()))
                    .map_err(|error| error.to_string())?,
                feedback: None,
            })
        }
        Answer::Questions { answers } => Ok(model::RequestResponse::UserInput {
            answers: answers
                .iter()
                .map(|(id, answer)| {
                    (
                        id.clone(),
                        match answer {
                            QuestionAnswer::FreeText { text } => text.clone().into(),
                            QuestionAnswer::SingleChoice { choice_id } => choice_id.clone().into(),
                            QuestionAnswer::MultipleChoices { choice_ids } => json!(choice_ids),
                        },
                    )
                })
                .collect(),
        }),
        Answer::SkipQuestions => Ok(model::RequestResponse::UserInput {
            answers: Default::default(),
        }),
        Answer::ToolExecution { success, content } => Ok(model::RequestResponse::Tool {
            result: json!({"success":success,"content":content}),
        }),
        Answer::Elicitation { action } => Ok(model::RequestResponse::Tool {
            result: serde_json::to_value(action).map_err(|error| error.to_string())?,
        }),
    }
}
