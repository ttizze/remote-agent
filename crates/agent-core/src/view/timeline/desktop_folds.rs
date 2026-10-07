//! Which desktop timeline entries fold away: settled turns behind "Worked
//! for …", output of superseded attempts, and the runs that stay open.
use super::desktop::{TimelineLatestRun, is_compaction, is_work_where};
use super::entries::{TimelineEntry, TimelineEntryKind, is_persistent_resource_card};
use super::timing::{elapsed_ms, format_duration};
use crate::view::work_log::{
    ItemType, ToolLifecycleStatus, presentation::work_entry_display_indicates_tool_failure,
};
use agent_domain::{
    AttemptStatus, InputIntent, Item, ItemKind, ItemStatus, MessageId, Role, RunAttemptId, RunId,
    RunStatus, Timestamp,
};
use std::collections::{HashMap, HashSet};

fn entry_run(entry: &TimelineEntry) -> Option<&RunId> {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } if message.role == Role::Assistant => {
            message.run.as_ref()
        }
        TimelineEntryKind::ProposedPlan(plan) => plan.run.as_ref(),
        TimelineEntryKind::Work(work) => work.run.as_ref(),
        _ => None,
    }
}

fn is_user_message(entry: &TimelineEntry) -> bool {
    entry
        .message()
        .is_some_and(|message| message.role == Role::User)
}

pub(super) fn terminal_assistant_message_ids(entries: &[TimelineEntry]) -> HashSet<MessageId> {
    let mut last_by_response: HashMap<String, MessageId> = HashMap::new();
    let mut unkeyed_response = 0;
    for message in entries.iter().filter_map(TimelineEntry::message) {
        match message.role {
            Role::User => unkeyed_response += 1,
            Role::Assistant => {
                let key = match &message.run {
                    Some(run) => format!("turn:{run}"),
                    None => format!("unkeyed:{unkeyed_response}"),
                };
                last_by_response.insert(key, message.id.clone());
            }
            Role::System => {}
        }
    }
    last_by_response.into_values().collect()
}

pub(super) struct TurnFold {
    pub(super) run: RunId,
    pub(super) created_at: Timestamp,
    pub(super) hidden_entry_ids: HashSet<String>,
    pub(super) label: String,
}

pub(super) struct SupersededAttemptFold {
    pub(super) run: RunId,
    pub(super) attempt: RunAttemptId,
    pub(super) created_at: Timestamp,
    pub(super) hidden_entry_ids: HashSet<String>,
}

/// Groups only provider output owned by a superseded attempt. User messages
/// stay visible because they are inputs to the logical run, including the
/// steer that started the replacement attempt.
pub(super) fn superseded_attempt_folds(
    entries: &[TimelineEntry],
    unfolded_runs: &HashSet<RunId>,
) -> HashMap<String, SupersededAttemptFold> {
    let mut by_attempt: Vec<(&RunAttemptId, Vec<&TimelineEntry>)> = vec![];
    for entry in entries {
        let Some(attempt) = &entry.attempt else {
            continue;
        };
        if attempt.status != AttemptStatus::Superseded
            || unfolded_runs.contains(&attempt.run)
            || is_user_message(entry)
            || is_persistent_resource_card(entry)
            || is_work_where(entry, |work| work.item_type == Some(ItemType::SystemNotice))
        {
            continue;
        }
        match by_attempt.iter_mut().find(|(id, _)| **id == attempt.id) {
            Some((_, group)) => group.push(entry),
            None => by_attempt.push((&attempt.id, vec![entry])),
        }
    }
    by_attempt
        .into_iter()
        .filter_map(|(_, group)| {
            let first = group.first()?;
            let attempt = first.attempt.as_ref()?;
            Some((
                first.id.clone(),
                SupersededAttemptFold {
                    run: attempt.run.clone(),
                    attempt: attempt.id.clone(),
                    created_at: first.created_at.clone(),
                    hidden_entry_ids: group.iter().map(|entry| entry.id.clone()).collect(),
                },
            ))
        })
        .collect()
}

/// The latest run stays unsettled while it runs or has no completion. This is
/// keyed on the run's own lifecycle rather than transient working state: right
/// after a send the previous run is still the latest one until the Host
/// creates the new one, and folding must not flicker through that window.
pub(super) fn unsettled_run(
    latest: Option<&TimelineLatestRun>,
    running: Option<&RunId>,
) -> Option<RunId> {
    if let Some(running) = running {
        return Some(running.clone());
    }
    let latest = latest?;
    let settled = latest.completed_at.is_some()
        && !matches!(
            latest.status,
            RunStatus::Running | RunStatus::Starting | RunStatus::Waiting
        );
    (!settled).then(|| latest.run.clone())
}

/// `runless_key` stands in for the run of entries that have none.
fn fold_run(entry: &TimelineEntry, runless_key: Option<&RunId>) -> Option<RunId> {
    match &entry.kind {
        TimelineEntryKind::Work(work) if work.item_type == Some(ItemType::SystemNotice) => None,
        TimelineEntryKind::Message { message, .. } if message.role == Role::Assistant => {
            message.run.as_ref().or(runless_key).cloned()
        }
        TimelineEntryKind::Work(work) => work.run.as_ref().or(runless_key).cloned(),
        TimelineEntryKind::Event(item)
            if is_persistent_resource_card(entry)
                || matches!(item.kind, ItemKind::Subagent { .. }) =>
        {
            item.run.as_ref().or(runless_key).cloned()
        }
        _ => None,
    }
}

/// A steer adds input to its existing turn without starting a new response.
fn starts_response(entry: &TimelineEntry) -> bool {
    match &entry.kind {
        TimelineEntryKind::Message { message, .. } => {
            message.role == Role::User
                && !matches!(
                    message.input_intent,
                    Some(InputIntent::Steer | InputIntent::PromotedQueuedToSteer)
                )
        }
        TimelineEntryKind::Work(work) => work.item_type == Some(ItemType::Notification),
        _ => false,
    }
}

/// A promptless provider restart replaces the native turn without adding a
/// user message, so every provider turn since the initiating prompt stays one
/// visual response. Steers keep that boundary; an automatic wake starts a new
/// response with its notification.
pub(super) fn last_response_boundary(entries: &[TimelineEntry]) -> Option<usize> {
    entries.iter().rposition(starts_response)
}

pub(super) fn active_visual_response_runs(
    entries: &[TimelineEntry],
    unsettled: Option<&RunId>,
    is_working: bool,
) -> HashSet<RunId> {
    let mut runs = HashSet::new();
    let Some(unsettled) = unsettled else {
        return runs;
    };
    runs.insert(unsettled.clone());
    if !is_working {
        return runs;
    }
    let start = last_response_boundary(entries).map_or(0, |index| index + 1);
    runs.extend(entries[start..].iter().filter_map(entry_run).cloned());
    runs
}

fn failed_item(entry: &TimelineEntry) -> Option<&Item> {
    let item = match &entry.kind {
        TimelineEntryKind::Event(item) => item,
        TimelineEntryKind::Work(work) => work.item.as_ref()?,
        _ => return None,
    };
    (matches!(item.kind, ItemKind::Error { .. }) && item.status == ItemStatus::Failed)
        .then_some(item.as_ref())
}

pub(super) fn failed_runs(
    entries: &[TimelineEntry],
    latest: Option<&TimelineLatestRun>,
) -> HashSet<RunId> {
    let mut failed = HashSet::new();
    if let Some(latest) = latest.filter(|latest| latest.status == RunStatus::Failed) {
        failed.insert(latest.run.clone());
    }
    failed.extend(
        entries
            .iter()
            .filter_map(|entry| failed_item(entry)?.run.clone()),
    );
    failed
}

fn later(a: &Timestamp, b: &Timestamp) -> Timestamp {
    if b.millis() > a.millis() {
        b.clone()
    } else {
        a.clone()
    }
}

pub(super) struct TurnFoldInput<'a> {
    pub(super) entries: &'a [TimelineEntry],
    pub(super) terminal_message_ids: &'a HashSet<MessageId>,
    pub(super) latest_run: Option<&'a TimelineLatestRun>,
    pub(super) unfolded_runs: &'a HashSet<RunId>,
    /// Keeps the latest runless response open.
    pub(super) runless_work_active: bool,
}

struct TurnGroup<'a> {
    entries: Vec<&'a TimelineEntry>,
    terminal: Option<&'a TimelineEntry>,
    has_streaming_message: bool,
    /// The user message or notification that started the turn. Entry
    /// timestamps alone undercount the duration: the first entry appears only
    /// once the provider produces output, and a turn cut short by a steer may
    /// hold a single instantaneous commentary message.
    start_boundary: Option<Timestamp>,
    anchor_entry_id: String,
}

/// Settled turns fold the activity before their terminal assistant message
/// behind a "Worked for …" row. Ordinary trailing work joins the fold, while
/// failures and work still in progress stay visible. A prompt without a run
/// (a provider-native subagent) folds its response the same way.
pub(super) fn turn_folds(input: &TurnFoldInput<'_>) -> HashMap<String, TurnFold> {
    let interrupted_runs: HashSet<&RunId> = input
        .entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            TimelineEntryKind::Event(item)
                if matches!(
                    item.kind,
                    ItemKind::RunInterruptRequest | ItemKind::RunInterruptResult { .. }
                ) =>
            {
                item.run.as_ref()
            }
            _ => None,
        })
        .collect();

    let mut groups: Vec<(RunId, TurnGroup)> = vec![];
    let mut runless_failed: HashSet<RunId> = HashSet::new();
    // Each runless prompt lends its response a fold key of its own.
    let mut runless_key: Option<RunId> = None;
    let mut pending_boundary: Option<(Timestamp, String)> = None;
    for (index, entry) in input.entries.iter().enumerate() {
        if starts_response(entry) {
            pending_boundary = input
                .entries
                .get(index + 1)
                .map(|next| (entry.created_at.clone(), next.id.clone()));
            let boundary_run = match &entry.kind {
                TimelineEntryKind::Message { message, .. } => message.run.as_ref(),
                TimelineEntryKind::Work(work) => work.run.as_ref(),
                _ => None,
            };
            runless_key = match boundary_run {
                None => RunId::new(format!("runless:{}", entry.id)).ok(),
                Some(_) => None,
            };
            continue;
        }
        let Some(run) = fold_run(entry, runless_key.as_ref()) else {
            continue;
        };
        if Some(&run) == runless_key.as_ref() && failed_item(entry).is_some() {
            runless_failed.insert(run.clone());
        }
        let position = match groups.iter().position(|(key, _)| *key == run) {
            Some(position) => position,
            None => {
                // Each boundary starts at most one turn; a second turn after the
                // same prompt falls back to its own first entry.
                let (start_boundary, anchor_entry_id) = match pending_boundary.take() {
                    Some((created_at, anchor)) => (Some(created_at), anchor),
                    None => (None, entry.id.clone()),
                };
                groups.push((
                    run,
                    TurnGroup {
                        entries: vec![],
                        terminal: None,
                        has_streaming_message: false,
                        start_boundary,
                        anchor_entry_id,
                    },
                ));
                groups.len() - 1
            }
        };
        let group = &mut groups[position].1;
        group.entries.push(entry);
        if let Some(message) = entry.message() {
            if input.terminal_message_ids.contains(&message.id) {
                group.terminal = Some(entry);
            }
            if message.streaming {
                group.has_streaming_message = true;
            }
        }
    }

    let mut folds = HashMap::new();
    for (run, group) in groups {
        if input.unfolded_runs.contains(&run)
            || interrupted_runs.contains(&run)
            || runless_failed.contains(&run)
            || (input.runless_work_active && Some(&run) == runless_key.as_ref())
            || group.has_streaming_message
        {
            continue;
        }
        let terminal_id = group.terminal.map(|terminal| terminal.id.as_str());
        let terminal_index = terminal_id
            .and_then(|id| group.entries.iter().position(|entry| entry.id == id))
            .unwrap_or(group.entries.len());
        let mut hidden_entry_ids = HashSet::new();
        for (index, entry) in group.entries.iter().enumerate() {
            if Some(entry.id.as_str()) == terminal_id {
                continue;
            }
            let foldable_trailing_activity = is_work_where(entry, |work| {
                work.tool_lifecycle_status != Some(ToolLifecycleStatus::InProgress)
                    && !work_entry_display_indicates_tool_failure(work)
            });
            if !is_work_where(entry, is_compaction)
                && index > terminal_index
                && !foldable_trailing_activity
            {
                continue;
            }
            // Linked resources can outlive their launching run and stay
            // visible after the surrounding work folds.
            if is_persistent_resource_card(entry)
                || is_work_where(entry, |work| work.item_type == Some(ItemType::Notification))
            {
                continue;
            }
            hidden_entry_ids.insert(entry.id.clone());
        }
        // A lone compaction row stays visible; it folds only with other work.
        let hides_other_work = group.entries.iter().any(|entry| {
            hidden_entry_ids.contains(&entry.id) && !is_work_where(entry, is_compaction)
        });
        if !hides_other_work {
            continue;
        }
        let (Some(first), Some(last)) = (group.entries.first(), group.entries.last()) else {
            continue;
        };
        let latest = input.latest_run.filter(|latest| latest.run == run);
        let latest_interrupted =
            latest.is_some_and(|latest| latest.status == RunStatus::Interrupted);
        // A turn cut short by a steer leaves trailing work behind its terminal
        // message; take whichever ended last.
        let last_end = last
            .message()
            .map_or(&last.created_at, |message| &message.updated_at);
        let elapsed = match latest
            .and_then(|latest| Some((latest.started_at.as_ref()?, latest.completed_at.as_ref()?)))
        {
            Some((started, completed)) => elapsed_ms(started, completed),
            None => {
                let end = match group.terminal.and_then(TimelineEntry::message) {
                    Some(terminal) => later(&terminal.updated_at, last_end),
                    None => last_end.clone(),
                };
                elapsed_ms(
                    group.start_boundary.as_ref().unwrap_or(&first.created_at),
                    &end,
                )
            }
        };
        let duration = format_duration(elapsed as f64);
        let label = if latest_interrupted {
            format!("You stopped after {duration}")
        } else {
            format!("Worked for {duration}")
        };
        folds.insert(
            group.anchor_entry_id.clone(),
            TurnFold {
                run,
                created_at: group
                    .start_boundary
                    .clone()
                    .unwrap_or_else(|| first.created_at.clone()),
                hidden_entry_ids,
                label,
            },
        );
    }
    folds
}
