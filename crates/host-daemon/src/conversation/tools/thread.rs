//! The thread toolkit and its caller checks.
use super::orchestrator::{
    interaction_mode_name, latest_active_run, option_selections, resolve_interaction_mode,
    resolve_runtime_mode, runtime_mode_name,
};
use super::{
    AgentTools, Outcome, Scope, ToolError, decode, failure, invalid, new_command, thread_id,
    unavailable,
};
use agent_domain::{
    Answer, Command, ContextDeliveryStatus, MessageAuthor, ModelSelection, RequestBody,
    RequestStatus, RunId, RunStatus, RuntimeRequestId, SourcePoint, State, ThreadId, Timestamp,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

/// A caller and the thread it addresses.
pub(crate) struct Access {
    pub(crate) caller: Arc<State>,
    pub(crate) target: Arc<State>,
}

fn source_point(value: &Value) -> Result<SourcePoint, ToolError> {
    let id = |key: &str| {
        value[key]
            .as_str()
            .ok_or_else(|| invalid(format!("sourcePoint.{key} is required")))
    };
    match value["type"].as_str() {
        Some("latest_stable") => Ok(SourcePoint::LatestStable),
        Some("run") => Ok(SourcePoint::Run(
            RunId::new(id("runId")?).map_err(|e| invalid(e.to_string()))?,
        )),
        Some("checkpoint") => Ok(SourcePoint::Checkpoint(
            agent_domain::CheckpointId::new(id("checkpointId")?)
                .map_err(|e| invalid(e.to_string()))?,
        )),
        _ => Err(invalid("Invalid sourcePoint")),
    }
}
fn run_id(value: &str) -> Result<RunId, ToolError> {
    RunId::new(value).map_err(|e| invalid(e.to_string()))
}
/// An entry for a queued run: the first `limit` characters (code points) of
/// its text.
fn queue_entry(state: &State, run: &RunId, limit: usize) -> Option<Value> {
    let run = state
        .runs
        .iter()
        .find(|candidate| &candidate.id == run && candidate.status == RunStatus::Queued)?;
    let message = state.message(&run.message)?;
    let count = message.text.chars().count();
    Some(json!({
        "queuedRunId": run.id,
        "text": message.text.chars().take(limit).collect::<String>(),
        "truncated": count > limit,
    }))
}
fn transfer_status(transfer: &agent_domain::Transfer) -> &'static str {
    if transfer.superseded {
        return "superseded";
    }
    match transfer.delivery.as_ref().map(|delivery| delivery.status) {
        None => "pending",
        Some(ContextDeliveryStatus::Pending) => "resolved_portable",
        Some(
            ContextDeliveryStatus::Injected
            | ContextDeliveryStatus::Inline
            | ContextDeliveryStatus::NativeFork,
        ) => "consumed",
    }
}
pub(crate) fn model_selection_json(selection: &ModelSelection) -> Value {
    let mut value = json!({"instanceId": selection.instance, "model": selection.model});
    if !selection.options.is_empty() {
        value["options"] = json!(
            selection
                .options
                .iter()
                .map(|(id, value)| json!({"id": id, "value": value}))
                .collect::<Vec<_>>()
        );
    }
    value
}
fn answers(value: &Value) -> Result<BTreeMap<String, Answer>, ToolError> {
    let Some(entries) = value.as_object() else {
        return Err(invalid("answers must be an object"));
    };
    Ok(entries
        .iter()
        .map(|(id, value)| {
            let answer = match value {
                Value::String(text) => Answer::Text(text.clone()),
                Value::Array(values) if values.iter().all(Value::is_string) => Answer::Choices(
                    values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect(),
                ),
                other => Answer::Text(other.to_string()),
            };
            (id.clone(), answer)
        })
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadInput {
    thread_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchInput {
    query: String,
    limit: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForkInput {
    source_point: Value,
    title: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeBackInput {
    target_thread_id: String,
    source_point: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureInput {
    model_selection: SelectionInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SelectionInput {
    pub(crate) instance_id: String,
    pub(crate) model: String,
    pub(crate) options: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestInput {
    thread_id: Option<String>,
    request_id: String,
    answers: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrganizeInput {
    thread_id: Option<String>,
    action: String,
    snoozed_until: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueListInput {
    thread_id: Option<String>,
    cursor: Option<u64>,
    limit: Option<u64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueInput {
    thread_id: Option<String>,
    queued_run_id: String,
    text: Option<String>,
    #[serde(default, deserialize_with = "present")]
    before_run_id: Option<Value>,
    target_run_id: Option<String>,
}
/// A key that is present, even when null.
fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

impl AgentTools {
    /// threadAccess `readCaller`.
    pub(crate) async fn read_caller(&self, scope: Scope<'_>) -> Result<Arc<State>, ToolError> {
        let caller = self.state(scope.thread).await.map_err(|_| unavailable())?;
        if caller
            .thread
            .as_ref()
            .is_none_or(|thread| thread.deleted_at.is_some())
        {
            return Err(failure(
                "thread_not_found",
                "The calling thread was not found.",
            ));
        }
        Ok(caller)
    }
    /// threadAccess `assertLiveCaller`.
    pub(crate) fn assert_live_caller(scope: Scope<'_>, caller: &State) -> Result<(), ToolError> {
        let thread = caller.thread.as_ref().expect("loaded");
        if thread.archived_at.is_some()
            || latest_active_run(caller).is_none()
            || thread.selection.instance != scope.instance
        {
            return Err(failure(
                "parent_not_active",
                "The calling provider no longer owns an active thread run.",
            ));
        }
        Ok(())
    }
    /// threadAccess `readMutationCaller`.
    pub(crate) async fn read_mutation_caller(
        &self,
        scope: Scope<'_>,
    ) -> Result<Arc<State>, ToolError> {
        let caller = self.read_caller(scope).await?;
        Self::assert_live_caller(scope, &caller)?;
        Ok(caller)
    }
    /// threadAccess `readThread`.
    async fn read_access(
        &self,
        scope: Scope<'_>,
        thread: Option<&String>,
    ) -> Result<Access, ToolError> {
        let caller = self.read_caller(scope).await?;
        let target_id = match thread {
            Some(id) => thread_id(id)?,
            None => scope.thread.clone(),
        };
        let project = caller.thread.as_ref().expect("loaded").project.clone();
        let target = self.state(&target_id).await.map_err(|_| unavailable())?;
        if target
            .thread
            .as_ref()
            .is_none_or(|thread| thread.project != project || thread.deleted_at.is_some())
        {
            return Err(failure(
                "thread_not_found",
                "The thread was not found in the calling project.",
            ));
        }
        Ok(Access { caller, target })
    }
    /// threadAccess `readWritableThread`.
    async fn writable_access(
        &self,
        scope: Scope<'_>,
        thread: Option<&String>,
    ) -> Result<Access, ToolError> {
        let access = self.read_access(scope, thread).await?;
        Self::assert_live_caller(scope, &access.caller)?;
        let caller = access.caller.thread.as_ref().expect("loaded");
        let target = access.target.thread.as_ref().expect("loaded");
        resolve_runtime_mode(
            caller.runtime_mode,
            Some(runtime_mode_name(target.runtime_mode)),
        )?;
        resolve_interaction_mode(
            caller.interaction_mode,
            Some(interaction_mode_name(target.interaction_mode)),
        )?;
        Ok(access)
    }
    async fn command(&self, thread: &ThreadId, command: Command) -> Outcome {
        let sequence = self
            .dispatch(thread, new_command(), command)
            .await
            .map_err(|_| unavailable())?;
        Ok(json!({"sequence": sequence}))
    }

    pub(crate) async fn search(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: SearchInput = decode(input)?;
        let query = input.query.trim().to_owned();
        let length = query.encode_utf16().count();
        if !(2..=200).contains(&length) {
            return Err(invalid("query must be 2 to 200 characters"));
        }
        if let Some(limit) = input.limit {
            super::bounded("limit", limit, 1, Some(50))?;
        }
        let caller = self.read_caller(scope).await?;
        let project = caller.thread.as_ref().expect("loaded").project.clone();
        let matches = self
            .backend
            .search(query, input.limit.map(|limit| limit as usize))
            .await
            .map_err(|_| unavailable())?;
        Ok(json!({
            "matches": matches.into_iter().filter(|m| m.project == project).map(|m| json!({
                "threadId": m.thread,
                "projectId": m.project,
                "source": m.source,
                "snippet": m.snippet,
                "messageCreatedAt": m.message_created_at,
            })).collect::<Vec<_>>(),
        }))
    }

    pub(crate) async fn fork(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ForkInput = decode(input)?;
        let source = source_point(&input.source_point)?;
        let title = input
            .title
            .as_deref()
            .map(|title| super::trimmed("title", title, None))
            .transpose()?;
        let access = self.writable_access(scope, None).await?;
        let thread = &access.target.thread.as_ref().expect("loaded").id;
        let command = new_command();
        let target = ThreadId::new(format!("{command}:fork")).expect("derived id");
        let sequence = self
            .dispatch(
                thread,
                command,
                Command::Fork {
                    target: target.clone(),
                    source,
                    title,
                    created_by: MessageAuthor::Agent,
                    creation_source: "mcp".into(),
                },
            )
            .await
            .map_err(|_| unavailable())?;
        Ok(json!({"sequence": sequence, "targetThreadId": target}))
    }

    pub(crate) async fn merge_back(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: MergeBackInput = decode(input)?;
        let source = source_point(&input.source_point)?;
        let access = self
            .writable_access(scope, Some(&input.target_thread_id))
            .await?;
        let target = access.target.thread.as_ref().expect("loaded").id.clone();
        let sequence = self
            .dispatch(
                scope.thread,
                new_command(),
                Command::MergeBack {
                    target: target.clone(),
                    source,
                },
            )
            .await
            .map_err(|_| unavailable())?;
        Ok(json!({"sequence": sequence, "targetThreadId": target}))
    }

    pub(crate) async fn transfers(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ThreadInput = decode(input)?;
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        Ok(json!({
            "transfers": access.target.transfers.iter().map(|transfer| json!({
                "id": transfer.id,
                "sourceThreadId": transfer.source,
                "targetThreadId": transfer.target,
                "status": transfer_status(transfer),
            })).collect::<Vec<_>>(),
        }))
    }

    pub(crate) async fn configuration(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ThreadInput = decode(input)?;
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        let thread = access.target.thread.as_ref().expect("loaded");
        Ok(json!({
            "threadId": thread.id,
            "modelSelection": model_selection_json(&thread.selection),
            "runtimeMode": runtime_mode_name(thread.runtime_mode),
            "interactionMode": interaction_mode_name(thread.interaction_mode),
        }))
    }

    /// A requested selection with the driver of its instance.
    pub(crate) async fn selection(
        &self,
        input: &SelectionInput,
    ) -> Result<ModelSelection, ToolError> {
        let instance = super::trimmed("instanceId", &input.instance_id, None)?;
        let model = super::trimmed("model", &input.model, None)?;
        let options = input
            .options
            .as_ref()
            .map(option_selections)
            .transpose()?
            .unwrap_or_default();
        let providers = self.backend.providers().await.map_err(|_| unavailable())?;
        let driver = providers
            .iter()
            .find(|provider| provider.instance == instance)
            .and_then(|provider| match provider.driver.as_str() {
                "codex" => Some(agent_domain::Driver::Codex),
                "claude" => Some(agent_domain::Driver::Claude),
                _ => None,
            })
            .ok_or_else(unavailable)?;
        Ok(ModelSelection {
            instance,
            driver,
            model,
            options: options
                .into_iter()
                .map(|(id, value)| {
                    let value = match value {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    (id, value)
                })
                .collect(),
        })
    }

    pub(crate) async fn configure(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ConfigureInput = decode(input)?;
        let access = self.writable_access(scope, None).await?;
        let thread = access.target.thread.as_ref().expect("loaded");
        let selection = self.selection(&input.model_selection).await?;
        let command = if selection.instance == thread.selection.instance {
            Command::SelectModel { selection }
        } else {
            Command::SwitchProvider { selection }
        };
        self.command(&thread.id.clone(), command).await
    }

    /// A pending user-input request with its card.
    fn question<'a>(
        state: &'a State,
        request: &RuntimeRequestId,
    ) -> Result<&'a agent_domain::Request, ToolError> {
        state
            .requests
            .iter()
            .find(|candidate| {
                &candidate.id == request
                    && candidate.status == RequestStatus::Pending
                    && matches!(candidate.body, RequestBody::Questions { .. })
            })
            .filter(|_| {
                state.items.iter().any(|item| {
                    matches!(&item.kind, agent_domain::ItemKind::UserInputRequest { request: id } if id == request)
                })
            })
            .ok_or_else(|| {
                failure(
                    "invalid_request",
                    "The pending user-input request was not found.",
                )
            })
    }

    pub(crate) async fn pending_requests(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ThreadInput = decode(input)?;
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        Ok(json!({
            "requestIds": access.target.requests.iter()
                .filter(|r| r.status == RequestStatus::Pending && matches!(r.body, RequestBody::Questions { .. }))
                .map(|r| &r.id)
                .collect::<Vec<_>>(),
        }))
    }

    pub(crate) async fn pending_request(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: RequestInput = decode(input)?;
        let request =
            RuntimeRequestId::new(input.request_id).map_err(|error| invalid(error.to_string()))?;
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        let found = Self::question(&access.target, &request)?;
        Ok(json!({
            "requestId": request,
            "questions": super::read::questions(&found.body).unwrap_or_default(),
        }))
    }

    pub(crate) async fn respond(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: RequestInput = decode(input)?;
        let request =
            RuntimeRequestId::new(input.request_id).map_err(|error| invalid(error.to_string()))?;
        let answers = answers(input.answers.as_ref().unwrap_or(&Value::Null))?;
        let access = self
            .writable_access(scope, input.thread_id.as_ref())
            .await?;
        Self::question(&access.target, &request)?;
        let thread = access.target.thread.as_ref().expect("loaded").id.clone();
        self.command(
            &thread,
            Command::Respond {
                request,
                decision: None,
                answers: Some(answers),
                attachments: BTreeMap::new(),
            },
        )
        .await
    }

    pub(crate) async fn organize(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: OrganizeInput = decode(input)?;
        let until = input
            .snoozed_until
            .as_deref()
            .map(|value| Timestamp::parse(value).map_err(|error| invalid(error.to_string())))
            .transpose()?;
        let action = input.action.as_str();
        if !matches!(
            action,
            "pin"
                | "unpin"
                | "snooze"
                | "unsnooze"
                | "settle"
                | "unsettle"
                | "archive"
                | "unarchive"
                | "mark_unread"
        ) {
            return Err(invalid("Invalid action"));
        }
        let access = self
            .writable_access(scope, input.thread_id.as_ref())
            .await?;
        let command = match action {
            "snooze" => {
                let Some(until) = until else {
                    return Err(failure("invalid_request", "snooze requires snoozedUntil."));
                };
                Command::Snooze { until: Some(until) }
            }
            "unsnooze" => Command::Snooze { until: None },
            "pin" => Command::Pin {
                pinned: true,
                order: None,
            },
            "unpin" => Command::Pin {
                pinned: false,
                order: None,
            },
            "settle" => Command::Settle {
                settled: true,
                at: None,
            },
            "unsettle" => Command::Settle {
                settled: false,
                at: None,
            },
            "archive" => Command::Archive { archived: true },
            "unarchive" => Command::Archive { archived: false },
            _ => Command::MarkUnread,
        };
        let thread = access.target.thread.as_ref().expect("loaded").id.clone();
        self.command(&thread, command).await
    }

    pub(crate) async fn queue_list(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: QueueListInput = decode(input)?;
        if let Some(limit) = input.limit {
            super::bounded("limit", limit, 1, Some(100))?;
        }
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        let runs = access.target.queued_runs();
        let cursor = input.cursor.unwrap_or(0) as usize;
        let end = cursor + input.limit.unwrap_or(20) as usize;
        Ok(json!({
            "items": runs.iter().skip(cursor).take(end.saturating_sub(cursor))
                .filter_map(|run| queue_entry(&access.target, &run.id, 1000))
                .collect::<Vec<_>>(),
            "nextCursor": (end < runs.len()).then_some(end),
        }))
    }

    pub(crate) async fn queue_read(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: QueueInput = decode(input)?;
        let run = run_id(&input.queued_run_id)?;
        let access = self.read_access(scope, input.thread_id.as_ref()).await?;
        queue_entry(&access.target, &run, 16_000)
            .ok_or_else(|| failure("invalid_request", "The queued message was not found."))
    }

    pub(crate) async fn queue_command(
        &self,
        scope: Scope<'_>,
        name: &str,
        input: &Value,
    ) -> Outcome {
        let input: QueueInput = decode(input)?;
        let run = run_id(&input.queued_run_id)?;
        let command = match name {
            "queue_edit" => {
                let text = input.text.ok_or_else(|| invalid("text is required"))?;
                if text.encode_utf16().count() > 100_000 {
                    return Err(invalid("text must be at most 100000 characters"));
                }
                Command::EditQueued {
                    run,
                    text,
                    attachments: None,
                    context: None,
                }
            }
            "queue_cancel" => Command::CancelQueued { run },
            "queue_reorder" => Command::ReorderQueued {
                run,
                before: match input
                    .before_run_id
                    .ok_or_else(|| invalid("beforeRunId is required"))?
                {
                    Value::Null => None,
                    Value::String(id) => Some(run_id(&id)?),
                    _ => return Err(invalid("beforeRunId must be a run id or null")),
                },
            },
            _ => Command::PromoteToSteer {
                queued: run,
                active: run_id(
                    &input
                        .target_run_id
                        .ok_or_else(|| invalid("targetRunId is required"))?,
                )?,
            },
        };
        let access = self
            .writable_access(scope, input.thread_id.as_ref())
            .await?;
        let thread = access.target.thread.as_ref().expect("loaded").id.clone();
        self.command(&thread, command).await
    }
}
