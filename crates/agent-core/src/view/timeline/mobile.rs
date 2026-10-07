//! The mobile thread feed: the committed item order as message rows and
//! grouped activities, before folds, tool toggles and the live slot are
//! presented (see `mobile_presentation`).
use crate::commands::outbox::PendingMessage;
use crate::js_text::{collapse_js_spaces, js_trim};
use crate::view::agents::format_subagent_display_title;
use crate::view::timeline::entries::{
    ChatMessage, EntriesInput, TimelineEntryKind, derive_timeline_entries, file_change_paths,
    question_answer,
};
use crate::view::timeline::lifecycle::HandoffDivider;
use crate::view::work_log::command_label::command_display_text;
use crate::view::work_log::item_detail::{turn_item_has_detail, turn_item_needs_detail_fetch};
use crate::view::work_log::presentation::{
    ToolGroupAction, ToolGroupSummaryKind, context_compaction_label,
    resolve_work_entry_tool_presentation, tool_group_action, tool_item_for_display,
    work_entry_display_indicates_tool_failure,
};
use crate::view::work_log::tool_activity::{
    ToolActivityAction, classify_tool_activity, collect_tool_file_paths, dynamic_tool_title,
    format_read_tool_label, format_search_tool_label,
};
use crate::view::work_log::tool_catalog::{
    ToolCatalogPresentation, ToolLogo, ToolSummaryAction, resolve_tool_definition,
    resolve_tool_presentation,
};
use crate::view::work_log::tool_output::compact_dynamic_tool_output;
use crate::view::work_log::tool_presentation::extract_tool_presentation;
use crate::view::work_log::{
    ItemType, ToolIcon, ToolLifecycleStatus, ToolSurface, WorkLogEntry, WorkTone,
};
use agent_domain::{
    BackgroundKind, Item, ItemKind, ItemStatus, NotificationOutcome, NotificationSource, Request,
    RequestBody, RunAttemptId, RunId, RunStatus, State, Task, Timestamp,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub use super::mobile_presentation::{
    LIVE_ACTIVITY_ROW_ID, derive_thread_feed_presentation, failed_feed_run_ids,
    is_context_compaction_activity_group, thread_feed_activity_is_visible,
    thread_feed_run_is_unsettled,
};
pub use super::work_row::WorkIcon;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeedStatus {
    Success,
    Failure,
    Neutral,
}

/// Whether an item belongs to this thread or was inherited from its fork source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeedVisibility {
    Local,
    Inherited,
}

impl FeedVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Inherited => "inherited",
        }
    }
}

/// One work item of the feed with everything its row and copy action show.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedActivity {
    pub id: String,
    pub created_at: Timestamp,
    pub run: Option<RunId>,
    pub attempt: Option<RunAttemptId>,
    pub summary: String,
    pub detail: Option<String>,
    pub can_expand: bool,
    /// Expanding fetches the withheld input and output with `LoadItemDetail`.
    pub fetches_detail: bool,
    pub full_detail: Option<String>,
    pub copy_text: String,
    pub icon: WorkIcon,
    pub logo: Option<ToolLogo>,
    pub tool_like: bool,
    pub prominent: bool,
    pub status: Option<FeedStatus>,
    pub lifecycle_status: ToolLifecycleStatus,
    pub work_entry: WorkLogEntry,
    /// A member of an expanded tool group's detail list.
    pub grouped_tool_detail: bool,
    pub live: bool,
    pub visibility: FeedVisibility,
    pub item: Arc<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityGroup {
    pub id: String,
    pub created_at: Timestamp,
    pub run: Option<RunId>,
    pub activities: Vec<FeedActivity>,
    /// The next row continues the same work log.
    pub continues_work_log: bool,
}

/// The header of a collapsible run of tool calls.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkToggle {
    pub id: String,
    pub created_at: Timestamp,
    pub run: Option<RunId>,
    pub group_id: String,
    pub hidden_count: usize,
    pub expanded: bool,
    pub summary: String,
    pub summary_kind: ToolGroupSummaryKind,
    pub tool_surface: Option<ToolSurface>,
    pub tool_icon: Option<ToolIcon>,
    pub summary_tool_icon: Option<ToolLogo>,
    pub has_failure: bool,
    pub live: bool,
    pub shimmer: bool,
    pub continues_work_log: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FeedRow {
    Message {
        id: String,
        created_at: Timestamp,
        message: ChatMessage,
        item: Option<Arc<Item>>,
    },
    ActivityGroup(ActivityGroup),
    WorkToggle(WorkToggle),
    /// Folds a settled run's work behind "Worked for …".
    RunFold {
        id: String,
        created_at: Timestamp,
        run: RunId,
        label: String,
        expanded: bool,
    },
    /// The live slot while no tool row carries it.
    Thinking {
        id: String,
        created_at: Timestamp,
        run: Option<RunId>,
        continues_work_log: bool,
    },
    /// A context handoff drawn before the run that received it.
    Handoff {
        id: String,
        created_at: Timestamp,
        divider: HandoffDivider,
    },
    /// A message the device sent that the thread has not folded yet.
    PendingMessage(PendingMessage),
}

impl FeedRow {
    pub fn id(&self) -> &str {
        match self {
            Self::Message { id, .. }
            | Self::RunFold { id, .. }
            | Self::Thinking { id, .. }
            | Self::Handoff { id, .. } => id,
            Self::ActivityGroup(group) => &group.id,
            Self::WorkToggle(toggle) => &toggle.id,
            Self::PendingMessage(message) => message.id.as_str(),
        }
    }

    pub fn created_at(&self) -> &Timestamp {
        match self {
            Self::Message { created_at, .. }
            | Self::RunFold { created_at, .. }
            | Self::Thinking { created_at, .. }
            | Self::Handoff { created_at, .. } => created_at,
            Self::ActivityGroup(group) => &group.created_at,
            Self::WorkToggle(toggle) => &toggle.created_at,
            Self::PendingMessage(message) => &message.created_at,
        }
    }

    pub fn activities(&self) -> &[FeedActivity] {
        match self {
            Self::ActivityGroup(group) => &group.activities,
            _ => &[],
        }
    }

    pub fn continues_work_log(&self) -> bool {
        match self {
            Self::ActivityGroup(group) => group.continues_work_log,
            Self::WorkToggle(toggle) => toggle.continues_work_log,
            Self::Thinking {
                continues_work_log, ..
            } => *continues_work_log,
            _ => false,
        }
    }
}

/// The run the feed treats as latest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLatestRun {
    pub run: RunId,
    pub status: RunStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

impl From<&agent_domain::Run> for FeedLatestRun {
    fn from(run: &agent_domain::Run) -> Self {
        Self {
            run: run.id.clone(),
            status: run.status,
            started_at: run.started_at.clone(),
            completed_at: run.completed_at.clone(),
        }
    }
}

/// What the feed presents besides the thread state: the client's disclosure
/// state, the live work and the outbox.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedInput {
    pub latest_run: Option<FeedLatestRun>,
    pub expanded_runs: BTreeSet<RunId>,
    pub expanded_work_groups: BTreeSet<String>,
    /// When the working indicator counts from; `None` while nothing works.
    pub active_work_started_at: Option<Timestamp>,
    /// The live work is a provider-native subagent's runless root turn.
    pub runless_work_active: bool,
    /// From `Outbox::undelivered_messages`.
    pub pending: Vec<PendingMessage>,
}

/// The tool input and output a dynamic tool entry carries.
fn entry_tool_data(entry: &WorkLogEntry) -> Option<Value> {
    match &entry.item.as_ref()?.kind {
        ItemKind::DynamicTool { input, output, .. } => Some(json!({
            "input": input.0,
            "output": output.as_ref().map(|output| output.0.clone()),
        })),
        _ => None,
    }
}

/// Expanded work rows keep their detail while compact rows show a stable one-line label.
pub fn work_entry_row_label(entry: &WorkLogEntry, expanded: bool) -> String {
    if expanded && entry.item_type == Some(ItemType::Reasoning) {
        return if entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress) {
            "Thinking".into()
        } else {
            "Thought".into()
        };
    }
    if let Some(presentation) = resolve_work_entry_tool_presentation(entry, None) {
        return presentation.display_name;
    }
    if let Some(command) = entry
        .command
        .as_deref()
        .filter(|command| !js_trim(command).is_empty())
    {
        return collapse_js_spaces(&command_display_text(command));
    }
    let action = tool_group_action(entry);
    let searching = matches!(
        action,
        ToolGroupAction::CodeSearch | ToolGroupAction::Search
    );
    let is_tool_read =
        action == ToolGroupAction::Read && entry.item_type == Some(ItemType::DynamicTool);
    let tool_data = entry_tool_data(entry);
    if searching
        && let Some(label) =
            format_search_tool_label(tool_data.as_ref().filter(|data| data.is_object()))
    {
        return label;
    }
    if is_tool_read {
        let first_path = match &entry.changed_files {
            Some(files) => files.first().cloned(),
            None => tool_data
                .as_ref()
                .and_then(|data| collect_tool_file_paths(data).into_iter().next()),
        };
        if let Some(path) = first_path.filter(|path| !path.is_empty()) {
            let extra = entry.changed_files.as_ref().map_or(1, Vec::len);
            return format_read_tool_label(&path, extra.saturating_sub(1));
        }
        if !expanded {
            return "Read file".into();
        }
    }
    let changed = entry
        .changed_files
        .as_ref()
        .filter(|files| !files.is_empty())
        .map(|files| match files.as_slice() {
            [only] => only.clone(),
            [first, ..] => format!("{first} +{} more", files.len() - 1),
            [] => unreachable!("files is not empty"),
        });
    let preview = entry
        .command
        .clone()
        .or_else(|| {
            if is_tool_read || (!expanded && searching) {
                None
            } else {
                entry.detail.clone()
            }
        })
        .or(changed);
    if expanded {
        return preview
            .map(|preview| js_trim(&preview).to_owned())
            .filter(|preview| !preview.is_empty())
            .unwrap_or_else(|| entry.label.clone());
    }
    match preview.filter(|preview| !preview.is_empty()) {
        Some(preview) => {
            let compacted = collapse_js_spaces(&preview);
            if compacted.is_empty() {
                entry.label.clone()
            } else {
                compacted
            }
        }
        None => entry.label.clone(),
    }
}

fn capitalize_phrase(value: &str) -> String {
    let trimmed = js_trim(value);
    let mut chars = trimmed.chars();
    match chars.next() {
        None => value.to_owned(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

fn item_request<'a>(state: &'a State, item: &Item) -> Option<&'a Request> {
    let (ItemKind::ApprovalRequest { request } | ItemKind::UserInputRequest { request }) =
        &item.kind
    else {
        return None;
    };
    state
        .requests
        .iter()
        .find(|candidate| &candidate.id == request)
}

fn approval_parts<'a>(
    state: &'a State,
    item: &Item,
) -> Option<(&'a str, &'a str, Option<&'a str>)> {
    match &item_request(state, item)?.body {
        RequestBody::Approval {
            kind,
            title,
            detail,
            ..
        } => Some((kind, title, detail.as_deref())),
        RequestBody::Questions { .. } => None,
    }
}

pub(crate) fn subagent_task<'a>(state: &'a State, item: &Item) -> Option<&'a Task> {
    let ItemKind::Subagent { task } = &item.kind else {
        return None;
    };
    state.task(task)
}

fn usage_limit(item: &Item) -> bool {
    matches!(&item.kind, ItemKind::Error { class: Some(class), .. } if class == "usage_limit")
}

pub(crate) fn is_failed_error(item: &Item) -> bool {
    matches!(item.kind, ItemKind::Error { .. }) && item.status == ItemStatus::Failed
}

/// The Host titles provider failures and retries by their outcome.
fn error_title(item: &Item) -> Option<&'static str> {
    let ItemKind::Error { retry, .. } = &item.kind else {
        return None;
    };
    let failed = if usage_limit(item) {
        "Usage limit reached"
    } else {
        "Provider error"
    };
    Some(match (retry, item.status) {
        (None, _) | (Some(_), ItemStatus::Failed) => failed,
        (Some(_), ItemStatus::Completed) => "Provider recovered",
        (Some(_), ItemStatus::Interrupted | ItemStatus::Cancelled) => "Provider retry stopped",
        (Some(_), ItemStatus::Pending | ItemStatus::Running | ItemStatus::Waiting) => {
            "Provider retry"
        }
    })
}

/// The item's own title, trimmed and non-empty.
fn item_title(state: &State, item: &Item) -> Option<String> {
    let title = match &item.kind {
        ItemKind::CommandExecution { title, .. } => title.clone(),
        ItemKind::DynamicTool { presentation, .. } => presentation.title.clone(),
        ItemKind::ApprovalRequest { .. } => {
            approval_parts(state, item).map(|(_, title, _)| title.to_owned())
        }
        ItemKind::Error { .. } => error_title(item).map(str::to_owned),
        ItemKind::Subagent { .. } => subagent_task(state, item).and_then(|task| task.title.clone()),
        ItemKind::ThreadCreated { title, .. } => Some(title.clone()),
        _ => None,
    }?;
    let title = js_trim(&title);
    (!title.is_empty()).then(|| title.to_owned())
}

fn is_tool_like(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Reasoning
            | ItemKind::CommandExecution { .. }
            | ItemKind::FileChange { .. }
            | ItemKind::WebSearch { .. }
            | ItemKind::ApprovalRequest { .. }
            | ItemKind::UserInputRequest { .. }
            | ItemKind::DynamicTool { .. }
            | ItemKind::Subagent { .. }
    )
}

fn is_prominent(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Fork { .. } | ItemKind::ThreadCreated { .. } | ItemKind::SystemNotice { .. }
    )
}

fn item_status(item: &Item) -> Option<FeedStatus> {
    match &item.kind {
        ItemKind::Notification { notification } => {
            return (notification.outcome == NotificationOutcome::Failed)
                .then_some(FeedStatus::Failure);
        }
        ItemKind::Error { .. } => {
            return Some(match item.status {
                ItemStatus::Failed if usage_limit(item) => FeedStatus::Neutral,
                ItemStatus::Failed => FeedStatus::Failure,
                ItemStatus::Completed => FeedStatus::Success,
                _ => FeedStatus::Neutral,
            });
        }
        _ => {}
    }
    if !is_tool_like(item) {
        return None;
    }
    Some(match item.status {
        ItemStatus::Failed => FeedStatus::Failure,
        ItemStatus::Completed => FeedStatus::Success,
        _ => FeedStatus::Neutral,
    })
}

fn item_tone(item: &Item) -> WorkTone {
    match item.kind {
        ItemKind::Reasoning => WorkTone::Thinking,
        ItemKind::CommandExecution { .. }
        | ItemKind::FileChange { .. }
        | ItemKind::WebSearch { .. }
        | ItemKind::DynamicTool { .. }
        | ItemKind::Subagent { .. } => WorkTone::Tool,
        _ => WorkTone::Info,
    }
}

fn dynamic_tool_action(name: &str, input: &Value) -> ToolActivityAction {
    let data = json!({ "toolName": name, "input": input });
    classify_tool_activity(Some(ItemType::DynamicTool), None, Some(&data))
}

fn item_icon(state: &State, item: &Item) -> WorkIcon {
    match &item.kind {
        ItemKind::Notification { notification } => match notification.source {
            NotificationSource::Native(BackgroundKind::Subagent)
            | NotificationSource::Delegated { .. } => WorkIcon::Hammer,
            NotificationSource::Native(BackgroundKind::Command) => WorkIcon::Command,
            NotificationSource::Native(BackgroundKind::Monitor) => WorkIcon::Eye,
            NotificationSource::Native(BackgroundKind::BackgroundTask) => WorkIcon::Zap,
        },
        ItemKind::DynamicTool { name, input, .. } => match dynamic_tool_action(name, &input.0) {
            ToolActivityAction::Read => WorkIcon::Eye,
            ToolActivityAction::Search => WorkIcon::Search,
            _ => WorkIcon::Wrench,
        },
        ItemKind::Reasoning => WorkIcon::Agent,
        ItemKind::CommandExecution { .. } => WorkIcon::Command,
        ItemKind::FileChange { .. } => WorkIcon::Edit,
        ItemKind::WebSearch { .. } => WorkIcon::Globe,
        ItemKind::ApprovalRequest { .. } => match approval_parts(state, item) {
            Some(("permission", _, _)) => WorkIcon::Lock,
            _ => WorkIcon::Message,
        },
        ItemKind::UserInputRequest { .. }
        | ItemKind::UserMessage { .. }
        | ItemKind::AssistantMessage { .. } => WorkIcon::Message,
        ItemKind::Subagent { .. } => WorkIcon::Hammer,
        ItemKind::RunInterruptRequest
        | ItemKind::RunInterruptResult { .. }
        | ItemKind::SystemNotice { .. } => WorkIcon::Warning,
        ItemKind::Error { .. } if usage_limit(item) => {
            if item.status == ItemStatus::Completed {
                WorkIcon::Check
            } else {
                WorkIcon::Warning
            }
        }
        ItemKind::Error { .. } => WorkIcon::Alert,
        ItemKind::ProposedPlan { .. } | ItemKind::TodoList { .. } => WorkIcon::Check,
        ItemKind::Compaction { .. } | ItemKind::Fork { .. } | ItemKind::ThreadCreated { .. } => {
            WorkIcon::Zap
        }
    }
}

fn item_tool_presentation(item: &Item) -> Option<ToolCatalogPresentation> {
    let ItemKind::DynamicTool {
        name, presentation, ..
    } = &item.kind
    else {
        return None;
    };
    resolve_tool_presentation(Some(name))
        .or_else(|| resolve_tool_presentation(presentation.title.as_deref()))
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn item_summary(
    state: &State,
    item: &Item,
    tool_presentation: Option<&ToolCatalogPresentation>,
) -> String {
    match &item.kind {
        ItemKind::Notification { notification } => return notification.summary.clone(),
        ItemKind::SystemNotice { message } => return message.clone(),
        ItemKind::Compaction { .. } => return context_compaction_label(item),
        _ => {}
    }
    let title = match &item.kind {
        ItemKind::DynamicTool { name, input, .. } => dynamic_tool_title(Some(name), &input.0),
        _ => None,
    }
    .or_else(|| item_title(state, item));
    if matches!(item.kind, ItemKind::Subagent { .. }) {
        return format_subagent_display_title(title.as_deref().unwrap_or("Subagent"));
    }
    if let Some(title) = title {
        return tool_presentation.map_or_else(
            || capitalize_phrase(&title),
            |presentation| presentation.display_name.clone(),
        );
    }
    match &item.kind {
        ItemKind::Reasoning => "Thinking".into(),
        ItemKind::CommandExecution { .. } => "Command".into(),
        ItemKind::FileChange { changes } => match file_change_paths(changes).as_slice() {
            [_, _, ..] => format!("Changed {} files", file_change_paths(changes).len()),
            [path] => format!("Changed {path}"),
            [] => "Changed files".into(),
        },
        ItemKind::WebSearch { .. } => "Searched the web".into(),
        ItemKind::ApprovalRequest { .. } => "Approval requested".into(),
        ItemKind::UserInputRequest { .. } => "Input requested".into(),
        ItemKind::RunInterruptRequest => "Interrupt requested".into(),
        ItemKind::RunInterruptResult { .. } => "Run interrupted".into(),
        ItemKind::Error { .. } if usage_limit(item) => "Usage limit reached".into(),
        ItemKind::Error { .. } => "Provider error".into(),
        ItemKind::Fork { .. } => "Thread forked".into(),
        ItemKind::ThreadCreated { .. } => "Thread created".into(),
        ItemKind::DynamicTool { name, input, .. } => {
            let name = non_empty(name);
            let input_data = json!({ "input": input.0 });
            match dynamic_tool_action(name.as_deref().unwrap_or_default(), &input.0) {
                ToolActivityAction::Read => format_read_tool_label(
                    collect_tool_file_paths(&input_data)
                        .first()
                        .map_or("", String::as_str),
                    0,
                ),
                ToolActivityAction::Search => format_search_tool_label(Some(&input_data))
                    .or(name)
                    .unwrap_or_else(|| "Tool call".into()),
                _ => tool_presentation
                    .map(|presentation| presentation.display_name.clone())
                    .or(name)
                    .unwrap_or_else(|| "Tool call".into()),
            }
        }
        ItemKind::ProposedPlan { .. } => "Proposed plan".into(),
        ItemKind::TodoList { .. } => "Plan updated".into(),
        ItemKind::UserMessage { .. } => "User message".into(),
        ItemKind::AssistantMessage { .. } => "Assistant message".into(),
        ItemKind::Notification { .. }
        | ItemKind::SystemNotice { .. }
        | ItemKind::Compaction { .. }
        | ItemKind::Subagent { .. } => unreachable!("returned above"),
    }
}

fn item_preview(state: &State, item: &Item) -> Option<String> {
    let plan = |id| state.plans.iter().find(|plan| &plan.id == id);
    match &item.kind {
        ItemKind::Reasoning
        | ItemKind::RunInterruptRequest
        | ItemKind::RunInterruptResult { .. }
        | ItemKind::UserMessage { .. }
        | ItemKind::AssistantMessage { .. }
        | ItemKind::Compaction { .. } => non_empty(&item.text),
        ItemKind::CommandExecution { command, .. } => non_empty(command),
        ItemKind::FileChange { changes } => file_change_paths(changes).into_iter().next(),
        ItemKind::WebSearch { query, .. } => non_empty(query),
        ItemKind::ApprovalRequest { .. } => {
            approval_parts(state, item).and_then(|(_, _, detail)| detail.map(str::to_owned))
        }
        ItemKind::UserInputRequest { .. } => match &item_request(state, item)?.body {
            RequestBody::Questions { questions } => non_empty(
                &questions
                    .iter()
                    .map(|question| question.question.as_str())
                    .collect::<Vec<_>>()
                    .join(" · "),
            ),
            RequestBody::Approval { .. } => None,
        },
        ItemKind::SystemNotice { message } => non_empty(message),
        ItemKind::Error { message, .. } => Some(message.clone()),
        ItemKind::Fork { parent, .. } => Some(parent.to_string()),
        ItemKind::ThreadCreated { thread, .. } => Some(thread.to_string()),
        ItemKind::Subagent { .. } => subagent_task(state, item).map(|task| {
            task.result
                .clone()
                .or_else(|| task.progress.clone())
                .unwrap_or_else(|| task.prompt.clone())
        }),
        ItemKind::DynamicTool { .. } => None,
        ItemKind::Notification { notification } => notification.detail.clone(),
        ItemKind::ProposedPlan { plan: id } => {
            non_empty(&plan(id).map_or_else(|| item.text.clone(), |plan| plan.markdown.clone()))
        }
        ItemKind::TodoList { plan: id } => {
            let steps = plan(id)
                .map(|plan| plan.steps.as_slice())
                .unwrap_or_default();
            Some(format!(
                "{}/{} completed",
                steps
                    .iter()
                    .filter(|step| step.status == "completed")
                    .count(),
                steps.len()
            ))
        }
    }
}

fn feed_work_entry(
    state: &State,
    item: &Arc<Item>,
    created_at: &Timestamp,
    summary: &str,
    detail: Option<&str>,
) -> WorkLogEntry {
    let title = item_title(state, item);
    let presentation = extract_tool_presentation(item);
    let mut entry = WorkLogEntry {
        run: item.run.clone(),
        item_type: Some(ItemType::of(&item.kind)),
        tool_lifecycle_status: Some(ToolLifecycleStatus::from(item.status)),
        item: Some(item.clone()),
        viewed_image_path: presentation.viewed_image_path,
        tool_surface: presentation.tool_surface,
        tool_icon: presentation.tool_icon,
        tool_source: presentation.tool_source,
        question_answer: question_answer(state, item),
        ..WorkLogEntry::new(
            item.id.to_string(),
            created_at.clone(),
            summary,
            item_tone(item),
        )
    };
    match &item.kind {
        ItemKind::Reasoning => entry.detail = non_empty(&item.text),
        ItemKind::CommandExecution { command, .. } => {
            entry.command = Some(command.clone());
            entry.raw_command = Some(command.clone());
            entry.tool_title = Some(title.unwrap_or_else(|| "Command".into()));
        }
        ItemKind::FileChange { changes } => {
            entry.changed_files = Some(file_change_paths(changes).into_iter().take(1).collect());
            entry.tool_title = Some(title.unwrap_or_else(|| "File change".into()));
        }
        ItemKind::WebSearch { query, .. } => {
            entry.detail = non_empty(query);
            entry.tool_title = Some(title.unwrap_or_else(|| "Web search".into()));
        }
        ItemKind::ApprovalRequest { .. } => {
            let approval = approval_parts(state, item);
            entry.detail = approval.and_then(|(_, _, detail)| detail.map(str::to_owned));
            entry.request_kind = approval.map(|(kind, _, _)| kind.to_owned());
        }
        ItemKind::DynamicTool { name, .. } => {
            entry.tool_title = Some(
                title
                    .or_else(|| non_empty(name))
                    .unwrap_or_else(|| "Tool".into()),
            );
        }
        _ => {
            entry.detail = detail
                .filter(|detail| !detail.is_empty())
                .map(str::to_owned)
        }
    }
    entry
}

/// Expanded detail for a row: the item without raw outputs or file bodies.
pub fn format_item_full_detail(visibility: FeedVisibility, item: &Item) -> String {
    serde_json::to_string_pretty(&json!({
        "visibility": visibility.as_str(),
        "item": tool_item_for_display(item),
    }))
    .unwrap_or_default()
}

pub(crate) fn feed_activity(
    state: &State,
    item: Arc<Item>,
    attempt: Option<RunAttemptId>,
    visibility: FeedVisibility,
) -> FeedActivity {
    let tool_presentation = item_tool_presentation(&item);
    let summary = item_summary(state, &item, tool_presentation.as_ref());
    let detail = match item.kind {
        ItemKind::Notification { .. } => None,
        _ => item_preview(state, &item),
    };
    let created_at = item.started_at.clone();
    let work_entry = feed_work_entry(state, &item, &created_at, &summary, detail.as_deref());
    let read_paths = match &item.kind {
        ItemKind::DynamicTool { input, .. }
            if tool_group_action(&work_entry) == ToolGroupAction::Read =>
        {
            Some(collect_tool_file_paths(&json!({ "input": input.0 })))
        }
        _ => None,
    };
    let full_detail = match &read_paths {
        Some(paths) => non_empty(&paths.join("\n")),
        None => Some(format_item_full_detail(visibility, &item)),
    };
    let mut copy_parts: Vec<&str> = vec![];
    for part in [
        Some(summary.as_str()),
        detail.as_deref(),
        full_detail.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !part.is_empty() && !copy_parts.contains(&part) {
            copy_parts.push(part);
        }
    }
    let copy_text = copy_parts.join("\n");
    let fetches_detail = turn_item_needs_detail_fetch(&item);
    let can_expand = !is_failed_error(&item)
        && match &read_paths {
            Some(paths) => !paths.is_empty() || fetches_detail,
            None => turn_item_has_detail(&item, state) || work_entry.question_answer.is_some(),
        };
    let icon = match work_entry.tool_surface {
        Some(ToolSurface::Browser) => WorkIcon::Browser,
        Some(ToolSurface::Computer) => WorkIcon::Computer,
        None => item_icon(state, &item),
    };
    let status = if matches!(item.kind, ItemKind::Error { .. }) && usage_limit(&item) {
        item_status(&item)
    } else if work_entry_display_indicates_tool_failure(&work_entry) {
        Some(FeedStatus::Failure)
    } else {
        item_status(&item)
    };
    FeedActivity {
        id: item.id.to_string(),
        created_at,
        run: item.run.clone(),
        attempt,
        summary,
        detail,
        can_expand,
        fetches_detail,
        full_detail,
        copy_text,
        icon,
        logo: tool_presentation.map(|presentation| presentation.logo),
        tool_like: is_tool_like(&item),
        prominent: is_prominent(&item) || is_failed_error(&item),
        status,
        lifecycle_status: ToolLifecycleStatus::from(item.status),
        work_entry,
        grouped_tool_detail: false,
        live: false,
        visibility,
        item,
    }
}

fn single_activity_group(activity: FeedActivity) -> ActivityGroup {
    ActivityGroup {
        id: activity.id.clone(),
        created_at: activity.created_at.clone(),
        run: activity.run.clone(),
        activities: vec![activity],
        continues_work_log: false,
    }
}

enum RawEntry {
    Row(Box<FeedRow>),
    Activity(Box<FeedActivity>),
}

fn is_empty_message(row: &FeedRow) -> bool {
    matches!(row, FeedRow::Message { message, .. }
        if js_trim(&message.text).is_empty() && message.attachments.is_empty())
}

/// A successful delegation is already represented by its durable child card.
/// Pending, failed and unmatched calls remain visible, even with identical prompts.
fn is_represented_delegation(
    activity: &FeedActivity,
    children_by_run: &BTreeMap<RunId, BTreeSet<String>>,
) -> bool {
    let item = &activity.item;
    let ItemKind::DynamicTool { name, output, .. } = &item.kind else {
        return false;
    };
    let Some(run) = &item.run else {
        return false;
    };
    if !matches!(item.status, ItemStatus::Running | ItemStatus::Completed)
        || resolve_tool_definition(Some(name)).map(|definition| definition.summary_action)
            != Some(ToolSummaryAction::Delegate)
        || work_entry_display_indicates_tool_failure(&activity.work_entry)
    {
        return false;
    }
    let output = compact_dynamic_tool_output(output.as_ref().map(|output| &output.0));
    output.as_ref().is_none_or(|output| !output.is_error)
        && output
            .and_then(|output| output.task_id)
            .is_some_and(|task| {
                children_by_run
                    .get(run)
                    .is_some_and(|children| children.contains(&task))
            })
}

fn group_adjacent_activities(state: &State, entries: Vec<RawEntry>) -> Vec<FeedRow> {
    let mut children_by_run: BTreeMap<RunId, BTreeSet<String>> = BTreeMap::new();
    for entry in &entries {
        let RawEntry::Activity(activity) = entry else {
            continue;
        };
        let (Some(task), Some(run)) = (subagent_task(state, &activity.item), &activity.item.run)
        else {
            continue;
        };
        if task.app_owned() {
            children_by_run
                .entry(run.clone())
                .or_default()
                .insert(task.id.to_string());
        }
    }
    let mut grouped = vec![];
    let mut open: Vec<FeedActivity> = vec![];
    let flush = |grouped: &mut Vec<FeedRow>, open: &mut Vec<FeedActivity>| {
        let Some(first) = open.first() else {
            return;
        };
        grouped.push(FeedRow::ActivityGroup(ActivityGroup {
            id: first.id.clone(),
            created_at: first.created_at.clone(),
            run: first.run.clone(),
            activities: std::mem::take(open),
            continues_work_log: false,
        }));
    };
    for entry in entries {
        let activity = match entry {
            RawEntry::Activity(activity) => {
                if is_represented_delegation(&activity, &children_by_run) {
                    continue;
                }
                *activity
            }
            // Skip empty messages so they don't break activity grouping.
            RawEntry::Row(row) if is_empty_message(&row) => continue,
            RawEntry::Row(row) => {
                flush(&mut grouped, &mut open);
                grouped.push(*row);
                continue;
            }
        };
        let standalone = matches!(
            activity.item.kind,
            ItemKind::Compaction { .. } | ItemKind::Notification { .. }
        );
        let subagent = matches!(activity.item.kind, ItemKind::Subagent { .. });
        let breaks = match open.first() {
            None => true,
            Some(first) => {
                let first_subagent = matches!(first.item.kind, ItemKind::Subagent { .. });
                subagent != first_subagent
                    || first.run != activity.run
                    || (subagent && first.item.attempt != activity.item.attempt)
                    || (!subagent && first.attempt != activity.attempt)
            }
        };
        if standalone || activity.prominent || breaks {
            flush(&mut grouped, &mut open);
        }
        let closes = standalone || activity.prominent;
        open.push(activity);
        if closes {
            flush(&mut grouped, &mut open);
        }
    }
    flush(&mut grouped, &mut open);
    grouped
}

/// Projects the committed item order into mobile rows. It preserves that
/// order and never rebuilds chat from separate message, plan or work-entry
/// collections.
pub fn build_thread_feed(state: &State) -> Vec<FeedRow> {
    let inherited: BTreeSet<&agent_domain::TurnItemId> =
        state.inherited_items.iter().map(|item| &item.id).collect();
    let visibility = |item: &Item| {
        if inherited.contains(&item.id) {
            FeedVisibility::Inherited
        } else {
            FeedVisibility::Local
        }
    };
    let mut entries = vec![];
    for entry in derive_timeline_entries(state, &EntriesInput::default()) {
        let attempt = entry.attempt.as_ref().map(|attempt| attempt.id.clone());
        let item = match entry.kind {
            TimelineEntryKind::Message { message, item } => {
                entries.push(RawEntry::Row(Box::new(FeedRow::Message {
                    id: entry.id,
                    created_at: entry.created_at,
                    message,
                    item,
                })));
                continue;
            }
            TimelineEntryKind::Handoff(divider) => {
                entries.push(RawEntry::Row(Box::new(FeedRow::Handoff {
                    id: entry.id,
                    created_at: entry.created_at,
                    divider: *divider,
                })));
                continue;
            }
            TimelineEntryKind::Work(work) => work.item,
            TimelineEntryKind::Event(item) => Some(item),
            TimelineEntryKind::ProposedPlan(_) => state
                .visible_items()
                .into_iter()
                .find(|item| item.id.as_str() == entry.id)
                .map(|item| Arc::new(item.clone())),
        };
        let Some(item) = item else {
            continue;
        };
        // Only the terminal interrupt result is useful to users; the request
        // before it is transient bookkeeping.
        if matches!(item.kind, ItemKind::RunInterruptRequest) {
            continue;
        }
        let visibility = visibility(&item);
        entries.push(RawEntry::Activity(Box::new(feed_activity(
            state, item, attempt, visibility,
        ))));
    }
    group_adjacent_activities(state, entries)
}

pub(crate) fn split_activity_group(group: &ActivityGroup) -> Vec<FeedRow> {
    if group.activities.len() == 1 {
        return vec![FeedRow::ActivityGroup(group.clone())];
    }
    group
        .activities
        .iter()
        .cloned()
        .map(|activity| FeedRow::ActivityGroup(single_activity_group(activity)))
        .collect()
}

pub(crate) fn single_row(activity: &FeedActivity) -> FeedRow {
    FeedRow::ActivityGroup(single_activity_group(activity.clone()))
}

#[cfg(test)]
#[path = "mobile_tests.rs"]
mod tests;
