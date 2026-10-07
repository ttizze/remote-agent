//! Timeline entries in the committed item order: messages, proposed plans,
//! work-log entries and lifecycle events, plus messages this device sent that
//! the thread has not folded yet.
use crate::commands::outbox::PendingMessage;
use crate::view::timeline::lifecycle::{HandoffDivider, handoff_dividers};
use crate::view::work_log::{
    ItemType, QuestionAnswer, SourceActivity, ToolLifecycleStatus, WorkLogEntry, WorkTone,
    presentation::context_compaction_label,
    tool_activity::{
        ToolActivityAction, classify_tool_activity, collect_tool_file_paths,
        format_read_tool_label, format_search_tool_label,
    },
    tool_presentation::extract_tool_presentation,
    turn_item::turn_item_is_workspace_preparation,
};
use agent_domain::{
    Attachment, AttemptStatus, Checkpoint, CheckpointStatus, InputIntent, Item, ItemKind,
    ItemStatus, Json, MessageAuthor, MessageContext, MessageId, PlanId, RequestBody, Role,
    RunAttemptId, RunId, State, Timestamp,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: MessageId,
    pub role: Role,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
    pub run: Option<RunId>,
    pub streaming: bool,
    pub created_by: Option<MessageAuthor>,
    pub creation_source: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub input_intent: Option<InputIntent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum PlanStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposedPlan {
    pub id: PlanId,
    pub run: Option<RunId>,
    pub markdown: String,
    pub status: PlanStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The attempt an entry's item belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineAttempt {
    pub id: RunAttemptId,
    pub run: RunId,
    pub ordinal: u64,
    pub status: AttemptStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineEntryKind {
    Message {
        message: ChatMessage,
        item: Option<Arc<Item>>,
    },
    ProposedPlan(ProposedPlan),
    Work(Box<WorkLogEntry>),
    /// A lifecycle item drawn on its own: interrupt, fork, subagent.
    Event(Arc<Item>),
    /// A context handoff drawn before the run that received it.
    Handoff(Box<HandoffDivider>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub id: String,
    pub created_at: Timestamp,
    pub attempt: Option<TimelineAttempt>,
    pub kind: TimelineEntryKind,
}

impl TimelineEntry {
    pub fn message(&self) -> Option<&ChatMessage> {
        match &self.kind {
            TimelineEntryKind::Message { message, .. } => Some(message),
            _ => None,
        }
    }
    pub fn work(&self) -> Option<&WorkLogEntry> {
        match &self.kind {
            TimelineEntryKind::Work(entry) => Some(entry),
            _ => None,
        }
    }
    /// The item an event or work entry presents.
    pub fn item(&self) -> Option<&Arc<Item>> {
        match &self.kind {
            TimelineEntryKind::Message { item, .. } => item.as_ref(),
            TimelineEntryKind::Work(entry) => entry.item.as_ref(),
            TimelineEntryKind::Event(item) => Some(item),
            TimelineEntryKind::ProposedPlan(_) | TimelineEntryKind::Handoff(_) => None,
        }
    }
}

/// Inputs of the committed timeline besides the folded state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntriesInput<'a> {
    /// Messages this device sent that the thread has not folded, in send order.
    pub pending: &'a [PendingMessage],
}

fn item_title(item: &Item) -> Option<String> {
    let title = match &item.kind {
        ItemKind::CommandExecution { title, .. } => title.as_deref(),
        ItemKind::DynamicTool { presentation, .. } => presentation.title.as_deref(),
        _ => None,
    }?;
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn updated_at(item: &Item) -> Timestamp {
    item.completed_at
        .clone()
        .unwrap_or_else(|| item.started_at.clone())
}

fn attempt_of(state: &State, item: &Item) -> Option<TimelineAttempt> {
    let id = item.attempt.as_ref()?;
    let run = item.run.as_ref()?;
    let attempt = state
        .attempts
        .iter()
        .find(|attempt| &attempt.id == id && &attempt.run == run)?;
    Some(TimelineAttempt {
        id: attempt.id.clone(),
        run: attempt.run.clone(),
        ordinal: attempt.ordinal,
        status: attempt.status,
    })
}

/// Paths a file change touched, from the provider's change list or tool input.
pub fn file_change_paths(changes: &Json) -> Vec<String> {
    let path = |value: &serde_json::Value| {
        ["path", "file_path", "notebook_path", "filePath"]
            .into_iter()
            .find_map(|key| value.get(key)?.as_str())
            .map(str::to_string)
    };
    match &changes.0 {
        serde_json::Value::Array(changes) => changes.iter().filter_map(path).collect(),
        value => path(value).into_iter().collect(),
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProviderErrorPresentation {
    pub label: String,
    pub detail: String,
}

pub fn provider_error_presentation(item: &Item) -> Option<ProviderErrorPresentation> {
    let ItemKind::Error {
        message,
        retry,
        class,
        ..
    } = &item.kind
    else {
        return None;
    };
    let usage_limit = class.as_deref() == Some("usage_limit");
    let Some(retry) = retry else {
        return Some(ProviderErrorPresentation {
            label: if usage_limit {
                "Usage limit reached".into()
            } else {
                "Provider error".into()
            },
            detail: message.clone(),
        });
    };
    let progress = match retry.max_attempts {
        None => retry.attempt.to_string(),
        Some(max) => format!("{}/{max}", retry.attempt),
    };
    let label = match item.status {
        ItemStatus::Running => format!("Retrying provider ({progress})"),
        ItemStatus::Completed => format!("Provider recovered ({progress} retries)"),
        ItemStatus::Failed => format!(
            "{} after {progress} retries",
            if usage_limit {
                "Usage limit reached"
            } else {
                "Provider error"
            }
        ),
        _ => format!("Provider retry stopped ({progress})"),
    };
    let delay = match retry.delay_ms {
        Some(delay) if item.status == ItemStatus::Running && delay > 0 => {
            if delay < 1_000 {
                format!(" Retrying in {delay}ms.")
            } else {
                let seconds = format!("{:.1}", delay as f64 / 1_000.0);
                format!(
                    " Retrying in {}s.",
                    seconds.strip_suffix(".0").unwrap_or(&seconds)
                )
            }
        }
        _ => String::new(),
    };
    Some(ProviderErrorPresentation {
        label,
        detail: format!("{message}{delay}"),
    })
}

fn plan_status(state: &State, plan: &PlanId) -> PlanStatus {
    match state.plans.iter().find(|candidate| &candidate.id == plan) {
        Some(plan) if plan.implemented_by.is_some() => PlanStatus::Completed,
        _ => PlanStatus::Active,
    }
}

/// The answers a question request received, in question order.
pub fn question_answer(state: &State, item: &Item) -> Option<QuestionAnswer> {
    let ItemKind::UserInputRequest { request } = &item.kind else {
        return None;
    };
    let request = state.requests.iter().find(|r| &r.id == request)?;
    let answers = request.answers.as_ref()?;
    let RequestBody::Questions { questions } = &request.body else {
        return None;
    };
    let position = |id: &str| questions.iter().position(|q| q.id == id);
    let mut answers: Vec<_> = answers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    answers.sort_by_key(|(id, _)| position(id));
    let mut attachments: Vec<_> = request
        .attachments
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    attachments.sort_by_key(|(id, _)| position(id));
    Some(QuestionAnswer {
        request: request.id.clone(),
        answers,
        attachments,
        question_text: questions
            .iter()
            .map(|q| (q.id.clone(), q.question.clone()))
            .collect(),
    })
}

fn work_tone(kind: &ItemKind) -> WorkTone {
    match kind {
        ItemKind::Reasoning => WorkTone::Thinking,
        ItemKind::CommandExecution { .. }
        | ItemKind::FileChange { .. }
        | ItemKind::WebSearch { .. }
        | ItemKind::DynamicTool { .. }
        | ItemKind::Subagent { .. }
        | ItemKind::ThreadCreated { .. }
        | ItemKind::UserInputRequest { .. }
        | ItemKind::ApprovalRequest { .. } => WorkTone::Tool,
        _ => WorkTone::Info,
    }
}

/// The work-log entry of an item drawn in the work log.
pub fn work_entry(state: &State, item: &Arc<Item>) -> WorkLogEntry {
    let title = item_title(item);
    let item_type = ItemType::of(&item.kind);
    let presentation = extract_tool_presentation(item);
    let mut entry = WorkLogEntry {
        run: item.run.clone(),
        item_type: Some(item_type),
        tool_lifecycle_status: Some(ToolLifecycleStatus::from(item.status)),
        item: Some(item.clone()),
        viewed_image_path: presentation.viewed_image_path,
        tool_surface: presentation.tool_surface,
        tool_icon: presentation.tool_icon,
        tool_source: presentation.tool_source,
        ..WorkLogEntry::new(
            item.id.to_string(),
            item.started_at.clone(),
            "",
            work_tone(&item.kind),
        )
    };
    let text = (!item.text.is_empty()).then(|| item.text.clone());
    match &item.kind {
        ItemKind::ThreadCreated { .. } => entry.label = "Created thread".into(),
        ItemKind::Compaction { .. } => {
            entry.label = context_compaction_label(item);
            entry.source_activity = Some(SourceActivity::ContextCompaction);
            entry.detail = text;
        }
        ItemKind::Reasoning => {
            entry.label = title.unwrap_or_else(|| "Thinking".into());
            entry.detail = text;
        }
        ItemKind::CommandExecution { command, .. } => {
            entry.label = title.clone().unwrap_or_else(|| "Ran command".into());
            entry.command = Some(command.clone());
            entry.raw_command = Some(command.clone());
            entry.tool_title = Some(title.unwrap_or_else(|| "Command".into()));
        }
        ItemKind::FileChange { changes } => {
            let paths = file_change_paths(changes);
            entry.label = title.clone().unwrap_or_else(|| match paths.as_slice() {
                [_, _, ..] => format!("Changed {} files", paths.len()),
                [path] => format!("Changed {path}"),
                [] => "Changed files".into(),
            });
            entry.changed_files = Some(paths);
            entry.tool_title = Some(title.unwrap_or_else(|| "File change".into()));
        }
        ItemKind::WebSearch { query, .. } => {
            entry.label = title.clone().unwrap_or_else(|| "Searched the web".into());
            entry.detail = (!query.is_empty()).then(|| query.clone());
            entry.tool_title = Some(title.unwrap_or_else(|| "Web search".into()));
        }
        ItemKind::SystemNotice { message } => {
            entry.label = message.clone();
            entry.source_activity = Some(SourceActivity::RuntimeWarning);
        }
        ItemKind::Error { retry, class, .. } => {
            let presentation = provider_error_presentation(item).expect("error item");
            entry.label = presentation.label;
            entry.detail = Some(presentation.detail);
            entry.source_activity = if class.as_deref() == Some("usage_limit")
                && item.status != ItemStatus::Completed
            {
                Some(SourceActivity::RuntimeWarning)
            } else if retry.is_none() {
                Some(SourceActivity::RuntimeError)
            } else {
                None
            };
        }
        ItemKind::DynamicTool { name, input, .. } => {
            let data = serde_json::json!({ "toolName": name, "input": input.0 });
            let classified = classify_tool_activity(Some(item_type), None, Some(&data));
            let input_data = serde_json::json!({ "input": input.0 });
            let read_path = collect_tool_file_paths(&input_data).into_iter().next();
            entry.label = title.clone().unwrap_or_else(|| match classified {
                ToolActivityAction::Read => {
                    format_read_tool_label(read_path.as_deref().unwrap_or(""), 0)
                }
                ToolActivityAction::Search => {
                    format_search_tool_label(Some(&input_data)).unwrap_or_else(|| name.clone())
                }
                _ => name.clone(),
            });
            entry.tool_title = Some(title.unwrap_or_else(|| name.clone()));
        }
        ItemKind::ApprovalRequest { request } => {
            let approval = state
                .requests
                .iter()
                .find(|r| &r.id == request)
                .and_then(|request| match &request.body {
                    RequestBody::Approval {
                        kind,
                        title,
                        detail,
                        ..
                    } => Some((kind, title, detail)),
                    RequestBody::Questions { .. } => None,
                });
            entry.label = approval
                .map(|(_, title, _)| title.trim())
                .filter(|title| !title.is_empty())
                .map_or_else(|| "Approval requested".into(), str::to_string);
            entry.detail =
                approval.map(|(kind, _, detail)| detail.clone().unwrap_or_else(|| kind.clone()));
            entry.request_kind = approval.map(|(kind, _, _)| kind.clone());
        }
        ItemKind::UserInputRequest { .. } => {
            entry.question_answer = question_answer(state, item);
            entry.label = if entry.question_answer.is_some() {
                "Answered questions".into()
            } else {
                "Input requested".into()
            };
        }
        _ => entry.label = item_type.as_str().replace('_', " "),
    }
    entry
}

fn is_standalone(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Fork { .. }
            | ItemKind::RunInterruptRequest
            | ItemKind::RunInterruptResult { .. }
            | ItemKind::Subagent { .. }
    )
}

/// Lifecycle items that stay visible when the work around them folds.
pub fn is_persistent_resource_card(entry: &TimelineEntry) -> bool {
    entry.item().is_some_and(|item| {
        matches!(
            item.kind,
            ItemKind::Fork { .. } | ItemKind::ThreadCreated { .. }
        ) && matches!(entry.kind, TimelineEntryKind::Event(_))
    })
}

fn chat_message(state: &State, item: &Item, message: &MessageId, user: bool) -> ChatMessage {
    let stored = state.message(message);
    let user_stored = stored.filter(|_| user);
    ChatMessage {
        id: message.clone(),
        role: if user { Role::User } else { Role::Assistant },
        text: stored.map_or_else(|| item.text.clone(), |m| m.text.clone()),
        attachments: stored.map(|m| m.attachments.clone()).unwrap_or_default(),
        context: user_stored.and_then(|m| m.context.clone()),
        run: item.run.clone(),
        streaming: !user && stored.is_some_and(|m| m.streaming),
        created_by: user_stored.map(|m| m.created_by),
        creation_source: user_stored.map(|m| m.creation_source.clone()),
        created_at: item.started_at.clone(),
        updated_at: stored.map_or_else(|| updated_at(item), |m| m.updated_at.clone()),
        input_intent: user_stored.map(|m| m.intent),
    }
}

/// The timeline in the committed item order, followed by the messages the
/// device sent and the thread has not folded, except queued ones.
pub fn derive_timeline_entries(state: &State, input: &EntriesInput<'_>) -> Vec<TimelineEntry> {
    let items = state.activity_items();
    let folded_answers: BTreeSet<String> = items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::UserInputRequest { request } if question_answer(state, item).is_some() => {
                Some(format!("async-answer:{request}"))
            }
            _ => None,
        })
        .collect();
    let mut retained = folded_answers.clone();
    let mut entries = vec![];
    for item in items {
        if turn_item_is_workspace_preparation(&item) {
            continue;
        }
        // Task progress belongs in the composer, not between conversation entries.
        if matches!(item.kind, ItemKind::TodoList { .. }) {
            continue;
        }
        if let ItemKind::UserMessage { message } = &item.kind
            && folded_answers.contains(message.as_str())
        {
            continue;
        }
        let item = Arc::new(item.into_owned());
        let created_at = item.started_at.clone();
        let attempt = attempt_of(state, &item);
        let (id, kind) = match &item.kind {
            ItemKind::Notification { notification } => {
                let mut entry = WorkLogEntry::new(
                    item.id.to_string(),
                    created_at.clone(),
                    notification.summary.clone(),
                    WorkTone::Info,
                );
                entry.run = item.run.clone();
                entry.item_type = Some(ItemType::Notification);
                entry.item = Some(item.clone());
                (
                    item.id.to_string(),
                    TimelineEntryKind::Work(Box::new(entry)),
                )
            }
            ItemKind::UserMessage { message } | ItemKind::AssistantMessage { message } => {
                let user = matches!(item.kind, ItemKind::UserMessage { .. });
                let message = chat_message(state, &item, message, user);
                retained.insert(message.id.to_string());
                (
                    message.id.to_string(),
                    TimelineEntryKind::Message {
                        message,
                        item: Some(item.clone()),
                    },
                )
            }
            ItemKind::ProposedPlan { plan } => (
                item.id.to_string(),
                TimelineEntryKind::ProposedPlan(ProposedPlan {
                    id: plan.clone(),
                    run: item.run.clone(),
                    markdown: state
                        .plans
                        .iter()
                        .find(|candidate| &candidate.id == plan && !candidate.markdown.is_empty())
                        .map_or_else(|| item.text.clone(), |plan| plan.markdown.clone()),
                    status: plan_status(state, plan),
                    created_at: created_at.clone(),
                    updated_at: updated_at(&item),
                }),
            ),
            _ if is_standalone(&item) => {
                (item.id.to_string(), TimelineEntryKind::Event(item.clone()))
            }
            _ => (
                item.id.to_string(),
                TimelineEntryKind::Work(Box::new(work_entry(state, &item))),
            ),
        };
        entries.push(TimelineEntry {
            id,
            created_at,
            attempt,
            kind,
        });
    }
    for divider in handoff_dividers(state) {
        let entry_run = |entry: &TimelineEntry| match &entry.kind {
            TimelineEntryKind::Message { message, .. } => message.run.clone(),
            TimelineEntryKind::ProposedPlan(plan) => plan.run.clone(),
            TimelineEntryKind::Work(work) => work.run.clone(),
            TimelineEntryKind::Event(item) => item.run.clone(),
            TimelineEntryKind::Handoff(_) => None,
        };
        let Some(index) = entries
            .iter()
            .position(|entry| entry_run(entry).as_ref() == Some(&divider.run))
        else {
            continue;
        };
        let created_at = entries[index].created_at.clone();
        entries.insert(
            index,
            TimelineEntry {
                id: format!("handoff:{}", divider.run),
                created_at,
                attempt: None,
                kind: TimelineEntryKind::Handoff(Box::new(divider)),
            },
        );
    }
    for pending in input.pending {
        if pending.queued || !retained.insert(pending.id.to_string()) {
            continue;
        }
        entries.push(TimelineEntry {
            id: pending.id.to_string(),
            created_at: pending.created_at.clone(),
            attempt: None,
            kind: TimelineEntryKind::Message {
                message: ChatMessage {
                    id: pending.id.clone(),
                    role: Role::User,
                    text: pending.text.clone(),
                    attachments: pending.attachments.clone(),
                    context: pending.context.clone(),
                    run: None,
                    streaming: false,
                    created_by: None,
                    creation_source: None,
                    created_at: pending.created_at.clone(),
                    updated_at: pending.created_at.clone(),
                    input_intent: None,
                },
                item: None,
            },
        });
    }
    entries
}

/// For each turn-starting user message whose run has a ready checkpoint, the
/// run ordinal its "Edit from here" rolls back to.
pub fn revert_turn_counts(
    entries: &[TimelineEntry],
    checkpoints: &[Checkpoint],
) -> BTreeMap<MessageId, u64> {
    let ready: BTreeMap<&RunId, &Checkpoint> = checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.status == CheckpointStatus::Ready)
        .filter_map(|checkpoint| Some((checkpoint.run.as_ref()?, checkpoint)))
        .collect();
    entries
        .iter()
        .filter_map(|entry| {
            let message = entry.message()?;
            if message.role != Role::User
                || !matches!(
                    message.input_intent,
                    Some(InputIntent::TurnStart | InputIntent::QueuedTurn)
                )
            {
                return None;
            }
            let checkpoint = ready.get(message.run.as_ref()?)?;
            Some((message.id.clone(), checkpoint.run_ordinal.saturating_sub(1)))
        })
        .collect()
}

#[cfg(test)]
#[path = "entries_tests.rs"]
mod tests;
