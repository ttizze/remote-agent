//! Presents the mobile feed: folds settled runs, collapses tool calls behind
//! toggles, keeps one live slot and marks rows that continue a work log.
use super::mobile::{
    ActivityGroup, FeedActivity, FeedInput, FeedLatestRun, FeedRow, FeedStatus, WorkToggle,
    compact_work_entry_text, is_failed_error, single_row, split_activity_group,
};
use crate::js_text::js_trim;
use crate::view::timeline::timing::format_duration;
use crate::view::work_log::command_label::command_program_name;
use crate::view::work_log::presentation::{
    ToolGroupAction, live_activity_tool_status, resolve_work_entry_tool_presentation,
    summarize_tool_group, tool_group_action, tool_group_summary_kind,
    work_entry_display_indicates_tool_failure,
};
use crate::view::work_log::{ItemType, ToolLifecycleStatus, WorkLogEntry};
use agent_domain::{ItemKind, ItemStatus, RunId, RunStatus, Timestamp};
use std::collections::{BTreeMap, BTreeSet};

/// Shared by the trailing live tool row and its Thinking fallback.
pub const LIVE_ACTIVITY_ROW_ID: &str = "live-activity-row";

pub fn is_context_compaction_activity_group(group: &ActivityGroup) -> bool {
    matches!(group.activities.as_slice(), [activity]
        if matches!(activity.item.kind, ItemKind::Compaction { .. }))
}

fn is_user_input_activity_group(group: &ActivityGroup) -> bool {
    group
        .activities
        .iter()
        .any(|activity| activity.work_entry.question_answer.is_some())
}

pub fn thread_feed_run_is_unsettled(run: Option<&FeedLatestRun>) -> bool {
    let Some(run) = run else {
        return false;
    };
    if run.status == RunStatus::Queued {
        return false;
    }
    run.completed_at.is_none()
        || matches!(
            run.status,
            RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting
        )
}

fn unsettled_run_id(run: Option<&FeedLatestRun>) -> Option<RunId> {
    thread_feed_run_is_unsettled(run).then(|| run.expect("unsettled run").run.clone())
}

/// A row is shown unless it is an unfinished tool call nobody watches.
pub fn thread_feed_activity_is_visible(
    prominent: bool,
    status: Option<FeedStatus>,
    tool_like: bool,
    lifecycle_status: Option<ToolLifecycleStatus>,
) -> bool {
    prominent
        || matches!(
            lifecycle_status,
            Some(
                ToolLifecycleStatus::Stopped
                    | ToolLifecycleStatus::Declined
                    | ToolLifecycleStatus::Idle
            )
        )
        || !(tool_like && status == Some(FeedStatus::Neutral))
}

fn activity_is_visible(activity: &FeedActivity) -> bool {
    thread_feed_activity_is_visible(
        activity.prominent,
        activity.status,
        activity.tool_like,
        Some(activity.lifecycle_status),
    )
}

/// Runs that ended in a provider failure, which keep their work unfolded.
pub fn failed_feed_run_ids(
    feed: &[FeedRow],
    latest_run: Option<&FeedLatestRun>,
) -> BTreeSet<RunId> {
    let mut failed = BTreeSet::new();
    if let Some(run) = latest_run.filter(|run| run.status == RunStatus::Failed) {
        failed.insert(run.run.clone());
    }
    for row in feed {
        for activity in row.activities() {
            if is_failed_error(&activity.item)
                && let Some(run) = &activity.item.run
            {
                failed.insert(run.clone());
            }
        }
    }
    failed
}

struct RunFold {
    run: RunId,
    created_at: Timestamp,
    hidden: BTreeSet<String>,
    label: String,
}

struct RunGroup<'a> {
    run: RunId,
    entries: Vec<&'a FeedRow>,
    start_boundary: Option<Timestamp>,
}

fn elapsed_ms(start: &Timestamp, end: &Timestamp) -> i64 {
    (end.millis() - start.millis()).max(0)
}

fn max_timestamp<'a>(left: Option<&'a Timestamp>, right: &'a Timestamp) -> &'a Timestamp {
    match left {
        Some(left) if right.millis() <= left.millis() => left,
        _ => right,
    }
}

/// A prompt without a run (a provider-native subagent, or an imported turn)
/// folds its response like a run. `runless_work_active` keeps the latest
/// runless response open; new runs must not reopen imported turns.
fn derive_run_folds(
    feed: &[FeedRow],
    latest_run: Option<&FeedLatestRun>,
    runless_work_active: bool,
) -> BTreeMap<String, RunFold> {
    let mut first_assistant: BTreeMap<RunId, &str> = BTreeMap::new();
    let mut terminal_assistant: BTreeMap<RunId, &str> = BTreeMap::new();
    let mut interrupted: BTreeSet<RunId> = BTreeSet::new();
    let mut failed = failed_feed_run_ids(feed, latest_run);
    let mut groups: Vec<RunGroup> = vec![];
    // Each runless prompt lends its response a fold key of its own, decided
    // per prompt so a new run does not unfold every imported turn above it.
    let mut runless_key: Option<RunId> = None;
    let mut pending_user_boundary: Option<Timestamp> = None;
    for row in feed {
        let run = match row {
            FeedRow::Message { message, id, .. } if message.role == agent_domain::Role::User => {
                pending_user_boundary = Some(message.created_at.clone());
                runless_key = match &message.run {
                    None => Some(RunId::new(format!("runless:{id}")).expect("runless key")),
                    Some(_) => None,
                };
                continue;
            }
            FeedRow::Message { message, .. } => message.run.clone().or_else(|| runless_key.clone()),
            FeedRow::ActivityGroup(group) => group.run.clone().or_else(|| runless_key.clone()),
            _ => None,
        };
        let Some(run) = run else {
            continue;
        };
        let index = match groups.iter().position(|group| group.run == run) {
            Some(index) => index,
            None => {
                groups.push(RunGroup {
                    run: run.clone(),
                    entries: vec![],
                    start_boundary: pending_user_boundary.take(),
                });
                groups.len() - 1
            }
        };
        groups[index].entries.push(row);
        match row {
            FeedRow::Message { id, .. } => {
                first_assistant.entry(run.clone()).or_insert(id);
                terminal_assistant.insert(run.clone(), id);
            }
            FeedRow::ActivityGroup(group) => {
                for activity in &group.activities {
                    if matches!(activity.item.kind, ItemKind::RunInterruptResult { .. }) {
                        interrupted.insert(run.clone());
                    }
                    if Some(&run) == runless_key.as_ref() && is_failed_error(&activity.item) {
                        failed.insert(run.clone());
                    }
                }
            }
            _ => {}
        }
    }

    let active_run = unsettled_run_id(latest_run);
    let mut folds = BTreeMap::new();
    for group in groups {
        let run = &group.run;
        if Some(run) == active_run.as_ref()
            || (runless_work_active && Some(run) == runless_key.as_ref())
            || interrupted.contains(run)
            || failed.contains(run)
            || group
                .entries
                .iter()
                .any(|row| matches!(row, FeedRow::Message { message, .. } if message.streaming))
        {
            continue;
        }
        let first_id = first_assistant.get(run).copied();
        let terminal_id = terminal_assistant.get(run).copied();
        let hidden: BTreeSet<String> = group
            .entries
            .iter()
            .filter(|row| {
                Some(row.id()) != first_id
                    && Some(row.id()) != terminal_id
                    && !row.activities().iter().any(|activity| {
                        activity.prominent
                            || matches!(activity.item.kind, ItemKind::Notification { .. })
                    })
            })
            .map(|row| row.id().to_owned())
            .collect();
        let (Some(first), Some(first_hidden), Some(last)) = (
            group.entries.first(),
            group.entries.iter().find(|row| hidden.contains(row.id())),
            group.entries.last(),
        ) else {
            continue;
        };
        let hides_non_compaction_work = group.entries.iter().any(|row| {
            hidden.contains(row.id())
                && !matches!(row, FeedRow::ActivityGroup(group) if is_context_compaction_activity_group(group))
        });
        if !hides_non_compaction_work {
            continue;
        }
        let message_end = |row: &FeedRow| match row {
            FeedRow::Message { message, .. } => Some(message.updated_at.clone()),
            _ => None,
        };
        let terminal_end = terminal_id
            .and_then(|id| group.entries.iter().find(|row| row.id() == id))
            .and_then(|row| message_end(row));
        let last_end = message_end(last).unwrap_or_else(|| last.created_at().clone());
        let latest = latest_run.filter(|latest| &latest.run == run);
        let elapsed = match latest {
            Some(FeedLatestRun {
                started_at: Some(started),
                completed_at: Some(completed),
                ..
            }) => elapsed_ms(started, completed),
            _ => elapsed_ms(
                group.start_boundary.as_ref().unwrap_or(first.created_at()),
                max_timestamp(terminal_end.as_ref(), &last_end),
            ),
        };
        let duration = format_duration(elapsed as f64);
        let stopped = latest.is_some_and(|latest| {
            matches!(latest.status, RunStatus::Interrupted | RunStatus::Cancelled)
        });
        folds.insert(
            first_hidden.id().to_owned(),
            RunFold {
                run: run.clone(),
                created_at: first_hidden.created_at().clone(),
                hidden,
                label: if stopped {
                    format!("You stopped after {duration}")
                } else {
                    format!("Worked for {duration}")
                },
            },
        );
    }
    folds
}

/// A steer or a later activity ends thinking even when the provider omits
/// its completion. In the tail group the last thought may still stream.
fn settle_superseded_reasoning(row: &FeedRow, tail: bool) -> FeedRow {
    let FeedRow::ActivityGroup(group) = row else {
        return row.clone();
    };
    let last = group.activities.len().saturating_sub(1);
    let mut group = group.clone();
    for (index, activity) in group.activities.iter_mut().enumerate() {
        if activity.work_entry.item_type == Some(ItemType::Reasoning)
            && activity.lifecycle_status == ToolLifecycleStatus::InProgress
            && (!tail || index < last)
        {
            activity.lifecycle_status = ToolLifecycleStatus::Completed;
            activity.status = Some(FeedStatus::Success);
            activity.work_entry.tool_lifecycle_status = Some(ToolLifecycleStatus::Completed);
        }
    }
    FeedRow::ActivityGroup(group)
}

fn is_work_log_row(row: Option<&FeedRow>) -> bool {
    match row {
        Some(FeedRow::WorkToggle(_) | FeedRow::Thinking { .. }) => true,
        Some(FeedRow::ActivityGroup(group)) => {
            !is_context_compaction_activity_group(group)
                && group.activities.iter().all(|activity| {
                    !activity.prominent
                        && !matches!(
                            activity.item.kind,
                            ItemKind::Notification { .. } | ItemKind::Subagent { .. }
                        )
                })
        }
        _ => false,
    }
}

/// The run state the tool rows of one presentation share.
struct Live<'a> {
    expanded_groups: &'a BTreeSet<String>,
    active_run: Option<RunId>,
    working: bool,
}

impl Live<'_> {
    fn active(&self, activity: &FeedActivity) -> bool {
        self.working
            && activity.lifecycle_status == ToolLifecycleStatus::InProgress
            && activity.run == self.active_run
    }
}

/// Presents the feed: folds settled runs behind "Worked for …", collapses
/// tool calls behind toggles and keeps exactly one live slot while working.
pub fn derive_thread_feed_presentation(feed: &[FeedRow], input: &FeedInput) -> Vec<FeedRow> {
    let latest_run = input.latest_run.as_ref();
    let retained: Vec<&FeedRow> = feed
        .iter()
        .filter(|row| {
            !matches!(
                row,
                FeedRow::RunFold { .. } | FeedRow::WorkToggle(_) | FeedRow::Thinking { .. }
            )
        })
        .collect();
    let source: Vec<FeedRow> = retained
        .iter()
        .enumerate()
        .map(|(index, row)| settle_superseded_reasoning(row, index + 1 == retained.len()))
        .collect();
    let failed = failed_feed_run_ids(&source, latest_run);
    let tail_group_id = match source.last() {
        Some(FeedRow::ActivityGroup(group)) => Some(group.id.as_str()),
        _ => None,
    };
    let active_run = unsettled_run_id(latest_run);
    let working = input.active_work_started_at.is_some()
        && latest_run.is_none_or(|run| run.status != RunStatus::Preparing);
    let folds = derive_run_folds(&source, latest_run, working && input.runless_work_active);
    let collapsed: BTreeSet<&str> = folds
        .values()
        .filter(|fold| !input.expanded_runs.contains(&fold.run))
        .flat_map(|fold| fold.hidden.iter().map(String::as_str))
        .collect();
    let live = Live {
        expanded_groups: &input.expanded_work_groups,
        active_run: active_run.clone(),
        working,
    };
    let mut result: Vec<FeedRow> = vec![];
    for row in &source {
        // A provider-native subagent works without a run: its runless tail is
        // live only while that runless work is active.
        let active_tail = working
            && (active_run.is_some() || input.runless_work_active)
            && matches!(row, FeedRow::ActivityGroup(group)
                if Some(group.id.as_str()) == tail_group_id && group.run == active_run);
        if let Some(fold) = folds.get(row.id()) {
            result.push(FeedRow::RunFold {
                id: format!("run-fold:{}", fold.run),
                created_at: fold.created_at.clone(),
                run: fold.run.clone(),
                label: fold.label.clone(),
                expanded: input.expanded_runs.contains(&fold.run),
            });
        }
        if collapsed.contains(row.id()) {
            continue;
        }
        match row {
            FeedRow::ActivityGroup(group)
                if group.run.as_ref().is_some_and(|run| failed.contains(run)) =>
            {
                result.extend(split_activity_group(group));
            }
            FeedRow::ActivityGroup(group)
                if !is_context_compaction_activity_group(group)
                    && !is_user_input_activity_group(group)
                    && !matches!(
                        group.activities.first().map(|activity| &activity.item.kind),
                        Some(ItemKind::Subagent { .. })
                    ) =>
            {
                append_activity_group_rows(&mut result, group, &live, active_tail);
            }
            row => result.push(row.clone()),
        }
    }
    // Keep exactly one live slot while a run works. When no tool row can
    // carry it yet (or the latest call failed), the slot reads "Thinking".
    if let Some(started_at) = input.active_work_started_at.as_ref().filter(|_| working)
        && !result.iter().any(|row| match row {
            FeedRow::WorkToggle(toggle) => toggle.shimmer,
            FeedRow::ActivityGroup(group) => {
                is_context_compaction_activity_group(group)
                    && group.run == active_run
                    && group.activities[0].item.status == ItemStatus::Running
            }
            _ => false,
        })
    {
        result.push(FeedRow::Thinking {
            id: LIVE_ACTIVITY_ROW_ID.into(),
            created_at: started_at.clone(),
            run: active_run.clone(),
            continues_work_log: false,
        });
    }
    let continues: Vec<bool> = (0..result.len())
        .map(|index| is_work_log_row(result.get(index)) && is_work_log_row(result.get(index + 1)))
        .collect();
    for (row, continues) in result.iter_mut().zip(continues) {
        match row {
            FeedRow::ActivityGroup(ActivityGroup {
                continues_work_log, ..
            })
            | FeedRow::WorkToggle(WorkToggle {
                continues_work_log, ..
            })
            | FeedRow::Thinking {
                continues_work_log, ..
            } => *continues_work_log = continues,
            _ => {}
        }
    }
    result
}

fn append_activity_group_rows(
    result: &mut Vec<FeedRow>,
    group: &ActivityGroup,
    live: &Live,
    active_tail: bool,
) {
    let mut anchors: BTreeMap<&str, &str> = BTreeMap::new();
    let mut anchor: Option<&str> = None;
    for activity in &group.activities {
        if activity.prominent || is_failed_error(&activity.item) {
            anchor = None;
            continue;
        }
        let anchor = *anchor.get_or_insert(&activity.id);
        anchors.insert(&activity.id, anchor);
    }
    let activities: Vec<&FeedActivity> = group
        .activities
        .iter()
        .filter(|activity| activity_is_visible(activity) || live.active(activity))
        .collect();
    if activities.is_empty() {
        return;
    }
    let mut groupable: Vec<&FeedActivity> = vec![];
    let flush = |result: &mut Vec<FeedRow>, groupable: &mut Vec<&FeedActivity>, trailing: bool| {
        let Some(first) = groupable.first() else {
            return;
        };
        let group_id = format!(
            "work-group:{}",
            anchors.get(first.id.as_str()).copied().unwrap_or(&first.id)
        );
        append_tool_group_rows(
            result,
            group,
            groupable,
            group_id,
            live,
            active_tail && trailing,
        );
        groupable.clear();
    };
    for activity in activities {
        if !activity.prominent
            && !is_failed_error(&activity.item)
            && !matches!(activity.item.kind, ItemKind::Notification { .. })
        {
            groupable.push(activity);
            continue;
        }
        flush(result, &mut groupable, false);
        result.push(single_row(activity));
    }
    flush(result, &mut groupable, true);
}

fn single_tool_call_label(activity: &FeedActivity, expanded: bool) -> String {
    if activity.work_entry.item_type == Some(ItemType::Reasoning) {
        if expanded {
            return "Thought".into();
        }
        let preview = compact_work_entry_text(activity.work_entry.detail.as_deref().unwrap_or(""));
        return if preview.is_empty() {
            "Thought".into()
        } else {
            preview
        };
    }
    if let Some(presentation) = resolve_work_entry_tool_presentation(
        &activity.work_entry,
        Some(ToolLifecycleStatus::Completed),
    ) {
        return presentation.display_name;
    }
    activity
        .work_entry
        .command
        .as_deref()
        .map(js_trim)
        .filter(|command| !command.is_empty())
        .map_or_else(|| activity.summary.clone(), str::to_owned)
}

fn live_tool_activity_summary(activity: &FeedActivity, present_tense: bool) -> String {
    let status = live_activity_tool_status(Some(activity.lifecycle_status), present_tense);
    if activity.work_entry.item_type == Some(ItemType::Reasoning) {
        let preview = compact_work_entry_text(activity.work_entry.detail.as_deref().unwrap_or(""));
        return if !preview.is_empty() {
            preview
        } else if status == ToolLifecycleStatus::InProgress {
            "Thinking".into()
        } else {
            "Thought".into()
        };
    }
    let entry = WorkLogEntry {
        tool_lifecycle_status: Some(status),
        ..activity.work_entry.clone()
    };
    if let Some(presentation) = resolve_work_entry_tool_presentation(&entry, None) {
        return presentation.display_name;
    }
    if let Some(command) = activity
        .work_entry
        .command
        .as_deref()
        .map(js_trim)
        .filter(|command| !command.is_empty())
    {
        let verb = match status {
            ToolLifecycleStatus::InProgress => "Running",
            ToolLifecycleStatus::Failed => "Failed",
            ToolLifecycleStatus::Declined => "Declined",
            ToolLifecycleStatus::Stopped => "Stopped",
            ToolLifecycleStatus::Completed | ToolLifecycleStatus::Idle => "Ran",
        };
        return format!(
            "{verb} {}",
            command_program_name(command).unwrap_or_else(|| "command".into())
        );
    }
    activity
        .detail
        .clone()
        .unwrap_or_else(|| activity.summary.clone())
}

fn append_tool_group_rows(
    result: &mut Vec<FeedRow>,
    source: &ActivityGroup,
    activities: &[&FeedActivity],
    group_id: String,
    live: &Live,
    active_tail: bool,
) {
    let expanded = live.expanded_groups.contains(&group_id);
    let latest_active = activities
        .iter()
        .rev()
        .find(|activity| live.active(activity));
    let active = latest_active.is_some();
    let is_live = active_tail || active;
    let latest = *latest_active.unwrap_or_else(|| activities.last().expect("a tool group"));
    // A successful trailing call remains the live slot until the next
    // activity. Failed and stopped calls hand that slot to the Thinking row.
    let shimmer = active_tail && (active || latest.status == Some(FeedStatus::Success));
    let single = (activities.len() == 1).then_some(latest);
    let entries: Vec<WorkLogEntry> = activities
        .iter()
        .map(|activity| activity.work_entry.clone())
        .collect();
    let single_tool_call = single.filter(|single| {
        single.tool_like && tool_group_action(&single.work_entry) != ToolGroupAction::Edit
    });
    let summary = if is_live {
        if expanded && latest.work_entry.item_type == Some(ItemType::Reasoning) {
            if latest.lifecycle_status == ToolLifecycleStatus::InProgress {
                "Thinking".into()
            } else {
                "Thought".into()
            }
        } else {
            live_tool_activity_summary(latest, is_live)
        }
    } else if let Some(single) = single_tool_call {
        single_tool_call_label(single, expanded)
    } else if let Some(single) = single.filter(|single| !single.tool_like) {
        single.work_entry.label.clone()
    } else {
        summarize_tool_group(&entries).summary
    };
    let primary_source = activities.iter().find_map(|activity| {
        activity
            .work_entry
            .tool_source
            .as_ref()
            .map(|source| (activity, source))
    });
    let primary_source_icon = primary_source.and_then(|(_, source)| {
        activities
            .iter()
            .find(|activity| {
                activity.work_entry.tool_source.as_ref().map(|s| &s.key) == Some(&source.key)
                    && activity.work_entry.tool_icon.is_some()
            })
            .and_then(|activity| activity.work_entry.tool_icon.clone())
            .or_else(|| source.icon.clone())
    });
    let tool_surface = primary_source
        .and_then(|(activity, _)| activity.work_entry.tool_surface)
        .or(latest.work_entry.tool_surface)
        .or_else(|| {
            activities
                .iter()
                .rev()
                .find_map(|activity| activity.work_entry.tool_surface)
        });
    let tool_icon = primary_source_icon
        .or_else(|| latest.work_entry.tool_icon.clone())
        .or_else(|| {
            activities
                .iter()
                .rev()
                .find_map(|activity| activity.work_entry.tool_icon.clone())
        });
    let summary_tool_icon = if is_live {
        resolve_work_entry_tool_presentation(&latest.work_entry, None)
    } else {
        single_tool_call.and_then(|single| {
            resolve_work_entry_tool_presentation(
                &single.work_entry,
                Some(ToolLifecycleStatus::Completed),
            )
        })
    }
    .map(|presentation| presentation.icon);
    let has_failure = activities
        .iter()
        .rev()
        .find(|activity| activity.tool_like)
        .is_some_and(|activity| work_entry_display_indicates_tool_failure(&activity.work_entry));
    let summary_entries = if is_live {
        vec![latest.work_entry.clone()]
    } else {
        entries
    };
    result.push(FeedRow::WorkToggle(WorkToggle {
        id: if shimmer {
            LIVE_ACTIVITY_ROW_ID.into()
        } else {
            format!(
                "{}:{group_id}",
                if is_live { "work-live" } else { "work-toggle" }
            )
        },
        created_at: source.created_at.clone(),
        run: source.run.clone(),
        group_id: group_id.clone(),
        hidden_count: activities.len(),
        expanded,
        summary,
        summary_kind: tool_group_summary_kind(&summary_entries),
        tool_surface,
        tool_icon,
        summary_tool_icon,
        has_failure,
        live: is_live,
        shimmer,
        continues_work_log: false,
    }));
    if !expanded {
        return;
    }
    result.push(FeedRow::ActivityGroup(ActivityGroup {
        id: format!("work-details:{group_id}"),
        created_at: activities[0].created_at.clone(),
        run: activities[0].run.clone(),
        activities: activities
            .iter()
            .map(|activity| FeedActivity {
                grouped_tool_detail: true,
                live: live.working
                    && activity.id == latest.id
                    && activity.lifecycle_status == ToolLifecycleStatus::InProgress
                    && activity.run == live.active_run,
                ..(*activity).clone()
            })
            .collect(),
        continues_work_log: false,
    }));
}
