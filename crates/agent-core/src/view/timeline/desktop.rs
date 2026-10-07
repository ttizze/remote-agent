//! Desktop conversation rows: settled turns fold behind "Worked for …",
//! superseded attempts fold away, adjacent work groups into toggles, and the
//! active turn keeps one live activity row.
use super::changed_files::{AssistantTurnDiff, TurnDiffSummary};
use super::desktop_folds::{
    TurnFoldInput, active_visual_response_runs, failed_runs, last_response_boundary,
    superseded_attempt_folds, terminal_assistant_message_ids, turn_folds, unsettled_run,
};
use super::desktop_labels::{
    entry_indicates_tool_success, entry_is_tool_like, single_tool_call_label,
    work_entry_is_visible_in_group,
};
use super::entries::{
    ChatMessage, ProposedPlan, TimelineEntry, TimelineEntryKind, revert_turn_counts,
};
use super::lifecycle::HandoffDivider;
use crate::view::work_log::{
    ItemType, SourceActivity, ToolIcon, ToolLifecycleStatus, ToolSurface, WorkLogEntry, WorkTone,
    presentation::{
        ToolGroupAction, ToolGroupSummaryKind, resolve_work_entry_tool_presentation,
        summarize_tool_group, tool_group_action, tool_group_summary_kind,
        work_entry_display_indicates_tool_failure,
    },
    tool_catalog::{ToolLogo, ToolSummaryAction, resolve_tool_definition},
    tool_output::compact_dynamic_tool_output,
};
use agent_domain::{
    Checkpoint, Item, ItemKind, ItemStatus, MessageId, NodeId, Role, RunAttemptId, RunId,
    RunStatus, ThreadShell, Timestamp, WorktreeSetupPhase, WorktreeSetupSnapshot,
    WorktreeSetupStageId, WorktreeSetupStageStatus,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// The thread's latest run as its list summary reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineLatestRun {
    pub run: RunId,
    pub status: RunStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

impl TimelineLatestRun {
    pub fn from_shell(shell: &ThreadShell) -> Option<Self> {
        Some(Self {
            run: shell.latest_run.clone()?,
            status: shell.status?,
            started_at: shell.latest_run_started_at.clone(),
            completed_at: shell.latest_run_completed_at.clone(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopTimelineInput {
    pub entries: Vec<TimelineEntry>,
    pub latest_run: Option<TimelineLatestRun>,
    /// The run the Host reports as running; it wins over a lagging latest run.
    pub running_run: Option<RunId>,
    pub expanded_runs: BTreeSet<RunId>,
    pub expanded_attempts: BTreeSet<RunAttemptId>,
    pub expanded_work_groups: BTreeSet<String>,
    pub is_working: bool,
    /// The live work has no run (a provider-native subagent thread), so
    /// runless entries are the current response instead of settled history.
    pub runless_work_active: bool,
    pub active_turn_started_at: Option<Timestamp>,
    pub turn_diffs: Vec<AssistantTurnDiff>,
    /// Checkpoints the "Edit from here" rollback counts turns from.
    pub checkpoints: Vec<Checkpoint>,
    pub supports_conversation_rollback: bool,
    /// Subagent tasks this app delegated (`Task::app_owned`).
    pub app_owned_tasks: BTreeSet<NodeId>,
    /// Live worktree setup; its card sits under the first user message.
    pub worktree_setup: Option<WorktreeSetupSnapshot>,
}

/// One desktop timeline row. `continues_work_log` marks a work-log row
/// followed by another, so adjacent work reads as one list.
#[derive(Debug, Clone, PartialEq)]
pub enum DesktopRow {
    WorktreeSetup {
        id: String,
        created_at: Timestamp,
        snapshot: WorktreeSetupSnapshot,
        /// The agent's turn took over; the card shows under its header.
        embedded: bool,
    },
    Work {
        id: String,
        created_at: Timestamp,
        grouped_entries: Vec<WorkLogEntry>,
        is_expanded_tool_group: bool,
        /// The heading of a lone settled call drawn without a toggle.
        display_label: Option<String>,
        continues_work_log: bool,
    },
    WorkLive {
        id: String,
        created_at: Timestamp,
        entry: Box<WorkLogEntry>,
        grouped_entries: Vec<WorkLogEntry>,
        group_id: String,
        expanded: bool,
        active: bool,
        continues_work_log: bool,
    },
    Working {
        id: String,
        created_at: Option<Timestamp>,
    },
    Thinking {
        id: String,
        created_at: Option<Timestamp>,
        /// The tool calls this row stands in for after the latest one failed.
        group_id: Option<String>,
        expanded: bool,
        continues_work_log: bool,
    },
    WorkToggle {
        id: String,
        created_at: Timestamp,
        run: Option<RunId>,
        group_id: String,
        hidden_count: usize,
        expanded: bool,
        summary: String,
        summary_kind: ToolGroupSummaryKind,
        tool_surface: Option<ToolSurface>,
        tool_icon: Option<ToolIcon>,
        summary_tool_icon: Option<ToolLogo>,
        has_failure: bool,
        continues_work_log: bool,
    },
    TurnFold {
        id: String,
        created_at: Timestamp,
        run: RunId,
        label: String,
        expanded: bool,
    },
    AttemptFold {
        id: String,
        created_at: Timestamp,
        run: RunId,
        attempt: RunAttemptId,
        label: String,
        expanded: bool,
    },
    ContextCompaction {
        id: String,
        created_at: Timestamp,
        label: String,
        active: bool,
    },
    Message {
        id: String,
        created_at: Timestamp,
        message: ChatMessage,
        item: Option<Arc<Item>>,
        duration_start: Timestamp,
        show_assistant_meta: bool,
        show_assistant_copy_button: bool,
        assistant_copy_streaming: bool,
        assistant_turn_diff: Option<TurnDiffSummary>,
        revert_turn_count: Option<u64>,
    },
    /// The footer of a response whose trailing tool calls follow its text.
    AssistantMeta {
        id: String,
        created_at: Timestamp,
        message: ChatMessage,
        item: Option<Arc<Item>>,
        show_assistant_copy_button: bool,
        assistant_copy_streaming: bool,
    },
    Event {
        id: String,
        created_at: Timestamp,
        item: Arc<Item>,
        /// Adjacent subagents of one provider turn, drawn as one card.
        subagents: Option<Vec<Arc<Item>>>,
        /// A created thread repeated below the final answer.
        resource_summary: bool,
    },
    Handoff {
        id: String,
        created_at: Timestamp,
        divider: HandoffDivider,
    },
    ProposedPlan {
        id: String,
        created_at: Timestamp,
        plan: ProposedPlan,
    },
}

impl DesktopRow {
    pub fn id(&self) -> &str {
        match self {
            Self::WorktreeSetup { id, .. }
            | Self::Work { id, .. }
            | Self::WorkLive { id, .. }
            | Self::Working { id, .. }
            | Self::Thinking { id, .. }
            | Self::WorkToggle { id, .. }
            | Self::TurnFold { id, .. }
            | Self::AttemptFold { id, .. }
            | Self::ContextCompaction { id, .. }
            | Self::Message { id, .. }
            | Self::AssistantMeta { id, .. }
            | Self::Event { id, .. }
            | Self::Handoff { id, .. }
            | Self::ProposedPlan { id, .. } => id,
        }
    }

    pub fn created_at(&self) -> Option<&Timestamp> {
        match self {
            Self::Working { created_at, .. } | Self::Thinking { created_at, .. } => {
                created_at.as_ref()
            }
            Self::WorktreeSetup { created_at, .. }
            | Self::Work { created_at, .. }
            | Self::WorkLive { created_at, .. }
            | Self::WorkToggle { created_at, .. }
            | Self::TurnFold { created_at, .. }
            | Self::AttemptFold { created_at, .. }
            | Self::ContextCompaction { created_at, .. }
            | Self::Message { created_at, .. }
            | Self::AssistantMeta { created_at, .. }
            | Self::Event { created_at, .. }
            | Self::Handoff { created_at, .. }
            | Self::ProposedPlan { created_at, .. } => Some(created_at),
        }
    }

    fn continues_work_log_mut(&mut self) -> Option<&mut bool> {
        match self {
            Self::Work {
                continues_work_log, ..
            }
            | Self::WorkLive {
                continues_work_log, ..
            }
            | Self::Thinking {
                continues_work_log, ..
            }
            | Self::WorkToggle {
                continues_work_log, ..
            } => Some(continues_work_log),
            _ => None,
        }
    }
}

const LIVE_ACTIVITY_ROW_ID: &str = "live-activity-row";
const WORKING_ROW_ID: &str = "working-indicator-row";
const WORKTREE_SETUP_ROW_ID: &str = "worktree-setup-row";

/// Where each message's elapsed time counts from: the latest user message, or
/// the previous settled assistant message after it.
pub fn compute_message_duration_start(messages: &[&ChatMessage]) -> BTreeMap<MessageId, Timestamp> {
    let mut result = BTreeMap::new();
    let mut last_boundary: Option<&Timestamp> = None;
    for message in messages {
        if message.role == Role::User {
            last_boundary = Some(&message.created_at);
        }
        result.insert(
            message.id.clone(),
            last_boundary.unwrap_or(&message.created_at).clone(),
        );
        if message.role == Role::Assistant && !message.streaming {
            last_boundary = Some(&message.updated_at);
        }
    }
    result
}

fn worktree_setup_agent_started(snapshot: &WorktreeSetupSnapshot) -> bool {
    snapshot.stages.iter().any(|stage| {
        stage.id == WorktreeSetupStageId::Agent && stage.status == WorktreeSetupStageStatus::Done
    })
}

fn work_group_id(entry_id: &str) -> String {
    format!("work-group:{entry_id}")
}

pub(super) fn is_work_where(
    entry: &TimelineEntry,
    predicate: impl Fn(&WorkLogEntry) -> bool,
) -> bool {
    entry.work().is_some_and(predicate)
}

pub(super) fn is_compaction(work: &WorkLogEntry) -> bool {
    work.source_activity == Some(SourceActivity::ContextCompaction)
}

/// Whether the entry still represents live activity, not a settled result.
fn work_entry_is_active_turn_activity(entry: &WorkLogEntry) -> bool {
    entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
        || (entry.tool_lifecycle_status.is_none()
            && (entry.source_activity == Some(SourceActivity::TaskProgress)
                || entry_is_tool_like(entry)))
}

fn expanded_work_group_row(
    group_id: &str,
    created_at: Timestamp,
    grouped_entries: Vec<WorkLogEntry>,
) -> DesktopRow {
    DesktopRow::Work {
        id: format!("{group_id}:details"),
        created_at,
        grouped_entries,
        is_expanded_tool_group: true,
        display_label: None,
        continues_work_log: false,
    }
}

/// When a settled turn ends with tool calls after its terminal text, the text
/// and tools read as one response: the message footer moves below the tools.
fn attach_trailing_tool_groups_to_assistant(rows: Vec<DesktopRow>) -> Vec<DesktopRow> {
    let mut messages_without_meta: HashSet<String> = HashSet::new();
    let mut meta_after_index: HashMap<usize, DesktopRow> = HashMap::new();
    for (message_index, row) in rows.iter().enumerate() {
        let DesktopRow::Message {
            id,
            message,
            item,
            show_assistant_meta: true,
            show_assistant_copy_button,
            assistant_copy_streaming,
            ..
        } = row
        else {
            continue;
        };
        let Some(run) = message
            .run
            .as_ref()
            .filter(|_| message.role == Role::Assistant)
        else {
            continue;
        };
        let mut last_trailing_work: Option<usize> = None;
        let mut has_trailing_tool_group = false;
        for (index, candidate) in rows.iter().enumerate().skip(message_index + 1) {
            match candidate {
                DesktopRow::Message { .. } => break,
                DesktopRow::WorkToggle {
                    run: Some(toggle_run),
                    ..
                } if toggle_run == run => {
                    has_trailing_tool_group = true;
                    last_trailing_work = Some(index);
                }
                DesktopRow::Event {
                    item,
                    resource_summary: true,
                    ..
                } if item.run.as_ref() == Some(run) => {
                    has_trailing_tool_group = true;
                    last_trailing_work = Some(index);
                }
                DesktopRow::Work {
                    grouped_entries,
                    is_expanded_tool_group,
                    ..
                } if grouped_entries
                    .iter()
                    .any(|entry| entry.run.as_ref() == Some(run)) =>
                {
                    if !is_expanded_tool_group
                        && grouped_entries.iter().any(|entry| {
                            entry_is_tool_like(entry)
                                || entry.item.as_ref().is_some_and(|item| {
                                    matches!(item.kind, ItemKind::Error { .. })
                                        && item.status == ItemStatus::Failed
                                })
                        })
                    {
                        has_trailing_tool_group = true;
                    }
                    if has_trailing_tool_group {
                        last_trailing_work = Some(index);
                    }
                }
                _ => {}
            }
        }
        let Some(last_trailing_work) = last_trailing_work else {
            continue;
        };
        messages_without_meta.insert(id.clone());
        meta_after_index.insert(
            last_trailing_work,
            DesktopRow::AssistantMeta {
                id: format!("assistant-meta:{}", message.id),
                created_at: rows[last_trailing_work]
                    .created_at()
                    .unwrap_or(&message.updated_at)
                    .clone(),
                message: message.clone(),
                item: item.clone(),
                show_assistant_copy_button: *show_assistant_copy_button,
                assistant_copy_streaming: *assistant_copy_streaming,
            },
        );
    }
    let mut result = Vec::with_capacity(rows.len() + meta_after_index.len());
    for (index, mut row) in rows.into_iter().enumerate() {
        if let DesktopRow::Message {
            id,
            show_assistant_meta,
            show_assistant_copy_button,
            ..
        } = &mut row
            && messages_without_meta.contains(id.as_str())
        {
            *show_assistant_meta = false;
            *show_assistant_copy_button = false;
        }
        result.push(row);
        if let Some(meta) = meta_after_index.remove(&index) {
            result.push(meta);
        }
    }
    result
}

/// Delegation already has a durable child card. Its tool row goes only once
/// the returned task id identifies that child; pending calls can share a prompt.
fn without_subagent_delegation_rows(
    entries: Vec<TimelineEntry>,
    app_owned_tasks: &BTreeSet<NodeId>,
) -> Vec<TimelineEntry> {
    let mut children_by_run: HashMap<RunId, HashSet<&str>> = HashMap::new();
    for entry in &entries {
        if let TimelineEntryKind::Event(item) = &entry.kind
            && let ItemKind::Subagent { task } = &item.kind
            && app_owned_tasks.contains(task)
            && let Some(run) = &item.run
        {
            children_by_run
                .entry(run.clone())
                .or_default()
                .insert(task.as_str());
        }
    }
    let keep: Vec<bool> = entries
        .iter()
        .map(|entry| {
            let Some(work) = entry.work() else {
                return true;
            };
            if work_entry_display_indicates_tool_failure(work) {
                return true;
            }
            let Some(item) = &work.item else {
                return true;
            };
            let (ItemKind::DynamicTool { name, output, .. }, Some(run)) = (&item.kind, &item.run)
            else {
                return true;
            };
            if !matches!(item.status, ItemStatus::Running | ItemStatus::Completed)
                || resolve_tool_definition(Some(name)).map(|definition| definition.summary_action)
                    != Some(ToolSummaryAction::Delegate)
            {
                return true;
            }
            let Some(output) = compact_dynamic_tool_output(output.as_ref().map(|output| &output.0))
            else {
                return true;
            };
            if output.is_error {
                return true;
            }
            output.task_id.as_deref().is_none_or(|task| {
                !children_by_run
                    .get(run)
                    .is_some_and(|children| children.contains(task))
            })
        })
        .collect();
    entries
        .into_iter()
        .zip(keep)
        .filter_map(|(entry, keep)| keep.then_some(entry))
        .collect()
}

/// A steer or later activity ends thinking even when the provider omits its completion.
fn settle_superseded_reasoning(entries: &mut [TimelineEntry]) {
    let Some(last) = entries.len().checked_sub(1) else {
        return;
    };
    for entry in &mut entries[..last] {
        if let TimelineEntryKind::Work(work) = &mut entry.kind
            && work.item_type == Some(ItemType::Reasoning)
            && work.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
        {
            work.tool_lifecycle_status = Some(ToolLifecycleStatus::Completed);
        }
    }
}

fn breaks_work_group(work: &WorkLogEntry) -> bool {
    is_compaction(work)
        || work.tone == WorkTone::Error
        || work.source_activity == Some(SourceActivity::RuntimeError)
        || matches!(
            work.item_type,
            Some(ItemType::SystemNotice | ItemType::Notification)
        )
}

/// The toggle summarizing a settled group of calls.
fn work_toggle_row(
    entry: &TimelineEntry,
    run: Option<RunId>,
    visible: Vec<WorkLogEntry>,
    expanded: bool,
) -> DesktopRow {
    let summary_kind = tool_group_summary_kind(&visible);
    let primary_source = visible.iter().find(|entry| entry.tool_source.is_some());
    let primary_source_icon = primary_source
        .and_then(|entry| entry.tool_source.as_ref())
        .filter(|source| !source.key.is_empty())
        .map(|source| {
            visible
                .iter()
                .find(|entry| {
                    entry.tool_source.as_ref().map(|other| &other.key) == Some(&source.key)
                        && entry.tool_icon.is_some()
                })
                .and_then(|entry| entry.tool_icon.clone())
                .or_else(|| source.icon.clone())
        });
    let tool_surface = primary_source
        .and_then(|entry| entry.tool_surface)
        .or_else(|| visible.iter().rev().find_map(|entry| entry.tool_surface));
    let tool_icon = primary_source_icon.flatten().or_else(|| {
        visible
            .iter()
            .rev()
            .find_map(|entry| entry.tool_icon.clone())
    });
    let has_failure = visible
        .iter()
        .rev()
        .find(|entry| entry_is_tool_like(entry))
        .is_some_and(work_entry_display_indicates_tool_failure);
    let single = match visible.as_slice() {
        [single] => Some(single),
        _ => None,
    };
    let single_tool_call = single.filter(|single| {
        entry_is_tool_like(single) && tool_group_action(single) != ToolGroupAction::Edit
    });
    let summary = match (single_tool_call, single) {
        (Some(single), _) => single_tool_call_label(single),
        (None, Some(single)) if !entry_is_tool_like(single) => single.label.clone(),
        _ => summarize_tool_group(&visible).summary,
    };
    let summary_tool_icon = single_tool_call.and_then(|single| {
        resolve_work_entry_tool_presentation(single, Some(ToolLifecycleStatus::Completed))
            .map(|presentation| presentation.icon)
    });
    DesktopRow::WorkToggle {
        id: format!("work-toggle:{}", entry.id),
        created_at: entry.created_at.clone(),
        run,
        group_id: work_group_id(&entry.id),
        hidden_count: visible.len(),
        expanded,
        summary,
        summary_kind,
        tool_surface,
        tool_icon,
        summary_tool_icon,
        has_failure,
        continues_work_log: false,
    }
}

/// Created threads stay below the final answer even when the work that
/// created them folds away.
fn attach_created_thread_summaries(
    rows: Vec<DesktopRow>,
    entries: &[TimelineEntry],
) -> Vec<DesktopRow> {
    let mut terminal_indexes: HashMap<RunId, usize> = HashMap::new();
    let mut collapsed_runs: HashSet<RunId> = HashSet::new();
    for (index, row) in rows.iter().enumerate() {
        match row {
            DesktopRow::Message {
                message,
                show_assistant_meta: true,
                ..
            } => {
                if let Some(run) = &message.run {
                    terminal_indexes.insert(run.clone(), index);
                }
            }
            DesktopRow::TurnFold {
                run,
                expanded: false,
                ..
            } => {
                collapsed_runs.insert(run.clone());
            }
            _ => {}
        }
    }
    let mut created_by_run: HashMap<&RunId, Vec<(&TimelineEntry, &Arc<Item>)>> = HashMap::new();
    for entry in entries {
        let item = match &entry.kind {
            TimelineEntryKind::Work(work) => work.item.as_ref(),
            TimelineEntryKind::Event(item) => Some(item),
            _ => None,
        };
        if let Some(item) = item
            && matches!(item.kind, ItemKind::ThreadCreated { .. })
            && let Some(run) = &item.run
        {
            created_by_run.entry(run).or_default().push((entry, item));
        }
    }
    let mut result = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        if let DesktopRow::Event { item, .. } = &row
            && matches!(item.kind, ItemKind::ThreadCreated { .. })
            && let Some(run) = &item.run
            && let Some(terminal) = terminal_indexes.get(run)
            && (collapsed_runs.contains(run) || index > *terminal)
        {
            continue;
        }
        let summaries = match &row {
            DesktopRow::Message {
                message,
                show_assistant_meta: true,
                ..
            } => message
                .run
                .as_ref()
                .and_then(|run| created_by_run.get(run))
                .map(|created| {
                    created
                        .iter()
                        .map(|(entry, item)| DesktopRow::Event {
                            id: format!("summary:{}", entry.id),
                            created_at: entry.created_at.clone(),
                            item: Arc::clone(item),
                            subagents: None,
                            resource_summary: true,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => vec![],
        };
        result.push(row);
        result.extend(summaries);
    }
    result
}

fn is_work_log_row(row: &DesktopRow) -> bool {
    matches!(
        row,
        DesktopRow::Work { .. }
            | DesktopRow::WorkToggle { .. }
            | DesktopRow::WorkLive { .. }
            | DesktopRow::Thinking { .. }
    )
}

/// The desktop rows of a thread's timeline.
pub fn derive_desktop_rows(input: &DesktopTimelineInput) -> Vec<DesktopRow> {
    let mut entries = input.entries.clone();
    settle_superseded_reasoning(&mut entries);
    let entries = without_subagent_delegation_rows(entries, &input.app_owned_tasks);
    let entries = entries.as_slice();
    let turn_diffs: HashMap<&MessageId, &TurnDiffSummary> = input
        .turn_diffs
        .iter()
        .map(|diff| (&diff.message, &diff.summary))
        .collect();
    let revert_counts = if input.supports_conversation_rollback {
        revert_turn_counts(entries, &input.checkpoints)
    } else {
        BTreeMap::new()
    };
    let messages: Vec<&ChatMessage> = entries.iter().filter_map(TimelineEntry::message).collect();
    let duration_starts = compute_message_duration_start(&messages);
    let terminal_message_ids = terminal_assistant_message_ids(entries);
    let latest_run = input.latest_run.as_ref();
    let unsettled = unsettled_run(latest_run, input.running_run.as_ref());
    let failed_runs = failed_runs(entries, latest_run);
    let superseded_folds = superseded_attempt_folds(entries, &failed_runs);
    let active_runs = active_visual_response_runs(entries, unsettled.as_ref(), input.is_working);
    let runless_work_active = input.is_working && input.runless_work_active;
    let unfolded_runs: HashSet<RunId> = active_runs.union(&failed_runs).cloned().collect();
    let folds = turn_folds(&TurnFoldInput {
        entries,
        terminal_message_ids: &terminal_message_ids,
        latest_run,
        unfolded_runs: &unfolded_runs,
        runless_work_active,
    });
    let collapsed: HashSet<&str> = folds
        .values()
        .filter(|fold| !input.expanded_runs.contains(&fold.run))
        .flat_map(|fold| fold.hidden_entry_ids.iter().map(String::as_str))
        .collect();
    let collapsed_superseded: HashSet<&str> = superseded_folds
        .values()
        .filter(|fold| !input.expanded_attempts.contains(&fold.attempt))
        .flat_map(|fold| fold.hidden_entry_ids.iter().map(String::as_str))
        .collect();
    let run_is_active_response = |run: Option<&RunId>| match run {
        None => runless_work_active,
        Some(run) => active_runs.contains(run),
    };
    let in_active_run = |work: &WorkLogEntry| {
        input.is_working
            && work.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
            && match &work.run {
                None => runless_work_active,
                Some(run) => Some(run) == unsettled.as_ref(),
            }
    };
    let hidden_or_anchor = |entry: &TimelineEntry| {
        collapsed.contains(entry.id.as_str())
            || collapsed_superseded.contains(entry.id.as_str())
            || folds.contains_key(&entry.id)
            || superseded_folds.contains_key(&entry.id)
    };

    // A steer continues the current turn; its elapsed-time header stays below
    // the initiating prompt or automatic wake.
    let active_turn_header_index = if input.is_working {
        last_response_boundary(entries).map_or(0, |index| index + 1)
    } else {
        entries.len()
    };

    // The contiguous trailing work of the active run collapses into one live
    // row that survives between actions: while a tool runs it shows that tool,
    // and once everything settles it keeps the latest tool in past tense
    // instead of vanishing.
    let mut active_tool_entries: Vec<(&TimelineEntry, &WorkLogEntry)> = vec![];
    if input.is_working && (unsettled.is_some() || runless_work_active) {
        let mut tail_attempt: Option<Option<&RunAttemptId>> = None;
        for entry in entries[active_turn_header_index..].iter().rev() {
            let Some(work) = entry.work() else {
                break;
            };
            if work.tone == WorkTone::Error
                || work.source_activity == Some(SourceActivity::RuntimeError)
                || matches!(
                    work.item_type,
                    Some(ItemType::SystemNotice | ItemType::Notification)
                )
                || !run_is_active_response(work.run.as_ref())
                || is_compaction(work)
                || hidden_or_anchor(entry)
            {
                break;
            }
            let attempt = entry.attempt.as_ref().map(|attempt| &attempt.id);
            match tail_attempt {
                None => tail_attempt = Some(attempt),
                Some(tail) if tail != attempt => break,
                Some(_) => {}
            }
            active_tool_entries.insert(0, (entry, work));
        }
    }
    let visible_active: Vec<(&TimelineEntry, &WorkLogEntry)> = active_tool_entries
        .iter()
        .copied()
        .filter(|(_, work)| work_entry_is_visible_in_group(work, true))
        .collect();
    let active_work_anchor = active_tool_entries.first().map(|(entry, _)| *entry);
    let latest_visible = visible_active.last().copied();
    let latest_running = visible_active
        .iter()
        .rev()
        .find(|(_, work)| work_entry_is_active_turn_activity(work))
        .copied();
    let latest_keeps_activity_live = latest_running.is_some()
        || latest_visible.is_some_and(|(_, work)| entry_indicates_tool_success(work));
    let latest_tool_failed = latest_running.is_none()
        && latest_visible.is_some_and(|(_, work)| {
            work.tool_lifecycle_status != Some(ToolLifecycleStatus::Declined)
                && work_entry_display_indicates_tool_failure(work)
        });
    let visible_active_works = || -> Vec<WorkLogEntry> {
        visible_active
            .iter()
            .map(|(_, work)| (*work).clone())
            .collect()
    };
    let active_work_placement = latest_visible.map(|(entry, _)| entry.id.as_str());
    let active_work_row = match (active_work_anchor, latest_visible) {
        (Some(anchor), Some((_, latest))) if !latest_tool_failed => {
            let group_id = work_group_id(&anchor.id);
            Some(DesktopRow::WorkLive {
                id: if latest_keeps_activity_live {
                    LIVE_ACTIVITY_ROW_ID.into()
                } else {
                    format!("work-live:{}", anchor.id)
                },
                created_at: anchor.created_at.clone(),
                entry: Box::new(latest_running.map_or(latest, |(_, work)| work).clone()),
                grouped_entries: visible_active_works(),
                expanded: input.expanded_work_groups.contains(&group_id),
                group_id,
                active: latest_keeps_activity_live,
                continues_work_log: false,
            })
        }
        _ => None,
    };
    let active_work_entry_ids: HashSet<&str> = if active_work_row.is_some() || latest_tool_failed {
        active_tool_entries
            .iter()
            .map(|(entry, _)| entry.id.as_str())
            .collect()
    } else {
        HashSet::new()
    };
    let working_row = || DesktopRow::Working {
        id: WORKING_ROW_ID.into(),
        created_at: input.active_turn_started_at.clone(),
    };

    let mut rows: Vec<DesktopRow> = vec![];
    let mut has_activity_row = false;
    let mut has_active_compaction = false;
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        index += 1;
        if input.is_working && index - 1 == active_turn_header_index {
            rows.push(working_row());
        }
        if Some(entry.id.as_str()) == active_work_placement
            && let Some(DesktopRow::WorkLive {
                created_at,
                grouped_entries,
                group_id,
                expanded,
                active,
                ..
            }) = &active_work_row
        {
            rows.push(active_work_row.clone().expect("live row"));
            has_activity_row |= *active;
            if *expanded {
                rows.push(expanded_work_group_row(
                    group_id,
                    created_at.clone(),
                    grouped_entries.clone(),
                ));
            }
        }
        // The interrupt result is the useful marker; the request is transient
        // bookkeeping that duplicates it.
        if matches!(&entry.kind, TimelineEntryKind::Event(item) if matches!(item.kind, ItemKind::RunInterruptRequest))
        {
            continue;
        }
        if let Some(fold) = folds.get(&entry.id) {
            rows.push(DesktopRow::TurnFold {
                id: format!("turn-fold:{}", fold.run),
                created_at: fold.created_at.clone(),
                run: fold.run.clone(),
                label: fold.label.clone(),
                expanded: input.expanded_runs.contains(&fold.run),
            });
        }
        if collapsed.contains(entry.id.as_str()) {
            continue;
        }
        if let Some(fold) = superseded_folds.get(&entry.id) {
            rows.push(DesktopRow::AttemptFold {
                id: format!("attempt-fold:{}", fold.attempt),
                created_at: fold.created_at.clone(),
                run: fold.run.clone(),
                attempt: fold.attempt.clone(),
                label: "Superseded attempt".into(),
                expanded: input.expanded_attempts.contains(&fold.attempt),
            });
        }
        if collapsed_superseded.contains(entry.id.as_str())
            || active_work_entry_ids.contains(entry.id.as_str())
        {
            continue;
        }
        match &entry.kind {
            TimelineEntryKind::Work(work) if is_compaction(work) => {
                let active = in_active_run(work);
                has_active_compaction |= active;
                rows.push(DesktopRow::ContextCompaction {
                    id: entry.id.clone(),
                    created_at: entry.created_at.clone(),
                    label: work.label.clone(),
                    active,
                });
            }
            TimelineEntryKind::Work(work) => {
                if work
                    .run
                    .as_ref()
                    .is_some_and(|run| failed_runs.contains(run))
                    || (work.item_type == Some(ItemType::Error)
                        && work.tool_lifecycle_status == Some(ToolLifecycleStatus::Failed))
                    || breaks_work_group(work)
                {
                    rows.push(DesktopRow::Work {
                        id: entry.id.clone(),
                        created_at: entry.created_at.clone(),
                        grouped_entries: vec![(**work).clone()],
                        is_expanded_tool_group: false,
                        display_label: None,
                        continues_work_log: false,
                    });
                    continue;
                }
                let mut grouped: Vec<&WorkLogEntry> = vec![work];
                while let Some(next) = entries.get(index) {
                    let Some(next_work) = next.work() else {
                        break;
                    };
                    if breaks_work_group(next_work)
                        || active_work_entry_ids.contains(next.id.as_str())
                        || hidden_or_anchor(next)
                        || next_work.run != work.run
                        || next.attempt.as_ref().map(|attempt| &attempt.id)
                            != entry.attempt.as_ref().map(|attempt| &attempt.id)
                    {
                        break;
                    }
                    grouped.push(next_work);
                    index += 1;
                }
                let visible: Vec<WorkLogEntry> = grouped
                    .into_iter()
                    .filter(|work| work_entry_is_visible_in_group(work, in_active_run(work)))
                    .cloned()
                    .collect();
                if visible.is_empty() {
                    continue;
                }
                let group_id = work_group_id(&entry.id);
                let expanded = input.expanded_work_groups.contains(&group_id);
                if let Some(latest_active) = visible.iter().rev().find(|work| in_active_run(work)) {
                    rows.push(DesktopRow::WorkLive {
                        id: format!("work-live:{}", entry.id),
                        created_at: entry.created_at.clone(),
                        entry: Box::new(latest_active.clone()),
                        grouped_entries: visible.clone(),
                        group_id: group_id.clone(),
                        expanded,
                        active: true,
                        continues_work_log: false,
                    });
                    has_activity_row = true;
                    if expanded {
                        rows.push(expanded_work_group_row(
                            &group_id,
                            entry.created_at.clone(),
                            visible,
                        ));
                    }
                } else if let [single] = visible.as_slice()
                    && entry_is_tool_like(single)
                {
                    let display_label = if tool_group_action(single) == ToolGroupAction::Edit {
                        summarize_tool_group(&visible).summary
                    } else {
                        single_tool_call_label(single)
                    };
                    rows.push(DesktopRow::Work {
                        id: entry.id.clone(),
                        created_at: entry.created_at.clone(),
                        grouped_entries: visible,
                        is_expanded_tool_group: false,
                        display_label: Some(display_label),
                        continues_work_log: false,
                    });
                } else {
                    rows.push(work_toggle_row(
                        entry,
                        work.run.clone(),
                        visible.clone(),
                        expanded,
                    ));
                    if expanded {
                        rows.push(expanded_work_group_row(
                            &group_id,
                            entry.created_at.clone(),
                            visible,
                        ));
                    }
                }
            }
            TimelineEntryKind::ProposedPlan(plan) => rows.push(DesktopRow::ProposedPlan {
                id: entry.id.clone(),
                created_at: entry.created_at.clone(),
                plan: plan.clone(),
            }),
            TimelineEntryKind::Handoff(divider) => rows.push(DesktopRow::Handoff {
                id: entry.id.clone(),
                created_at: entry.created_at.clone(),
                divider: (**divider).clone(),
            }),
            TimelineEntryKind::Event(item) => {
                // Adjacent subagents of one provider turn share a card.
                if let ItemKind::Subagent { .. } = item.kind
                    && let Some(DesktopRow::Event {
                        item: previous,
                        subagents,
                        ..
                    }) = rows.last_mut()
                    && matches!(previous.kind, ItemKind::Subagent { .. })
                    && previous.run == item.run
                    && previous.attempt == item.attempt
                {
                    subagents
                        .get_or_insert_with(|| vec![Arc::clone(previous)])
                        .push(Arc::clone(item));
                    continue;
                }
                rows.push(DesktopRow::Event {
                    id: entry.id.clone(),
                    created_at: entry.created_at.clone(),
                    item: Arc::clone(item),
                    subagents: None,
                    resource_summary: false,
                });
            }
            TimelineEntryKind::Message { message, item } => {
                let assistant = message.role == Role::Assistant;
                let still_in_progress = assistant && run_is_active_response(message.run.as_ref());
                // While the turn runs, the latest assistant message is only
                // provisionally terminal; its footer waits until the turn settles.
                let show_assistant_meta =
                    assistant && terminal_message_ids.contains(&message.id) && !still_in_progress;
                rows.push(DesktopRow::Message {
                    id: entry.id.clone(),
                    created_at: entry.created_at.clone(),
                    message: message.clone(),
                    item: item.clone(),
                    duration_start: duration_starts
                        .get(&message.id)
                        .unwrap_or(&message.created_at)
                        .clone(),
                    show_assistant_meta,
                    show_assistant_copy_button: show_assistant_meta,
                    assistant_copy_streaming: message.streaming || still_in_progress,
                    assistant_turn_diff: assistant
                        .then(|| turn_diffs.get(&message.id).map(|diff| (*diff).clone()))
                        .flatten(),
                    revert_turn_count: (message.role == Role::User)
                        .then(|| revert_counts.get(&message.id).copied())
                        .flatten(),
                });
            }
        }
    }

    // Until the agent's turn is in the timeline, the setup card sits under the
    // send with the working header above it, so nothing moves at the handoff.
    // Once the turn is live the stage list leaves the timeline; a script still
    // running is surfaced by the working header. A failed or cancelled setup
    // stays under the send so its outcome and actions remain reachable.
    let setup = input.worktree_setup.as_ref();
    let setup_handed_off = setup.is_some_and(worktree_setup_agent_started)
        && latest_run.is_some_and(|latest| latest.started_at.is_some());
    // A finished setup keeps the slot until the turn is live, so the card does
    // not jump above the working header before the run starts.
    let setup_owns_working_slot = !setup_handed_off
        && setup.is_some_and(|setup| {
            matches!(
                setup.phase,
                WorktreeSetupPhase::Running | WorktreeSetupPhase::Done
            )
        });
    if let Some(setup) =
        setup.filter(|setup| !setup_handed_off || setup.phase != WorktreeSetupPhase::Running)
    {
        let setup_row = DesktopRow::WorktreeSetup {
            id: WORKTREE_SETUP_ROW_ID.into(),
            created_at: setup.started_at.clone(),
            snapshot: setup.clone(),
            embedded: setup_handed_off,
        };
        // The main pass may already have placed the working header (a
        // bootstrap counts as working); the card goes right under it.
        let working_index = setup_owns_working_slot
            .then(|| {
                rows.iter()
                    .position(|row| matches!(row, DesktopRow::Working { .. }))
            })
            .flatten();
        if let Some(working_index) = working_index {
            rows.insert(working_index + 1, setup_row);
        } else {
            let insert_at = rows
                .iter()
                .position(|row| {
                    matches!(row, DesktopRow::Message { message, .. } if message.role == Role::User)
                })
                .map_or(rows.len(), |index| index + 1);
            let inserted = if setup_owns_working_slot {
                vec![
                    DesktopRow::Working {
                        id: WORKING_ROW_ID.into(),
                        created_at: Some(setup.started_at.clone()),
                    },
                    setup_row,
                ]
            } else {
                vec![setup_row]
            };
            rows.splice(insert_at..insert_at, inserted);
        }
    }

    // A setup that owns the working slot shows no activity row of its own.
    let has_working_row = rows
        .iter()
        .any(|row| matches!(row, DesktopRow::Working { .. }));
    if input.is_working && !has_working_row && active_turn_header_index == entries.len() {
        rows.push(working_row());
    }
    if input.is_working
        && !setup_owns_working_slot
        && !has_active_compaction
        && (!has_activity_row || latest_tool_failed)
    {
        // A failed latest tool hands the row back to thinking; its group stays
        // reachable through the same disclosure the live row offers.
        match active_work_anchor.filter(|_| latest_tool_failed) {
            Some(anchor) => {
                let group_id = work_group_id(&anchor.id);
                let expanded = input.expanded_work_groups.contains(&group_id);
                rows.push(DesktopRow::Thinking {
                    id: LIVE_ACTIVITY_ROW_ID.into(),
                    created_at: input.active_turn_started_at.clone(),
                    group_id: Some(group_id.clone()),
                    expanded,
                    continues_work_log: false,
                });
                if expanded {
                    rows.push(expanded_work_group_row(
                        &group_id,
                        anchor.created_at.clone(),
                        visible_active_works(),
                    ));
                }
            }
            None => rows.push(DesktopRow::Thinking {
                id: LIVE_ACTIVITY_ROW_ID.into(),
                created_at: input.active_turn_started_at.clone(),
                group_id: None,
                expanded: false,
                continues_work_log: false,
            }),
        }
    }

    let mut rows =
        attach_trailing_tool_groups_to_assistant(attach_created_thread_summaries(rows, entries));
    // Adjacent work stays one visual list even when virtualization splits it.
    for index in 1..rows.len() {
        if is_work_log_row(&rows[index])
            && let Some(continues) = rows[index - 1].continues_work_log_mut()
        {
            *continues = true;
        }
    }
    rows
}

#[cfg(test)]
#[path = "desktop_tests.rs"]
mod tests;
