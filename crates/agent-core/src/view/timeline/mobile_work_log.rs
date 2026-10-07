//! What the mobile work log draws for a feed row: the layout of a group, the
//! label, icon, tone and detail of each call, tool toggles and subagent cards.
use super::lifecycle::{LifecycleRow, SubagentLink, lifecycle_row};
use super::mobile::{
    FeedActivity, FeedStatus, WorkIcon, WorkToggle, format_item_full_detail, is_failed_error,
    subagent_task, work_entry_row_label,
};
pub use super::work_row::{
    ProviderFailureRow, WorkActivityDetail, WorkActivityRow, WorkIconTone, WorkLabelTone,
    WorkLogRow, WorkRowIcon, WorkRowRole,
};
use crate::sync::Detail;
use crate::view::agents::{active_status, summarize_subagent_statuses};
use crate::view::time::format_duration;
use crate::view::work_log::ToolSurface;
use crate::view::work_log::item_detail::{ToolCallLines, tool_call_lines, turn_item_output_text};
use crate::view::work_log::presentation::{
    ToolGroupAction, ToolGroupSummaryKind, resolve_work_entry_tool_presentation, tool_group_action,
    work_entry_viewed_image_path,
};
use crate::view::work_log::tool_catalog::ToolLogo;
use crate::view::work_log::turn_item::workspace_preparation_retry_run_ids;
use crate::view::work_log::user_input::{has_question_answer, question_answer_preview};
use agent_domain::{
    Driver, Item, ItemKind, ItemStatus, State, ThreadId, Timestamp,
    WORKSPACE_PREPARATION_FAILURE_CODE,
};
use serde_json::json;

/// How a group of activities is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkLogLayout {
    Empty,
    /// A subagent card or group.
    Subagents,
    /// An expanded group of thoughts, drawn as their text.
    Reasoning,
    /// An expanded tool group: a scrolling list with a capped height.
    GroupedList,
    Rows,
}

pub fn work_log_layout(activities: &[FeedActivity]) -> WorkLogLayout {
    let Some(first) = activities.first() else {
        return WorkLogLayout::Empty;
    };
    if matches!(first.item.kind, ItemKind::Subagent { .. }) {
        WorkLogLayout::Subagents
    } else if first.grouped_tool_detail
        && activities
            .iter()
            .all(|activity| matches!(activity.item.kind, ItemKind::Reasoning))
    {
        WorkLogLayout::Reasoning
    } else if first.grouped_tool_detail {
        WorkLogLayout::GroupedList
    } else {
        WorkLogLayout::Rows
    }
}

fn provider_failure_row(state: &State, activity: &FeedActivity) -> Option<ProviderFailureRow> {
    let item = &activity.item;
    let ItemKind::Error {
        message,
        class,
        code,
        reset_at,
        ..
    } = &item.kind
    else {
        return None;
    };
    if !is_failed_error(item) {
        return None;
    }
    let retry_preparation = item
        .run
        .as_ref()
        .filter(|_| code.as_deref() == Some(WORKSPACE_PREPARATION_FAILURE_CODE))
        .filter(|run| workspace_preparation_retry_run_ids(&state.runs, &state.items).contains(run))
        .cloned();
    Some(ProviderFailureRow {
        summary: activity.summary.clone(),
        warning: class.as_deref() == Some("usage_limit"),
        message: message.clone(),
        reset_at: reset_at.clone(),
        created_at: activity.created_at.clone(),
        retry_preparation,
        copy_text: activity.copy_text.clone(),
    })
}

fn call_lines(item: &Item) -> Option<ToolCallLines> {
    match &item.kind {
        ItemKind::CommandExecution { command, .. } => Some(tool_call_lines(Some(command), None)),
        ItemKind::DynamicTool { input, .. } => Some(tool_call_lines(None, Some(&input.0))),
        ItemKind::WebSearch { query, .. } => {
            Some(tool_call_lines(None, Some(&json!({ "query": query }))))
        }
        _ => None,
    }
}

/// The row for a call; `detail` is the loaded withheld detail of its item.
pub fn work_log_row(
    state: &State,
    activity: &FeedActivity,
    expanded: bool,
    detail: Option<&Detail>,
) -> WorkLogRow {
    if let Some(failure) = provider_failure_row(state, activity) {
        return WorkLogRow::ProviderFailure(failure);
    }
    let item = &activity.item;
    let opens_thread = match &item.kind {
        ItemKind::Notification { notification } => notification.child_thread.clone(),
        _ => None,
    };
    let can_expand = activity.can_expand && opens_thread.is_none();
    let reasoning = matches!(item.kind, ItemKind::Reasoning).then(|| item.text.clone());
    let fetched = match detail {
        Some(Detail::Loaded(item)) => Some(item.as_ref()),
        _ => None,
    };
    let fetch_error = match detail {
        Some(Detail::Failed(error)) => Some(error.as_str()),
        _ => None,
    };
    // Reads keep their path list; the fetched file contents show as output.
    let is_read = tool_group_action(&activity.work_entry) == ToolGroupAction::Read;
    // Tool calls show the call in the foreground and the result muted below it.
    let shown = fetched.unwrap_or(item);
    let call = match shown.kind {
        ItemKind::WebSearch { .. } if expanded => call_lines(shown),
        _ if expanded && !is_read => call_lines(shown),
        _ => None,
    };
    let failed_exit_code = match (&call, &shown.kind) {
        (
            Some(_),
            ItemKind::CommandExecution {
                exit_code: Some(code),
                ..
            },
        ) if *code != 0 => Some(*code),
        _ => None,
    };
    let full_detail = if expanded && reasoning.is_none() && call.is_none() {
        match fetched {
            Some(fetched) if !is_read => {
                Some(format_item_full_detail(activity.visibility, fetched))
            }
            _ => activity.full_detail.clone(),
        }
    } else {
        None
    };
    let output = if !expanded {
        None
    } else if matches!(shown.kind, ItemKind::WebSearch { .. }) {
        turn_item_output_text(shown)
    } else if let Some(fetched) = fetched {
        Some(turn_item_output_text(fetched).unwrap_or_else(|| "No output.".into()))
    } else if let Some(error) = fetch_error {
        Some(format!("Couldn't load output: {error}"))
    } else if activity.fetches_detail {
        Some("Loading output…".into())
    } else {
        None
    };
    let viewed_image_path = work_entry_viewed_image_path(&activity.work_entry);
    let tool_presentation = resolve_work_entry_tool_presentation(&activity.work_entry, None);
    let preview = work_entry_row_label(&activity.work_entry, false);
    let answer = activity.work_entry.question_answer.as_ref();
    let answer_preview = answer
        .map(question_answer_preview)
        .filter(|preview| !preview.is_empty());
    let accessible_preview = [Some(preview), answer_preview.clone()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    let display_text = work_entry_row_label(&activity.work_entry, expanded);
    let system_notice = matches!(item.kind, ItemKind::SystemNotice { .. });
    let usage_limit = matches!(&item.kind, ItemKind::Error { class: Some(class), .. } if class == "usage_limit")
        && item.status != ItemStatus::Completed;
    let destructive = !system_notice
        && !usage_limit
        && matches!(activity.icon, WorkIcon::Alert | WorkIcon::Warning);
    let failed = activity.status == Some(FeedStatus::Failure);
    let tool_icon = activity.work_entry.tool_icon.clone().or_else(|| {
        activity
            .work_entry
            .tool_source
            .as_ref()
            .and_then(|source| source.icon.clone())
    });
    let icon = if reasoning.is_some() {
        WorkRowIcon::Brain
    } else {
        tool_presentation.map_or(WorkRowIcon::Feed(activity.icon), |presentation| {
            WorkRowIcon::Logo(presentation.icon)
        })
    };
    let shows_detail = expanded
        && (reasoning.is_some()
            || full_detail.is_some()
            || call.is_some()
            || output.is_some()
            || viewed_image_path.is_some()
            || answer.is_some());
    WorkLogRow::Activity(Box::new(WorkActivityRow {
        id: activity.id.clone(),
        role: if opens_thread.is_some() {
            WorkRowRole::Link
        } else if can_expand {
            WorkRowRole::Button
        } else {
            WorkRowRole::None
        },
        can_expand,
        expanded,
        load_detail: expanded && activity.fetches_detail,
        shimmer: activity.live && !expanded,
        label: if system_notice {
            activity.summary.clone()
        } else {
            display_text
        },
        answer_highlighted: !expanded && answer.is_some_and(has_question_answer),
        answer_preview,
        icon,
        failure_mark: failed && tool_icon.is_some(),
        tool_icon,
        icon_tone: if usage_limit {
            WorkIconTone::Warning
        } else if destructive {
            WorkIconTone::Destructive
        } else if failed {
            WorkIconTone::Failed
        } else {
            WorkIconTone::Default
        },
        label_tone: if usage_limit {
            WorkLabelTone::Warning
        } else if destructive {
            WorkLabelTone::Danger
        } else {
            WorkLabelTone::Default
        },
        failed,
        accessibility_label: if failed {
            format!("{accessible_preview}, tool call failed")
        } else {
            accessible_preview
        },
        accessibility_hint: if opens_thread.is_some() {
            "Opens this agent's thread. Long press to copy.".into()
        } else if can_expand {
            format!(
                "Double tap to {} full details. Long press to copy.",
                if expanded { "hide" } else { "show" }
            )
        } else {
            "Long press to copy.".into()
        },
        opens_thread,
        open_label: None,
        copy_text: activity.copy_text.clone(),
        detail: shows_detail.then(|| WorkActivityDetail {
            reasoning,
            call,
            full_detail,
            output,
            failed_exit_code,
            viewed_image_path,
            shows_question_answer: answer.is_some(),
        }),
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum WorkToggleIcon {
    Logo(ToolLogo),
    Surface(ToolSurface),
    Summary(ToolGroupSummaryKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkTogglePresentation {
    pub icon: WorkToggleIcon,
    pub accessibility_label: String,
    pub accessibility_hint: String,
}

pub fn work_toggle_presentation(toggle: &WorkToggle) -> WorkTogglePresentation {
    WorkTogglePresentation {
        icon: match (toggle.summary_tool_icon, toggle.tool_surface) {
            (Some(logo), _) => WorkToggleIcon::Logo(logo),
            (None, Some(surface)) => WorkToggleIcon::Surface(surface),
            (None, None) => WorkToggleIcon::Summary(toggle.summary_kind),
        },
        accessibility_label: if toggle.has_failure {
            format!("{}, tool call failed", toggle.summary)
        } else {
            toggle.summary.clone()
        },
        accessibility_hint: format!(
            "Double tap to {} {} tool {}.",
            if toggle.expanded { "hide" } else { "show" },
            toggle.hidden_count,
            if toggle.hidden_count == 1 {
                "call"
            } else {
                "calls"
            }
        ),
    }
}

/// The timing of one subagent the elapsed time spans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentTiming {
    pub status: ItemStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

/// A group's wall time spans its first launch to its last completion.
pub fn subagent_card_elapsed(agents: &[SubagentTiming], now_ms: i64) -> Option<String> {
    let starts: Vec<i64> = agents
        .iter()
        .filter_map(|agent| agent.started_at.as_ref().map(Timestamp::millis))
        .collect();
    let start = *starts.iter().min()?;
    let live = agents.iter().any(|agent| active_status(agent.status));
    // Settled agents without a completion must not keep counting their age.
    if !live && agents.iter().any(|agent| agent.completed_at.is_none()) {
        return None;
    }
    let end = if live {
        now_ms
    } else {
        agents
            .iter()
            .filter_map(|agent| agent.completed_at.as_ref().map(Timestamp::millis))
            .max()?
    };
    let duration = end - start;
    (duration > 0).then(|| format_duration(duration))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SubagentGroupTone {
    Default,
    Active,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentMember {
    pub link: SubagentLink,
    pub elapsed: Option<String>,
    pub accessibility_hint: String,
}

/// A card of adjacent subagents. More than one draws a collapsible header.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SubagentGroupCard {
    /// Toggles the card in `expanded_work_groups`.
    pub group_id: String,
    pub grouped: bool,
    pub label: String,
    pub summary: String,
    pub accessibility_label: String,
    pub tone: SubagentGroupTone,
    /// Avatars beyond the first three, drawn as "+N".
    pub overflow: u32,
    pub elapsed: Option<String>,
    pub expanded: bool,
    pub shows_members: bool,
    pub members: Vec<SubagentMember>,
}

pub fn subagent_group_card(
    state: &State,
    group_id: &str,
    activities: &[FeedActivity],
    expanded: bool,
    now_ms: i64,
) -> SubagentGroupCard {
    let agents: Vec<(SubagentLink, SubagentTiming)> = activities
        .iter()
        .filter_map(|activity| {
            let item = &activity.item;
            let Some(LifecycleRow::Subagent(link)) = lifecycle_row(state, item, false) else {
                return None;
            };
            let task = subagent_task(state, item);
            let timing = SubagentTiming {
                status: link.live_status,
                started_at: Some(
                    task.map_or_else(|| item.started_at.clone(), |task| task.started_at.clone()),
                ),
                completed_at: task
                    .and_then(|task| task.completed_at.clone())
                    .or_else(|| item.completed_at.clone()),
            };
            Some((link, timing))
        })
        .collect();
    let grouped = agents.len() > 1;
    let label = format!("{} subagents", agents.len());
    let statuses: Vec<ItemStatus> = agents.iter().map(|(_, timing)| timing.status).collect();
    let summary = summarize_subagent_statuses(&statuses);
    let timings: Vec<SubagentTiming> = agents.iter().map(|(_, timing)| timing.clone()).collect();
    SubagentGroupCard {
        group_id: group_id.to_owned(),
        grouped,
        accessibility_label: format!("{label}, {summary}"),
        tone: if statuses.iter().any(|status| active_status(*status)) {
            SubagentGroupTone::Active
        } else if statuses.contains(&ItemStatus::Failed) {
            SubagentGroupTone::Failed
        } else {
            SubagentGroupTone::Default
        },
        overflow: crate::view::count(agents.len().saturating_sub(3)),
        elapsed: subagent_card_elapsed(&timings, now_ms),
        expanded,
        shows_members: !grouped || expanded,
        label,
        summary,
        members: agents
            .into_iter()
            .map(|(link, timing)| SubagentMember {
                accessibility_hint: if link.thread.is_some() {
                    "Opens this agent's thread".into()
                } else {
                    "Provider-managed agent".into()
                },
                elapsed: subagent_card_elapsed(std::slice::from_ref(&timing), now_ms),
                link,
            })
            .collect(),
    }
}

const PROVIDER_DISPLAY_NAMES: [(Driver, &str); 2] =
    [(Driver::Codex, "Codex"), (Driver::Claude, "Claude")];

/// The provider and model as compact metadata, without repeats.
pub fn thread_activity_metadata(
    driver: Option<Driver>,
    instance: Option<&str>,
    model: Option<&str>,
) -> String {
    let provider = match driver {
        Some(driver) => PROVIDER_DISPLAY_NAMES
            .iter()
            .find(|(candidate, _)| *candidate == driver)
            .map(|(_, name)| *name)
            .or(instance),
        None => instance,
    };
    let mut values: Vec<&str> = vec![];
    for value in [provider, model].into_iter().flatten() {
        if !value.is_empty() && !values.contains(&value) {
            values.push(value);
        }
    }
    values.join(" · ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ActivityStatusTone {
    Active,
    Danger,
    Success,
    Warning,
    Neutral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityStatus {
    pub label: String,
    pub tone: ActivityStatusTone,
}

/// The status dot's tone and its accessible label.
pub fn thread_activity_status(status: ItemStatus) -> ActivityStatus {
    let (label, tone) = match status {
        ItemStatus::Completed => ("Completed", ActivityStatusTone::Success),
        ItemStatus::Failed => ("Failed", ActivityStatusTone::Danger),
        ItemStatus::Cancelled => ("Cancelled", ActivityStatusTone::Warning),
        ItemStatus::Interrupted => ("Interrupted", ActivityStatusTone::Warning),
        ItemStatus::Pending => ("Pending", ActivityStatusTone::Active),
        ItemStatus::Running => ("Running", ActivityStatusTone::Active),
        ItemStatus::Waiting => ("Waiting", ActivityStatusTone::Active),
    };
    ActivityStatus {
        label: label.into(),
        tone,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentConnectionPhase {
    Available,
    Offline,
    Connecting,
    Reconnecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadContentPresentation {
    Ready,
    Loading,
    Unavailable { title: String, detail: String },
}

pub fn thread_content_presentation(
    has_detail: bool,
    detail_error: Option<&str>,
    detail_deleted: bool,
    connection: ContentConnectionPhase,
) -> ThreadContentPresentation {
    let unavailable = |title: &str, detail: &str| ThreadContentPresentation::Unavailable {
        title: title.into(),
        detail: detail.into(),
    };
    if has_detail {
        return ThreadContentPresentation::Ready;
    }
    if detail_deleted {
        return unavailable(
            "Thread unavailable",
            "This thread was deleted or is no longer available.",
        );
    }
    if let Some(error) = detail_error {
        return unavailable("Could not load conversation", error);
    }
    match connection {
        // Messages arrive once the (re)connection completes; the composer's
        // connection pill reports the phase.
        ContentConnectionPhase::Connected
        | ContentConnectionPhase::Connecting
        | ContentConnectionPhase::Reconnecting => ThreadContentPresentation::Loading,
        _ => unavailable(
            "Messages not cached",
            "Reconnect this environment to load the conversation.",
        ),
    }
}

/// A file an activity names, opened in the selected thread's workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityFileTarget {
    pub thread: ThreadId,
    pub path: Vec<String>,
    pub line: Option<String>,
}

/// Activity provenance may be a parent thread, but files open in the thread
/// whose workspace is selected.
pub fn activity_file_target(
    current_thread: &ThreadId,
    relative_path: &str,
    line: Option<f64>,
) -> ActivityFileTarget {
    ActivityFileTarget {
        thread: current_thread.clone(),
        path: relative_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect(),
        line: line
            .filter(|line| line.is_finite() && *line > 0.0)
            .map(|line| (line.floor() as i64).to_string()),
    }
}

#[cfg(test)]
#[path = "mobile_work_log_tests.rs"]
mod tests;
