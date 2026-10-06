//! The projections the list and read tools share: thread detail, list items
//! and the summarized timeline text of each item.
use super::orchestrator::{latest_active_run, latest_run, runtime_mode_name};
use super::{AgentTools, Outcome, Scope, bounded, decode, failure, invalid, thread_id};
use agent_domain::{
    ItemKind, MessageAuthor, RequestBody, RequestStatus, Run, RunStatus, State, Thread, ThreadId,
    ThreadShell, TransferKind,
};
use agent_runtime::{HistoryRow, timeline_rows};
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_THREAD_READ_LIMIT: usize = 50;
const DEFAULT_THREAD_RUN_LIMIT: usize = 10;
const DEFAULT_THREAD_ITEM_MAX_CHARS: usize = 20_000;

/// Threads named by `thread` context records on the user's own messages. An
/// agent cannot widen its reach by writing a record.
fn user_attached_threads(parent: &State) -> std::collections::HashSet<&str> {
    parent
        .messages
        .iter()
        .filter(|message| {
            message.role == agent_domain::Role::User && message.created_by == MessageAuthor::User
        })
        .filter_map(|message| message.context.as_ref())
        .flat_map(|context| &context.records)
        .filter(|record| record.0["kind"] == "thread")
        .filter_map(|record| record.0["threadId"].as_str())
        .collect()
}
pub(crate) fn actor(author: MessageAuthor) -> &'static str {
    match author {
        MessageAuthor::User => "user",
        MessageAuthor::Agent => "agent",
    }
}
pub(crate) fn relationship(
    parent: Option<&ThreadId>,
    fork_boundary: Option<u64>,
) -> Option<&'static str> {
    parent.map(|_| {
        if fork_boundary.is_some() {
            "fork"
        } else {
            "subagent"
        }
    })
}
fn status_name(status: RunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
/// `activity_run_status`, falling back to `status`, or `idle` without runs.
pub(crate) fn shell_status(shell: &ThreadShell) -> String {
    shell
        .activity_run_status
        .or(shell.status)
        .map_or_else(|| "idle".to_owned(), status_name)
}
fn settlement(settled: Option<bool>, at: Option<&agent_domain::Timestamp>) -> (bool, Value) {
    let settled = settled == Some(true);
    (settled, if settled { json!(at) } else { Value::Null })
}
pub(crate) fn linked_pull_request(thread: &Thread) -> Value {
    thread.linked_pull_request.as_ref().map_or(Value::Null, |pr| {
        json!({"projectId": pr.project, "repository": pr.repository, "number": pr.number, "url": pr.url})
    })
}
pub(crate) fn title_regeneration(thread: &Thread) -> Value {
    thread.title_request.as_ref().map_or(
        Value::Null,
        |request| json!({"requestId": request.id, "startedAt": request.started_at}),
    )
}

pub(crate) fn list_item(shell: &ThreadShell) -> Value {
    let (settled, settled_at) = settlement(shell.settled, shell.settled_at.as_ref());
    json!({
        "threadId": shell.id,
        "title": shell.title,
        "createdBy": actor(shell.created_by),
        "creationSource": shell.creation_source,
        "status": shell_status(shell),
        "latestRunId": shell.latest_run,
        "providerInstanceId": shell.selection.instance,
        "model": shell.selection.model,
        "runtimeMode": runtime_mode_name(shell.runtime_mode),
        "interactionMode": super::orchestrator::interaction_mode_name(shell.interaction_mode),
        "linkedPullRequest": shell.linked_pull_request.as_ref().map(|pr| json!({"projectId": pr.project, "repository": pr.repository, "number": pr.number, "url": pr.url})),
        "settled": settled,
        "settledAt": settled_at,
        "parentThreadId": shell.parent,
        "relationshipToParent": relationship(shell.parent.as_ref(), shell.fork_boundary),
        "itemCount": shell.visible_item_count,
        "createdAt": shell.created_at,
        "updatedAt": shell.updated_at,
    })
}

fn thread_detail(state: &State, item_count: usize) -> Value {
    let thread = state.thread.as_ref().expect("loaded");
    let latest = latest_run(state);
    let active = latest_active_run(state);
    let (settled, settled_at) = settlement(thread.settled, thread.settled_at.as_ref());
    json!({
        "threadId": thread.id,
        "projectId": thread.project,
        "title": thread.title,
        "createdBy": actor(thread.created_by),
        "creationSource": thread.creation_source,
        "status": active.or(latest).map_or_else(|| "idle".to_owned(), |run| status_name(run.status)),
        "latestRunId": latest.map(|run| &run.id),
        "activeRunId": active.map(|run| &run.id),
        "providerInstanceId": thread.selection.instance,
        "model": thread.selection.model,
        "runtimeMode": runtime_mode_name(thread.runtime_mode),
        "interactionMode": super::orchestrator::interaction_mode_name(thread.interaction_mode),
        "linkedPullRequest": linked_pull_request(thread),
        "titleRegeneration": title_regeneration(thread),
        "branch": thread.workspace.as_ref().and_then(|w| w.branch.as_ref()),
        "worktreePath": thread.workspace.as_ref().and_then(|w| w.worktree_path.as_ref()),
        "parentThreadId": thread.parent,
        "relationshipToParent": relationship(thread.parent.as_ref(), thread.fork_boundary),
        "runCount": state.runs.len(),
        "itemCount": item_count,
        "pendingRequestCount": state.requests.iter().filter(|r| r.status == RequestStatus::Pending).count(),
        "archived": thread.archived_at.is_some(),
        "settled": settled,
        "settledAt": settled_at,
        "createdAt": thread.created_at,
        "updatedAt": thread.updated_at,
    })
}

fn thread_run(run: &Run) -> Value {
    json!({
        "runId": run.id,
        "ordinal": run.ordinal,
        "status": run.status,
        "providerInstanceId": run.selection.instance,
        "model": run.selection.model,
        "requestedAt": run.requested_at,
        "startedAt": run.started_at,
        "completedAt": run.completed_at,
    })
}

fn notification_of(row: &HistoryRow) -> Option<&agent_domain::Notification> {
    match &row.item.kind {
        ItemKind::Notification { notification } => Some(notification),
        ItemKind::UserMessage { .. } => row.message.as_ref()?.notification.as_ref(),
        _ => None,
    }
}

/// Turn item types.
pub(crate) fn item_type(row: &HistoryRow) -> &'static str {
    if notification_of(row).is_some() {
        return "notification";
    }
    match &row.item.kind {
        ItemKind::Fork { .. } => "fork",
        ItemKind::UserMessage { .. } => "user_message",
        ItemKind::AssistantMessage { .. } => "assistant_message",
        ItemKind::Reasoning => "reasoning",
        ItemKind::RunInterruptRequest => "run_interrupt_request",
        ItemKind::RunInterruptResult { .. } => "run_interrupt_result",
        ItemKind::CommandExecution { .. } => "command_execution",
        ItemKind::FileChange { .. } => "file_change",
        ItemKind::DynamicTool { .. } => "dynamic_tool",
        ItemKind::WebSearch { .. } => "web_search",
        ItemKind::ProposedPlan { .. } => "proposed_plan",
        ItemKind::TodoList { .. } => "todo_list",
        ItemKind::ApprovalRequest { .. } => "approval_request",
        ItemKind::UserInputRequest { .. } => "user_input_request",
        ItemKind::Subagent { .. } => "subagent",
        ItemKind::Compaction { .. } => "compaction",
        ItemKind::Error { .. } => "error",
        ItemKind::SystemNotice { .. } => "system_notice",
        ItemKind::Notification { .. } => "notification",
        ItemKind::ThreadCreated { .. } => "thread_created",
    }
}

/// `JSON.stringify(value, null, 2)`.
fn json_text(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

pub(crate) fn questions(body: &RequestBody) -> Option<Vec<Value>> {
    let RequestBody::Questions { questions } = body else {
        return None;
    };
    Some(
        questions
            .iter()
            .map(|question| {
                json!({
                    "id": question.id,
                    "header": question.header,
                    "question": question.question,
                    "options": question.options.iter().map(|option| json!({
                        "label": option.label,
                        "description": option.description.clone().unwrap_or_default(),
                    })).collect::<Vec<_>>(),
                    "multiSelect": question.multiple,
                    "required": question.required,
                })
            })
            .collect(),
    )
}

fn file_changes(changes: &Value) -> String {
    let render = |change: &Value| {
        [
            change["path"].as_str().map(str::to_owned),
            change["diff"]
                .as_str()
                .or(change["newStr"].as_str())
                .map(str::to_owned),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
    };
    match changes {
        Value::Array(changes) => changes.iter().map(render).collect::<Vec<_>>().join("\n"),
        Value::Object(_) => render(changes),
        other => json_text(other),
    }
}

/// The item's text and title.
fn item_text(state: &State, row: &HistoryRow) -> (Option<String>, Option<String>) {
    if let Some(notification) = notification_of(row) {
        let text = [
            Some(notification.summary.clone()),
            notification.detail.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
        return (Some(text), None);
    }
    let item = &row.item;
    let message_text = || {
        row.message
            .as_ref()
            .map_or_else(|| item.text.clone(), |m| m.text.clone())
    };
    match &item.kind {
        ItemKind::UserMessage { .. } | ItemKind::AssistantMessage { .. } => {
            (Some(message_text()), None)
        }
        ItemKind::Reasoning => (Some(item.text.clone()), None),
        ItemKind::ProposedPlan { .. } => (
            Some(
                row.plan
                    .as_ref()
                    .map_or_else(|| item.text.clone(), |plan| plan.markdown.clone()),
            ),
            None,
        ),
        ItemKind::TodoList { .. } => {
            let text = row.plan.as_ref().map_or_else(String::new, |plan| {
                std::iter::once(plan.markdown.clone())
                    .filter(|markdown| !markdown.is_empty())
                    .chain(
                        plan.steps
                            .iter()
                            .map(|step| format!("[{}] {}", step.status, step.text)),
                    )
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            (Some(text), None)
        }
        ItemKind::UserInputRequest { request } => (
            state
                .requests
                .iter()
                .find(|r| &r.id == request)
                .and_then(|r| questions(&r.body))
                .map(|questions| json_text(&json!(questions))),
            None,
        ),
        ItemKind::ApprovalRequest { request } => {
            let found = state.requests.iter().find(|r| &r.id == request);
            match found.map(|r| &r.body) {
                Some(RequestBody::Approval {
                    kind,
                    title,
                    detail,
                    ..
                }) => (
                    Some(detail.clone().unwrap_or_else(|| kind.clone())),
                    Some(title.clone()),
                ),
                _ => (None, None),
            }
        }
        ItemKind::FileChange { changes } => (Some(file_changes(&changes.0)), None),
        ItemKind::CommandExecution { command, title, .. } => (
            Some(
                [
                    Some(format!("$ {command}")),
                    Some(item.text.clone()).filter(|t| !t.is_empty()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            ),
            title.clone(),
        ),
        ItemKind::WebSearch { query, results } => (
            Some(json_text(
                &json!({"patterns": [query], "results": results.as_ref().map(|r| &r.0)}),
            )),
            None,
        ),
        ItemKind::RunInterruptRequest
        | ItemKind::RunInterruptResult { .. }
        | ItemKind::SystemNotice { .. } => {
            let text = match &item.kind {
                ItemKind::SystemNotice { message } => message.clone(),
                _ => item.text.clone(),
            };
            (Some(text), None)
        }
        ItemKind::Error { message, .. } => (Some(message.clone()), None),
        ItemKind::Notification { .. } => (None, None),
        ItemKind::Compaction { .. } => (Some(item.text.clone()).filter(|t| !t.is_empty()), None),
        ItemKind::Fork { .. } => (
            Some(format!(
                "Forked to thread {}.",
                state
                    .thread
                    .as_ref()
                    .map(|t| t.id.as_str())
                    .unwrap_or_default()
            )),
            Some("Forked from conversation".into()),
        ),
        ItemKind::ThreadCreated {
            thread,
            title,
            instance,
            model,
            ..
        } => (
            Some(format!(
                "Created thread {thread} with {instance} ({model})."
            )),
            Some(title.clone()),
        ),
        ItemKind::Subagent { task } => {
            let task = state.tasks.iter().find(|t| &t.id == task);
            (
                task.map(|task| {
                    task.result
                        .clone()
                        .or_else(|| task.progress.clone())
                        .unwrap_or_else(|| task.prompt.clone())
                }),
                task.and_then(|task| task.title.clone()),
            )
        }
        ItemKind::DynamicTool {
            presentation,
            name,
            input,
            output,
        } => (
            Some(json_text(
                &json!({"toolName": name, "input": input.0, "output": output.as_ref().map(|o| &o.0)}),
            )),
            presentation.title.clone(),
        ),
    }
}

/// The units of `text` from `offset` to `offset + max`, kept on character
/// boundaries: a character that starts in the range is included.
pub(crate) fn utf16_window(text: &str, offset: usize, max: usize) -> (String, bool, usize) {
    let end = offset + max;
    let mut units = 0;
    let mut window = String::new();
    for ch in text.chars() {
        let start = units;
        units += ch.len_utf16();
        if start >= offset && start < end {
            window.push(ch);
        }
    }
    (window, units > end, end)
}

fn timeline_item(state: &State, row: &HistoryRow, max: usize, offset: usize) -> Value {
    let (text, title) = item_text(state, row);
    let message = match &row.item.kind {
        ItemKind::UserMessage { message } | ItemKind::AssistantMessage { message } => Some(message),
        _ => None,
    };
    let (window, truncated, end) = text
        .as_deref()
        .map(|text| utf16_window(text, offset, max))
        .map_or((None, false, 0), |(window, truncated, end)| {
            (Some(window), truncated, end)
        });
    let (visibility, source) = match &row.item.kind {
        ItemKind::Fork { parent, .. } => ("synthetic", parent.clone()),
        _ if row.inherited => ("inherited", row.source.clone()),
        _ => ("local", row.source.clone()),
    };
    json!({
        "position": row.position,
        "visibility": visibility,
        "sourceThreadId": source,
        "itemId": row.item.id,
        "runId": row.item.run,
        "messageId": message,
        "createdBy": row.message.as_ref().filter(|_| message.is_some()).map(|m| actor(m.created_by)),
        "creationSource": row.message.as_ref().filter(|_| message.is_some()).map(|m| &m.creation_source),
        "type": item_type(row),
        "status": row.item.status,
        "title": title,
        "text": window,
        "textTruncated": truncated,
        "nextTextOffset": if truncated { json!(end) } else { Value::Null },
        "updatedAt": row.item.completed_at.as_ref().unwrap_or(&row.item.started_at),
    })
}

/// The message and item location of a subagent's result for a run.
fn result_location(
    target: &State,
    run: &Run,
) -> (
    Option<agent_domain::MessageId>,
    Option<agent_domain::TurnItemId>,
) {
    let failure = (run.status == RunStatus::Failed)
        .then(|| {
            target
                .items
                .iter()
                .filter(|item| {
                    item.run.as_ref() == Some(&run.id)
                        && matches!(item.kind, ItemKind::Error { .. })
                })
                .max_by_key(|item| item.ordinal)
        })
        .flatten();
    if let Some(failure) = failure {
        return (None, Some(failure.id.clone()));
    }
    let message = target
        .messages
        .iter()
        .filter(|m| {
            m.run.as_ref() == Some(&run.id)
                && m.role == agent_domain::Role::Assistant
                && !m.text.trim().is_empty()
        })
        .max_by(|a, b| a.updated_at.cmp(&b.updated_at));
    let item = target
        .items
        .iter()
        .filter(|item| {
            item.run.as_ref() == Some(&run.id)
                && matches!(item.kind, ItemKind::AssistantMessage { .. })
                && !item.text.trim().is_empty()
        })
        .max_by_key(|item| item.ordinal);
    let item_message = item.and_then(|item| match &item.kind {
        ItemKind::AssistantMessage { message } => Some(message.clone()),
        _ => None,
    });
    (
        message.map(|m| m.id.clone()).or(item_message),
        item.map(|item| item.id.clone()),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadInput {
    thread_id: String,
    item_id: Option<String>,
    text_offset: Option<u64>,
    view: Option<String>,
    after_position: Option<u64>,
    limit: Option<u64>,
    run_limit: Option<u64>,
    max_chars_per_item: Option<u64>,
}

impl AgentTools {
    /// The caller itself, a thread of its project, or a thread the user
    /// attached to the caller as context.
    async fn load_readable(
        &self,
        scope: Scope<'_>,
        target: &ThreadId,
    ) -> Result<(std::sync::Arc<State>, std::sync::Arc<State>), super::ToolError> {
        let parent = self.load_caller(scope).await?;
        let target_state = if target == scope.thread {
            parent.clone()
        } else {
            let project = parent.thread.as_ref().expect("loaded").project.clone();
            match self.load_project_thread(&project, target).await {
                Err(super::ToolError::Failure {
                    code: "thread_not_found",
                    ..
                }) if user_attached_threads(&parent).contains(target.as_str()) => {
                    self.state(target).await.map_err(|error| {
                        failure(
                            "orchestration_error",
                            format!("Unable to read thread {target}: {error}"),
                        )
                    })?
                }
                loaded => loaded?,
            }
        };
        if target_state
            .thread
            .as_ref()
            .is_none_or(|thread| thread.deleted_at.is_some())
        {
            return Err(failure(
                "thread_not_found",
                format!("Thread {target} is no longer available."),
            ));
        }
        Ok((parent, target_state))
    }

    pub(crate) async fn read_thread(&self, scope: Scope<'_>, input: &Value) -> Outcome {
        let input: ReadInput = decode(input)?;
        let target_id = thread_id(&input.thread_id)?;
        let view = match input.view.as_deref() {
            None | Some("messages") => "messages",
            Some("activity") => "activity",
            Some(_) => return Err(invalid("Invalid view")),
        };
        if let Some(limit) = input.limit {
            bounded("limit", limit, 1, Some(100))?;
        }
        if let Some(limit) = input.run_limit {
            bounded("runLimit", limit, 1, Some(50))?;
        }
        if let Some(limit) = input.max_chars_per_item {
            bounded("maxCharsPerItem", limit, 1, Some(50_000))?;
        }
        let limit = input
            .limit
            .map_or(DEFAULT_THREAD_READ_LIMIT, |l| l as usize);
        let run_limit = input
            .run_limit
            .map_or(DEFAULT_THREAD_RUN_LIMIT, |l| l as usize);
        let max = input
            .max_chars_per_item
            .map_or(DEFAULT_THREAD_ITEM_MAX_CHARS, |l| l as usize);
        let (parent, target) = self.load_readable(scope, &target_id).await?;
        let rows = timeline_rows(&target);
        let matching: Vec<&HistoryRow> = rows
            .iter()
            .filter(|row| match &input.item_id {
                Some(item) => row.item.id.as_str() == item,
                None => {
                    input
                        .after_position
                        .is_none_or(|after| row.position as u64 > after)
                        && (view == "activity"
                            || matches!(
                                item_type(row),
                                "user_message" | "assistant_message" | "proposed_plan"
                            ))
                }
            })
            .collect();
        let page = &matching[..matching.len().min(limit)];
        let offset = if input.item_id.is_some() {
            input.text_offset.unwrap_or(0) as usize
        } else {
            0
        };
        let target_thread = target.thread.as_ref().expect("loaded");
        let parent_thread = parent.thread.as_ref().expect("loaded");
        let task = (target_thread.parent.as_ref() == Some(&parent_thread.id)
            && target_thread.fork_boundary.is_none())
        .then(|| {
            parent
                .tasks
                .iter()
                .find(|task| task.app_owned() && task.child_thread == target_thread.id)
        })
        .flatten();
        if let Some(task) = task
            && input.text_offset.unwrap_or(0) == 0
            && self.page_has_terminal_result(&parent, &target, task, page, max)
        {
            self.read_task(scope, &task.id, false, true).await?;
        }
        let mut runs: Vec<&Run> = target.runs.iter().collect();
        runs.sort_by_key(|run| std::cmp::Reverse(run.ordinal));
        Ok(json!({
            "thread": thread_detail(&target, rows.len()),
            "recentRuns": runs.into_iter().take(run_limit).map(thread_run).collect::<Vec<_>>(),
            "items": page.iter().map(|row| timeline_item(&target, row, max, offset)).collect::<Vec<_>>(),
            "nextPosition": page.last().map(|row| row.position),
            "hasMore": matching.len() > page.len(),
        }))
    }

    fn page_has_terminal_result(
        &self,
        parent: &State,
        target: &State,
        task: &agent_domain::Task,
        page: &[&HistoryRow],
        max: usize,
    ) -> bool {
        let target_id = &target.thread.as_ref().expect("loaded").id;
        let Some(transfer) = parent
            .transfers
            .iter()
            .find(|t| t.kind == TransferKind::SubagentResult && &t.source == target_id)
        else {
            return false;
        };
        let run = target
            .runs
            .iter()
            .find(|run| run.ordinal == transfer.boundary)
            .or_else(|| {
                target
                    .runs
                    .iter()
                    .find(|run| Some(&run.message) == task.original_message.as_ref())
            });
        let Some(run) = run.filter(|run| {
            matches!(
                run.status,
                RunStatus::Completed
                    | RunStatus::Failed
                    | RunStatus::Cancelled
                    | RunStatus::Interrupted
            )
        }) else {
            return false;
        };
        let (message, item) = result_location(target, run);
        if message.is_none() && item.is_none() {
            return false;
        }
        page.iter().any(|row| {
            if &row.source != target_id || row.inherited {
                return false;
            }
            let matches = item.as_ref() == Some(&row.item.id)
                || matches!(&row.item.kind, ItemKind::AssistantMessage { message: id } if Some(id) == message.as_ref());
            matches
                && item_text(target, row)
                    .0
                    .is_some_and(|text| text.encode_utf16().count() <= max)
        })
    }
}
