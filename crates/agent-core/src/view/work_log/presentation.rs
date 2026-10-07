//! Work-log presentation both layouts share: tool identity and status labels,
//! failure and success markers, group actions and group summaries.
use super::media_source::{MarkdownImageSource, classify_markdown_image_source, workspace_media};
use super::tool_activity::{ToolActivityAction, classify_tool_activity};
use super::tool_catalog::{
    ToolDefinition, ToolLogo, ToolSummaryAction, resolve_tool_definition, without_completion_suffix,
};
use super::tool_output::tool_output_indicates_failure;
use super::tool_presentation::is_workspace_image_preview_path;
use super::tool_summary::{
    ToolCallOutcome, ToolSummaryCall, summarize_tool_calls, tool_result_indicates_failure,
};
use super::{ItemType, ToolLifecycleStatus, ToolSource, ToolSourceKind, WorkLogEntry, WorkTone};
use crate::js_text::{js_to_fixed, js_trim, utf16_prefix};
use agent_domain::{Item, ItemKind, ItemStatus, Json, ThreadId};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

/// Keys that carry file bodies in provider file-change payloads.
const FILE_BODY_KEYS: [&str; 6] = [
    "diff",
    "old_string",
    "new_string",
    "content",
    "edits",
    "new_source",
];

/// The item for inspection and copying: raw outputs and file bodies removed.
pub fn tool_item_for_display(item: &Item) -> Item {
    let mut display = item.clone();
    match &mut display.kind {
        ItemKind::CommandExecution { .. } => display.text.clear(),
        ItemKind::DynamicTool { output, .. } => {
            *output = None;
            display.text.clear();
        }
        ItemKind::FileChange { changes } => {
            let strip = |value: &Value| match value {
                Value::Object(object) => Value::Object(
                    object
                        .iter()
                        .filter(|(key, _)| !FILE_BODY_KEYS.contains(&key.as_str()))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                ),
                value => value.clone(),
            };
            *changes = Json(match &changes.0 {
                Value::Array(entries) => entries.iter().map(strip).collect(),
                value => strip(value),
            });
            // A failed edit keeps the provider's error where the diff would be.
            if item.status != ItemStatus::Failed || js_trim(&item.text).is_empty() {
                display.text.clear();
            }
        }
        _ => {}
    }
    display
}

/// Compacts a token count to three significant figures with a unit suffix
/// (`19.9B`, `76.7M`, `804K`).
pub fn format_tokens(value: u64) -> String {
    fn trim(value: f64) -> String {
        let digits = if value >= 100.0 {
            0
        } else if value >= 10.0 {
            1
        } else {
            2
        };
        let fixed = js_to_fixed(value, digits);
        match fixed.split_once('.') {
            Some((integer, fraction)) if fraction.bytes().all(|digit| digit == b'0') => {
                integer.into()
            }
            _ => fixed,
        }
    }
    let number = value as f64;
    for (scale, unit) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if number >= scale {
            return format!("{}{unit}", trim(number / scale));
        }
    }
    value.to_string()
}

pub fn context_compaction_label(item: &Item) -> String {
    if item.status == ItemStatus::Running {
        return "Compacting context".into();
    }
    if let ItemKind::Compaction {
        before: Some(before),
        after: Some(after),
    } = item.kind
    {
        return format!(
            "Context compacted {} → {} tokens",
            format_tokens(before),
            format_tokens(after)
        );
    }
    "Context compacted".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGroupAction {
    Read,
    Edit,
    Command,
    ThreadCreate,
    CodeSearch,
    Search,
    Other,
    Update,
}

impl ToolGroupAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Edit => "edit",
            Self::Command => "command",
            Self::ThreadCreate => "thread-create",
            Self::CodeSearch => "code-search",
            Self::Search => "search",
            Self::Other => "other",
            Self::Update => "update",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolGroupSummaryKind {
    Action(ToolGroupAction),
    DynamicTool,
    Reasoning,
    AgentTool,
    ToneTool,
    Mixed,
}

pub fn normalize_compact_tool_label(value: &str) -> String {
    without_completion_suffix(value)
}

fn dynamic_tool_name(entry: &WorkLogEntry) -> Option<&str> {
    match entry.item.as_ref().map(|item| &item.kind) {
        Some(ItemKind::DynamicTool { name, .. }) if !name.is_empty() => Some(name),
        _ => None,
    }
}

/// Structured identity is authoritative, including when it names a foreign server.
fn work_entry_tool_name(entry: &WorkLogEntry) -> &str {
    if let Some(name) = dynamic_tool_name(entry) {
        return name;
    }
    match &entry.tool_title {
        Some(title) if resolve_tool_definition(Some(title)).is_some() => title,
        _ => &entry.label,
    }
}

fn work_entry_tool_definition(entry: &WorkLogEntry) -> Option<ToolDefinition> {
    resolve_tool_definition(Some(work_entry_tool_name(entry)))
}

fn work_entry_tool_output(entry: &WorkLogEntry) -> Option<Value> {
    let item = entry.item.as_ref()?;
    match &item.kind {
        ItemKind::DynamicTool { output, .. } => output.as_ref().map(|output| output.0.clone()),
        ItemKind::CommandExecution { .. } if !item.text.is_empty() => {
            Some(Value::String(item.text.clone()))
        }
        _ => None,
    }
}

/// How a recognized orchestration tool call reads in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkEntryToolPresentation {
    pub display_name: String,
    pub icon: ToolLogo,
}

fn tool_presentation(
    definition: &ToolDefinition,
    status: Option<ToolLifecycleStatus>,
) -> WorkEntryToolPresentation {
    let [action, running, completed, detail] = definition.labels;
    let verb = match status {
        Some(ToolLifecycleStatus::InProgress) => running.to_owned(),
        Some(ToolLifecycleStatus::Completed) => completed.to_owned(),
        Some(ToolLifecycleStatus::Failed) => format!("Failed to {}", action.to_lowercase()),
        Some(ToolLifecycleStatus::Declined) => format!("Declined to {}", action.to_lowercase()),
        Some(ToolLifecycleStatus::Stopped) => format!("Stopped {}", running.to_lowercase()),
        Some(ToolLifecycleStatus::Idle) | None => running.to_owned(),
    };
    WorkEntryToolPresentation {
        display_name: format!("{verb} {detail}"),
        icon: ToolLogo::App,
    }
}

/// Only active or completed calls can inherit the live group's present tense.
pub fn live_activity_tool_status(
    status: Option<ToolLifecycleStatus>,
    present_tense: bool,
) -> ToolLifecycleStatus {
    match status {
        Some(
            status @ (ToolLifecycleStatus::Failed
            | ToolLifecycleStatus::Declined
            | ToolLifecycleStatus::Stopped
            | ToolLifecycleStatus::Idle),
        ) => status,
        _ if present_tense || status == Some(ToolLifecycleStatus::InProgress) => {
            ToolLifecycleStatus::InProgress
        }
        _ => ToolLifecycleStatus::Completed,
    }
}

/// Resolves tool identity before choosing labels or icons in either layout.
pub fn resolve_work_entry_tool_presentation(
    entry: &WorkLogEntry,
    fallback_status: Option<ToolLifecycleStatus>,
) -> Option<WorkEntryToolPresentation> {
    let definition = work_entry_tool_definition(entry)?;
    let status = if tool_result_indicates_failure(work_entry_tool_output(entry).as_ref()) {
        Some(ToolLifecycleStatus::Failed)
    } else {
        entry.tool_lifecycle_status.or(fallback_status)
    };
    Some(tool_presentation(&definition, status))
}

fn work_log_entry_is_tool_like(entry: &WorkLogEntry) -> bool {
    matches!(
        entry.tone,
        WorkTone::Tool | WorkTone::Thinking | WorkTone::Error
    ) || entry
        .command
        .as_deref()
        .is_some_and(|command| !js_trim(command).is_empty())
        || entry.request_kind.is_some()
        || matches!(
            entry.item_type,
            Some(ItemType::CommandExecution | ItemType::FileChange | ItemType::WebSearch)
        )
}

/// The output prefix a command's failure status is read from.
const COMMAND_OUTPUT_STATUS_UNITS: usize = 32_768;

fn work_entry_indicates_tool_failure_from_output(
    entry: &WorkLogEntry,
    include_command: bool,
) -> bool {
    if entry.tone == WorkTone::Error
        || matches!(
            entry.tool_lifecycle_status,
            Some(ToolLifecycleStatus::Failed | ToolLifecycleStatus::Declined)
        )
    {
        return true;
    }
    if !work_log_entry_is_tool_like(entry) {
        return false;
    }
    if work_entry_tool_definition(entry).is_some()
        && tool_result_indicates_failure(work_entry_tool_output(entry).as_ref())
    {
        return true;
    }
    if let Some(item) = &entry.item
        && let ItemKind::CommandExecution { exit_code, .. } = &item.kind
    {
        if item.output_indicates_failure || exit_code.is_some_and(|code| code != 0) {
            return true;
        }
        // A fetched detail carries the output. Read only the preview-sized
        // prefix for status, without exposing it in the row detail.
        if !item.text.is_empty()
            && tool_output_indicates_failure(utf16_prefix(&item.text, COMMAND_OUTPUT_STATUS_UNITS))
        {
            return true;
        }
    }
    let output = if include_command {
        [entry.detail.as_deref(), entry.command.as_deref()]
            .into_iter()
            .flatten()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        entry.detail.clone().unwrap_or_default()
    };
    !output.is_empty() && tool_output_indicates_failure(&output)
}

/// Includes rows whose command field holds error output.
pub fn work_entry_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    work_entry_indicates_tool_failure_from_output(entry, true)
}

/// Checks rendered output without treating the user's command as an error.
pub fn work_entry_display_indicates_tool_failure(entry: &WorkLogEntry) -> bool {
    work_entry_indicates_tool_failure_from_output(entry, false)
}

/// Whether the row can show a success marker.
pub fn work_entry_indicates_tool_success(entry: &WorkLogEntry) -> bool {
    work_log_entry_is_tool_like(entry)
        && !work_entry_indicates_tool_failure(entry)
        && entry.tone != WorkTone::Thinking
        && !matches!(
            entry.tool_lifecycle_status,
            Some(
                ToolLifecycleStatus::Idle
                    | ToolLifecycleStatus::InProgress
                    | ToolLifecycleStatus::Stopped
            )
        )
}

static GREP_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?-u:\b)grep(?-u:\b)").expect("grep pattern compiles"));

fn work_log_entry_is_local_code_search(entry: &WorkLogEntry) -> bool {
    entry.item_type == Some(ItemType::WebSearch)
        && GREP_WORD.is_match(&normalize_compact_tool_label(
            entry.tool_title.as_deref().unwrap_or(&entry.label),
        ))
}

pub fn tool_group_action(entry: &WorkLogEntry) -> ToolGroupAction {
    if entry.item_type == Some(ItemType::ThreadCreated) {
        return ToolGroupAction::ThreadCreate;
    }
    if entry.request_kind.as_deref() == Some("file-read") || entry.viewed_image_path.is_some() {
        return ToolGroupAction::Read;
    }
    // Approvals and questions describe requested work, not work that ran.
    if matches!(
        entry.item_type,
        Some(ItemType::ApprovalRequest | ItemType::UserInputRequest)
    ) {
        return if work_log_entry_is_tool_like(entry) {
            ToolGroupAction::Other
        } else {
            ToolGroupAction::Update
        };
    }
    let tool_name = match entry.item.as_ref().map(|item| &item.kind) {
        Some(ItemKind::DynamicTool { name, .. }) => Some(name.as_str()),
        _ => entry.tool_title.as_deref(),
    }
    .filter(|name| !name.is_empty());
    let data = tool_name.map(|name| json!({ "toolName": name }));
    let classified = classify_tool_activity(entry.item_type, None, data.as_ref());
    if classified == ToolActivityAction::Read {
        return ToolGroupAction::Read;
    }
    if classified == ToolActivityAction::FileChange || entry.item_type == Some(ItemType::FileChange)
    {
        return ToolGroupAction::Edit;
    }
    if classified == ToolActivityAction::Command
        || entry.item_type == Some(ItemType::CommandExecution)
        || entry
            .command
            .as_deref()
            .is_some_and(|command| !command.is_empty())
    {
        return ToolGroupAction::Command;
    }
    if classified == ToolActivityAction::Search {
        return if entry.item_type == Some(ItemType::WebSearch)
            && !work_log_entry_is_local_code_search(entry)
        {
            ToolGroupAction::Search
        } else {
            ToolGroupAction::CodeSearch
        };
    }
    if work_log_entry_is_local_code_search(entry) {
        return ToolGroupAction::CodeSearch;
    }
    if entry.item_type == Some(ItemType::WebSearch) {
        return ToolGroupAction::Search;
    }
    if entry
        .changed_files
        .as_ref()
        .is_some_and(|files| !files.is_empty())
    {
        return ToolGroupAction::Edit;
    }
    if work_log_entry_is_tool_like(entry) {
        ToolGroupAction::Other
    } else {
        ToolGroupAction::Update
    }
}

fn single_line_image_path(path: Option<&str>) -> Option<&str> {
    path.map(js_trim)
        .filter(|path| !path.contains(['\r', '\n']) && is_workspace_image_preview_path(path))
}

/// The image a read or screenshot row previews.
pub fn work_entry_viewed_image_path(entry: &WorkLogEntry) -> Option<String> {
    if let Some(path) = single_line_image_path(entry.viewed_image_path.as_deref()) {
        return Some(path.into());
    }
    let detail = single_line_image_path(entry.detail.as_deref())?;
    (tool_group_action(entry) == ToolGroupAction::Read).then(|| detail.into())
}

/// A Host image the thread's environment serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedImageAsset {
    pub thread: ThreadId,
    pub path: String,
    pub alt: String,
    pub src_fragment: String,
}

pub fn resolve_viewed_image_asset(
    source: &str,
    thread: &ThreadId,
    workspace_root: Option<&str>,
) -> Option<ViewedImageAsset> {
    // A relative path with no known workspace still names a file relative to
    // the thread's workspace, so classify against "." and drop the prefix.
    let MarkdownImageSource::WorkspaceFile { path } =
        classify_markdown_image_source(Some(source), Some(workspace_root.unwrap_or(".")))
    else {
        return None;
    };
    let resolved = match path.strip_prefix("./") {
        Some(relative) if workspace_root.is_none() => relative,
        _ => &path,
    };
    let media = workspace_media(source, resolved)?;
    Some(ViewedImageAsset {
        thread: thread.clone(),
        path: media.path,
        alt: media.name,
        src_fragment: media.src_fragment,
    })
}

fn tool_group_action_count(action: ToolGroupAction, entries: &[&WorkLogEntry]) -> usize {
    if action != ToolGroupAction::Edit {
        return entries.len();
    }
    let mut changed_files = std::collections::HashSet::new();
    let mut edits_without_file_details = 0;
    for entry in entries {
        match &entry.changed_files {
            Some(files) if !files.is_empty() => changed_files.extend(files),
            _ => edits_without_file_details += 1,
        }
    }
    changed_files.len() + edits_without_file_details
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

fn tool_group_action_label(action: ToolGroupAction, count: usize) -> String {
    match action {
        ToolGroupAction::Read => format!("Read {}", plural(count, "file", "files")),
        ToolGroupAction::Edit => format!("Changed {}", plural(count, "file", "files")),
        ToolGroupAction::Command => format!("Ran {}", plural(count, "command", "commands")),
        ToolGroupAction::ThreadCreate => format!("Created {}", plural(count, "thread", "threads")),
        ToolGroupAction::Search => format!("Searched the web {}", plural(count, "time", "times")),
        ToolGroupAction::CodeSearch => format!("Searched code {}", plural(count, "time", "times")),
        ToolGroupAction::Other => format!("Used {}", plural(count, "tool", "tools")),
        ToolGroupAction::Update => format!("Received {}", plural(count, "update", "updates")),
    }
}

fn tool_summary_call(entry: &WorkLogEntry) -> ToolSummaryCall {
    let input = entry.item.as_ref().and_then(|item| match &item.kind {
        ItemKind::DynamicTool { input, .. } => Some(input.0.clone()),
        ItemKind::CommandExecution { command, .. } => Some(Value::String(command.clone())),
        _ => None,
    });
    ToolSummaryCall {
        input,
        output: work_entry_tool_output(entry),
        outcome: match entry.tool_lifecycle_status {
            Some(ToolLifecycleStatus::Failed | ToolLifecycleStatus::Declined) => {
                ToolCallOutcome::Failed
            }
            _ if entry.tone == WorkTone::Error => ToolCallOutcome::Failed,
            Some(ToolLifecycleStatus::Completed) => ToolCallOutcome::Completed,
            _ => ToolCallOutcome::Unfinished,
        },
    }
}

/// Group keys are the reference action names, so a created-thread row and a
/// thread creation tool share the "thread-create" group.
fn summary_action_priority(key: &str) -> u8 {
    match key {
        "command" | "edit" | "delegate" | "task-cancel" | "thread-create" | "thread-send"
        | "thread-interrupt" | "thread-configure" | "thread-fork" | "thread-merge"
        | "thread-organize" | "thread-update" | "queue-edit" | "queue-cancel" | "queue-reorder"
        | "queue-steer" | "question-respond" | "project-create" => 0,
        "other" | "update" => 2,
        _ => 1,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolGroupSummary {
    pub summary: String,
    pub has_failure: bool,
}

struct ActionGroup<'a> {
    key: &'static str,
    action: ToolGroupAction,
    summary_action: Option<ToolSummaryAction>,
    entries: Vec<&'a WorkLogEntry>,
}

fn sentence(labels: &[String]) -> String {
    match labels {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [init @ .., last] => format!("{}, and {last}", init.join(", ")),
    }
}

fn lowercase_first(label: &str) -> String {
    let mut chars = label.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// Summarizes at most two action categories; every omitted call still counts
/// in the remainder.
pub fn summarize_tool_group(entries: &[WorkLogEntry]) -> ToolGroupSummary {
    let tool_entries: Vec<&WorkLogEntry> = entries
        .iter()
        .filter(|entry| entry.item_type != Some(ItemType::Reasoning))
        .collect();
    if !entries.is_empty() && tool_entries.is_empty() {
        return ToolGroupSummary {
            summary: if entries.len() == 1 {
                "Thought".into()
            } else {
                format!("Thought (×{})", entries.len())
            },
            has_failure: false,
        };
    }
    let mut groups: Vec<ActionGroup> = vec![];
    let mut sources: Vec<&ToolSource> = vec![];
    let mut sourced_count = 0;
    for entry in &tool_entries {
        let summary_action =
            work_entry_tool_definition(entry).map(|definition| definition.summary_action);
        if let (Some(source), None) = (&entry.tool_source, summary_action) {
            sourced_count += 1;
            match sources.iter_mut().find(|known| known.key == source.key) {
                Some(known) => *known = source,
                None => sources.push(source),
            }
            continue;
        }
        let action = tool_group_action(entry);
        let key = summary_action.map_or(action.as_str(), ToolSummaryAction::as_str);
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => group.entries.push(entry),
            None => groups.push(ActionGroup {
                key,
                action,
                summary_action,
                entries: vec![entry],
            }),
        }
    }
    struct Summary {
        index: usize,
        count: usize,
        priority: u8,
        label: String,
        failed_count: usize,
    }
    let summaries: Vec<Summary> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let (label, failed_count) = match group.summary_action {
                Some(summary_action) => {
                    let calls: Vec<_> =
                        group.entries.iter().map(|e| tool_summary_call(e)).collect();
                    let summary = summarize_tool_calls(summary_action, &calls);
                    (summary.label, summary.failed_count)
                }
                None => (
                    tool_group_action_label(
                        group.action,
                        tool_group_action_count(group.action, &group.entries),
                    ),
                    group
                        .entries
                        .iter()
                        .filter(|entry| work_entry_display_indicates_tool_failure(entry))
                        .count(),
                ),
            };
            Summary {
                index,
                count: group.entries.len(),
                priority: summary_action_priority(group.key),
                label,
                failed_count,
            }
        })
        .collect();
    let mut selected: Vec<&Summary> = summaries.iter().collect();
    selected.sort_by_key(|summary| (summary.priority, summary.index));
    selected.truncate(2);
    selected.sort_by_key(|summary| summary.index);
    let mut labels: Vec<String> = selected
        .iter()
        .map(|summary| summary.label.clone())
        .collect();
    if !sources.is_empty() {
        let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
        let formatted = match names.as_slice() {
            [only] => (*only).to_owned(),
            [first, second] => format!("{first} and {second}"),
            [init @ .., last] => format!("{}, and {last}", init.join(", ")),
            [] => unreachable!("sources is not empty"),
        };
        let all_integrations = sources
            .iter()
            .all(|source| source.kind == ToolSourceKind::Integration);
        let suffix = if !all_integrations {
            ""
        } else if sources.len() == 1 {
            " integration"
        } else {
            " integrations"
        };
        labels.insert(0, format!("Used {formatted}{suffix}"));
    }
    let remaining = tool_entries.len().saturating_sub(
        sourced_count + selected.iter().map(|summary| summary.count).sum::<usize>(),
    );
    if remaining > 0 {
        labels.push(format!(
            "Performed {remaining} other {}",
            if remaining == 1 { "action" } else { "actions" }
        ));
    }
    let labels: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            if index == 0 {
                label.clone()
            } else {
                lowercase_first(label)
            }
        })
        .collect();
    ToolGroupSummary {
        summary: sentence(&labels),
        has_failure: summaries.iter().any(|summary| summary.failed_count > 0),
    }
}

pub fn tool_group_summary_kind(entries: &[WorkLogEntry]) -> ToolGroupSummaryKind {
    let tool_entries: Vec<&WorkLogEntry> = entries
        .iter()
        .filter(|entry| entry.item_type != Some(ItemType::Reasoning))
        .collect();
    if !entries.is_empty() && tool_entries.is_empty() {
        return ToolGroupSummaryKind::Reasoning;
    }
    let mut actions: Vec<ToolGroupAction> = tool_entries
        .iter()
        .map(|entry| tool_group_action(entry))
        .collect();
    actions.sort_by_key(|action| action.as_str());
    actions.dedup();
    let [action] = actions[..] else {
        return ToolGroupSummaryKind::Mixed;
    };
    if action != ToolGroupAction::Other {
        return ToolGroupSummaryKind::Action(action);
    }
    let kinds: Vec<ToolGroupSummaryKind> = tool_entries
        .iter()
        .map(|entry| match (entry.item_type, entry.tone) {
            (Some(ItemType::DynamicTool), _) => ToolGroupSummaryKind::DynamicTool,
            (Some(ItemType::Subagent), _) | (_, WorkTone::Thinking) => {
                ToolGroupSummaryKind::AgentTool
            }
            (_, WorkTone::Tool) => ToolGroupSummaryKind::ToneTool,
            _ => ToolGroupSummaryKind::Action(ToolGroupAction::Other),
        })
        .collect();
    if kinds.iter().all(|kind| *kind == kinds[0]) {
        kinds[0]
    } else {
        ToolGroupSummaryKind::Mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::work_log::fixtures::*;
    use crate::view::work_log::tool_catalog::tool_names;
    use ToolLifecycleStatus as Status;
    use rstest::rstest;

    fn base() -> WorkLogEntry {
        entry("w1")
            .created("2026-01-01T00:00:00.000Z")
            .label("Read")
    }

    fn command_item() -> Item {
        command("command", "rg \"command not found\"")
    }

    fn orchestration_tool(id: &str, tool: &str, output: Option<Value>) -> Item {
        dynamic_tool(id, &format!("orchestration.{tool}"), json!({}), output)
    }

    fn label(label: &str) -> WorkLogEntry {
        entry("label").label(label)
    }

    fn display_name(entry: &WorkLogEntry, fallback: Option<Status>) -> Option<String> {
        resolve_work_entry_tool_presentation(entry, fallback).map(|p| p.display_name)
    }

    #[rstest]
    #[case::output_indicates_failure(command_item().output_indicates_failure(), true)]
    #[case::exit_code(command_item().exit_code(Some(2)), true)]
    #[case::failing_output(command_item().text("sh: missing-command: command not found"), true)]
    #[case::passing_output(command_item().text("Found 3 matches"), false)]
    #[case::failure_beyond_the_status_prefix(
        command_item().text(&format!("{} command not found", "x".repeat(32_768))),
        false
    )]
    #[case::plain(command_item(), false)]
    fn preserves_command_failure_state_after_removing_displayed_output(
        #[case] item: Item,
        #[case] failed: bool,
    ) {
        let entry = base()
            .tone(WorkTone::Tool)
            .item_type(ItemType::CommandExecution)
            .status(Status::Completed)
            .item(item.clone());
        assert_eq!(work_entry_display_indicates_tool_failure(&entry), failed);
        assert_eq!(work_entry_indicates_tool_success(&entry), !failed);
        assert_eq!(tool_item_for_display(&item).text, "");
    }

    #[test]
    fn is_true_for_error_tone() {
        assert!(work_entry_indicates_tool_failure(
            &base().tone(WorkTone::Error).detail("nothing special")
        ));
    }

    #[test]
    fn is_true_when_lifecycle_says_failed_even_if_detail_is_empty() {
        assert!(work_entry_indicates_tool_failure(
            &base().tone(WorkTone::Tool).status(Status::Failed)
        ));
    }

    #[test]
    fn detects_file_not_found_style_tool_output_with_completed_lifecycle() {
        assert!(work_entry_indicates_tool_failure(
            &base()
                .tone(WorkTone::Tool)
                .status(Status::Completed)
                .detail("File not found: C:\\foo\\nonexistent.ts")
        ));
    }

    #[test]
    fn detects_glob_no_files_and_powershell_command_errors() {
        assert!(work_entry_indicates_tool_failure(
            &base()
                .label("Glob")
                .tone(WorkTone::Tool)
                .detail("No files found")
        ));
        assert!(work_entry_indicates_tool_failure(
            &base().label("Bash").tone(WorkTone::Tool).detail(
                "The term 'this_is_not_a_command' is not recognized as the name of a cmdlet, function, script file, or operable program."
            )
        ));
    }

    #[test]
    fn is_false_for_successful_completed_tools() {
        assert!(!work_entry_indicates_tool_failure(
            &base()
                .tone(WorkTone::Tool)
                .status(Status::Completed)
                .detail("Found 3 matching files")
        ));
    }

    #[test]
    fn does_not_treat_error_text_in_a_command_as_rendered_failure() {
        let entry = base()
            .label("Ran command")
            .tone(WorkTone::Tool)
            .status(Status::Completed)
            .command("rg \"file not found\"")
            .detail("Found 3 matches");
        assert!(!work_entry_display_indicates_tool_failure(&entry));
        // Rows can hold output in the command field, so that path stays separate.
        assert!(work_entry_indicates_tool_failure(&entry));
        assert!(work_entry_display_indicates_tool_failure(
            &entry.detail("File not found")
        ));
    }

    #[test]
    fn treats_successful_tool_rows_as_success_candidates() {
        assert!(work_entry_indicates_tool_success(
            &base()
                .tone(WorkTone::Tool)
                .status(Status::Completed)
                .detail("ok")
        ));
        assert!(!work_entry_indicates_tool_success(
            &base()
                .tone(WorkTone::Tool)
                .status(Status::InProgress)
                .detail("…")
        ));
        assert!(!work_entry_indicates_tool_success(
            &base().tone(WorkTone::Thinking).detail("…")
        ));
        assert!(!work_entry_indicates_tool_success(
            &base().tone(WorkTone::Tool).status(Status::Stopped)
        ));
        assert!(!work_entry_indicates_tool_success(
            &base().tone(WorkTone::Tool).status(Status::Idle)
        ));
    }

    #[test]
    fn does_not_run_heuristics_on_non_tool_info_rows() {
        assert!(!work_entry_indicates_tool_failure(
            &base()
                .label("Context compacted")
                .tone(WorkTone::Info)
                .detail("File not found in conversation")
        ));
    }

    fn summary(entries: &[WorkLogEntry]) -> String {
        summarize_tool_group(entries).summary
    }

    #[test]
    fn excludes_reasoning_from_mixed_tool_counts_and_icons() {
        let thought = entry("thought")
            .item_type(ItemType::Reasoning)
            .tone(WorkTone::Thinking)
            .detail("Check the source");
        let mut second_thought = thought.clone();
        second_thought.id = "thought-2".into();
        let command = entry("command")
            .item_type(ItemType::CommandExecution)
            .command("vp test run");
        assert_eq!(
            summarize_tool_group(&[thought.clone(), command.clone(), second_thought.clone()]),
            ToolGroupSummary {
                summary: "Ran 1 command".into(),
                has_failure: false,
            }
        );
        assert_eq!(
            tool_group_summary_kind(&[thought.clone(), command]),
            ToolGroupSummaryKind::Action(ToolGroupAction::Command)
        );
        assert_eq!(summary(std::slice::from_ref(&thought)), "Thought");
        assert_eq!(summary(&[thought.clone(), second_thought]), "Thought (×2)");
        assert_eq!(
            tool_group_summary_kind(&[thought]),
            ToolGroupSummaryKind::Reasoning
        );
    }

    #[test]
    fn counts_created_threads_alongside_adjacent_commands() {
        assert_eq!(
            summary(&[
                entry("command")
                    .item_type(ItemType::CommandExecution)
                    .command("vp test run"),
                entry("created")
                    .item_type(ItemType::ThreadCreated)
                    .label("Created thread"),
            ]),
            "Ran 1 command and created 1 thread"
        );
    }

    #[test]
    fn deduplicates_named_sources_ahead_of_ordinary_actions() {
        let source = |entry: WorkLogEntry| {
            entry.source("browser-use:chrome", "Chrome", ToolSourceKind::Integration)
        };
        assert_eq!(
            summary(&[
                source(entry("open").label("Open page")),
                source(entry("inspect").label("Inspect page")),
                entry("command")
                    .label("Ran command")
                    .item_type(ItemType::CommandExecution)
                    .command("git status"),
            ]),
            "Used Chrome integration and ran 1 command"
        );
    }

    #[test]
    fn omits_the_integration_suffix_for_special_browser_and_computer_sources() {
        assert_eq!(
            summary(&[
                entry("inspect").label("Inspect page").source(
                    "browser-use",
                    "Browser",
                    ToolSourceKind::Browser
                ),
                entry("click").label("Click").source(
                    "computer-use",
                    "Computer Use",
                    ToolSourceKind::Computer
                ),
            ]),
            "Used Browser and Computer Use"
        );
    }

    #[test]
    fn presents_and_summarizes_every_tool_using_the_same_structured_identity() {
        let used = Regex::new(r"Used (?:1 tool|Orchestration integration)").unwrap();
        for tool in tool_names() {
            let entry = entry(tool)
                .label("Custom provider title")
                .status(Status::Completed)
                .item_type(ItemType::DynamicTool)
                .source(
                    "orchestration",
                    "Orchestration",
                    ToolSourceKind::Integration,
                )
                .item(orchestration_tool(tool, tool, None));
            let presentation = resolve_work_entry_tool_presentation(&entry, None);
            let name = presentation.expect(tool).display_name;
            assert!(!name.contains(tool), "{tool}");
            let group = summarize_tool_group(std::slice::from_ref(&entry));
            assert!(!used.is_match(&group.summary), "{tool}");
            assert!(!group.has_failure, "{tool}");
            let failed = entry.status(Status::Failed);
            assert!(
                display_name(&failed, None)
                    .unwrap()
                    .starts_with("Failed to "),
                "{tool}"
            );
            let group = summarize_tool_group(&[failed]);
            assert!(group.has_failure, "{tool}");
            assert!(
                group.summary.starts_with("Tried to ")
                    || group.summary.starts_with("Requested thread creation"),
                "{tool}"
            );
        }
    }

    #[rstest]
    #[case("project_list", "Listing projects", "Listed projects")]
    #[case("project_create", "Registering a project", "Registered a project")]
    #[case(
        "thread_launch",
        "Launching a project thread",
        "Launched a project thread"
    )]
    #[case("queue_edit", "Editing a queued message", "Edited a queued message")]
    #[case(
        "pending_request_respond",
        "Answering pending questions",
        "Answered pending questions"
    )]
    #[case("thread_configure", "Setting thread model", "Set thread model")]
    #[case(
        "thread_fork",
        "Forking this thread",
        "Requested a fork of this thread"
    )]
    fn labels_through_its_lifecycle(
        #[case] tool: &str,
        #[case] running: &str,
        #[case] completed: &str,
    ) {
        let entry = label(&format!("Orchestration.{tool}"));
        assert_eq!(display_name(&entry, None).as_deref(), Some(running));
        assert_eq!(
            display_name(&entry.status(Status::Completed), None).as_deref(),
            Some(completed)
        );
    }

    #[test]
    fn summarizes_project_tools_from_mcp_arguments_and_results_without_claiming_failed_effects() {
        let create = entry("create")
            .label("Custom title")
            .item_type(ItemType::DynamicTool)
            .status(Status::Completed)
            .item(dynamic_tool(
                "create",
                "orchestration.project_create",
                json!({"title": "Repo", "workspaceRoot": "/tmp/repo"}),
                Some(json!({"id": "project-1"})),
            ));
        let list = create
            .clone()
            .item(orchestration_tool("list", "project_list", None));
        assert_eq!(
            summarize_tool_group(&[list, create.clone()]),
            ToolGroupSummary {
                summary: "Listed projects 1 time and registered 1 project".into(),
                has_failure: false,
            }
        );
        let failed = create.clone().item(orchestration_tool(
            "failed",
            "project_create",
            Some(json!({"isError": true})),
        ));
        assert_eq!(
            summarize_tool_group(&[create, failed]),
            ToolGroupSummary {
                summary: "Registered 1 project".into(),
                has_failure: true,
            }
        );
    }

    #[test]
    fn does_not_summarize_a_foreign_structured_identity_as_orchestration_work() {
        let entry = entry("foreign")
            .label("project_create")
            .status(Status::Completed)
            .item(dynamic_tool(
                "foreign",
                "another-server.project_create",
                json!({}),
                None,
            ));
        assert_eq!(summary(&[entry]), "Used 1 tool");
    }

    #[test]
    fn shows_returned_mcp_errors_as_failures_even_in_the_live_activity_row() {
        let entry = entry("create")
            .label("Orchestration.project_create")
            .status(Status::InProgress)
            .item_type(ItemType::DynamicTool)
            .item(orchestration_tool(
                "create",
                "project_create",
                Some(json!({"isError": true})),
            ));
        assert_eq!(
            display_name(&entry, None).as_deref(),
            Some("Failed to register a project")
        );
        assert!(work_entry_display_indicates_tool_failure(&entry));
        assert!(!work_entry_indicates_tool_success(&entry));
        let child_failure = entry
            .label("Orchestration.task_status")
            .status(Status::Completed)
            .item(orchestration_tool(
                "status",
                "task_status",
                Some(
                    json!({"taskId": "child", "status": "failed", "summary": "command not found"}),
                ),
            ));
        assert!(!work_entry_display_indicates_tool_failure(&child_failure));
        assert!(work_entry_indicates_tool_success(&child_failure));
    }

    #[test]
    fn uses_structured_mcp_identity_when_the_provider_supplies_a_custom_title() {
        let entry = label("Tool call complete")
            .tool_title("Inspect the current page")
            .item(orchestration_tool(
                "read",
                "thread_read",
                Some(json!({"title": "Example"})),
            ));
        assert_eq!(
            resolve_work_entry_tool_presentation(&entry, None),
            Some(WorkEntryToolPresentation {
                display_name: "Reading a thread".into(),
                icon: ToolLogo::App,
            })
        );
    }

    #[rstest]
    #[case(Some(Status::InProgress), "Reading a thread")]
    #[case(Some(Status::Completed), "Read a thread")]
    #[case(Some(Status::Failed), "Failed to read a thread")]
    #[case(Some(Status::Declined), "Declined to read a thread")]
    #[case(Some(Status::Stopped), "Stopped reading a thread")]
    #[case(None, "Reading a thread")]
    fn describes_the_tools_own_state(#[case] status: Option<Status>, #[case] name: &str) {
        let mut entry = label("Orchestration.thread_read");
        entry.tool_lifecycle_status = status;
        assert_eq!(
            resolve_work_entry_tool_presentation(&entry, None),
            Some(WorkEntryToolPresentation {
                display_name: name.into(),
                icon: ToolLogo::App,
            })
        );
    }

    #[test]
    fn uses_the_summarys_state_only_when_the_provider_omitted_a_lifecycle_status() {
        let entry = label("Orchestration.thread_read");
        assert_eq!(
            display_name(&entry, Some(Status::InProgress)).as_deref(),
            Some("Reading a thread")
        );
        assert_eq!(
            display_name(&entry, Some(Status::Completed)).as_deref(),
            Some("Read a thread")
        );
        assert_eq!(
            display_name(
                &entry.clone().status(Status::Completed),
                Some(Status::InProgress)
            )
            .as_deref(),
            Some("Read a thread")
        );
        assert_eq!(
            display_name(&entry.status(Status::Failed), Some(Status::Completed)).as_deref(),
            Some("Failed to read a thread")
        );
    }

    #[rstest]
    #[case("thread_read", "Reading a thread", "Read a thread")]
    #[case("thread_send", "Sending to a thread", "Sent to a thread")]
    fn preserves_verb_forms_and_the_rest_of_the_label(
        #[case] tool: &str,
        #[case] running: &str,
        #[case] completed: &str,
    ) {
        let entry = label(&format!("orchestration.{tool}"));
        assert_eq!(
            display_name(&entry.clone().status(Status::InProgress), None).as_deref(),
            Some(running)
        );
        assert_eq!(
            display_name(&entry.status(Status::Completed), None).as_deref(),
            Some(completed)
        );
    }

    #[test]
    fn keeps_app_branding_for_non_browser_tools_and_falls_back_to_the_original_tool_label() {
        assert_eq!(
            resolve_work_entry_tool_presentation(
                &label("mcp__orchestration__task_status").tool_title("Check the child task"),
                None
            ),
            Some(WorkEntryToolPresentation {
                display_name: "Getting delegated task status".into(),
                icon: ToolLogo::App,
            })
        );
    }

    #[test]
    fn does_not_brand_unknown_tools_or_another_servers_matching_tool_name() {
        for name in [
            "mcp__github__thread_read",
            "orchestration.unknown_tool",
            "orchestration.toString",
            "Search files",
        ] {
            assert_eq!(
                resolve_work_entry_tool_presentation(&label(name), None),
                None,
                "{name}"
            );
        }
        assert_eq!(
            resolve_work_entry_tool_presentation(
                &label("thread_read").item(dynamic_tool(
                    "foreign",
                    "another-server.thread_read",
                    json!({}),
                    None
                )),
                None
            ),
            None
        );
    }

    #[test]
    fn returns_a_single_image_path_from_supported_read_entries() {
        let read = entry("entry-1").label("Read");
        assert_eq!(
            work_entry_viewed_image_path(
                &read
                    .clone()
                    .request_kind("file-read")
                    .detail(" assets/a.png ")
            )
            .as_deref(),
            Some("assets/a.png")
        );
        assert_eq!(
            work_entry_viewed_image_path(
                &read
                    .clone()
                    .item_type(ItemType::DynamicTool)
                    .tool_title("Read file")
                    .detail("C:\\workspace\\a.webp")
            )
            .as_deref(),
            Some("C:\\workspace\\a.webp")
        );
        assert_eq!(
            work_entry_viewed_image_path(
                &read
                    .item_type(ItemType::DynamicTool)
                    .detail("Read: {\"file_path\":\"truncated...\"}")
                    .viewed_image(" /workspace/reference image.webp ")
            )
            .as_deref(),
            Some("/workspace/reference image.webp")
        );
    }

    #[test]
    fn rejects_non_image_multi_line_and_non_read_details() {
        let read = entry("entry-1").label("Read");
        assert_eq!(
            work_entry_viewed_image_path(&read.clone().request_kind("file-read").detail("a.txt")),
            None
        );
        assert_eq!(
            work_entry_viewed_image_path(
                &read
                    .clone()
                    .request_kind("file-read")
                    .detail("a.png\nb.png")
            ),
            None
        );
        assert_eq!(work_entry_viewed_image_path(&read.detail("a.png")), None);
    }

    #[test]
    fn groups_legacy_claude_image_reads_with_other_reads() {
        assert_eq!(
            tool_group_action(
                &entry("legacy-read")
                    .item_type(ItemType::DynamicTool)
                    .viewed_image("/workspace/reference.png")
            ),
            ToolGroupAction::Read
        );
    }

    #[test]
    fn groups_claude_read_and_grep_from_tool_name_not_file_contents() {
        assert_eq!(
            tool_group_action(
                &entry("read")
                    .label("Read")
                    .item_type(ItemType::DynamicTool)
                    .tool_title("Read")
                    .item(dynamic_tool(
                        "read",
                        "Read",
                        json!({"file_path": "src/env.ts"}),
                        None
                    ))
            ),
            ToolGroupAction::Read
        );
        assert_eq!(
            tool_group_action(
                &entry("grep")
                    .label("Grep")
                    .item_type(ItemType::DynamicTool)
                    .tool_title("Grep")
                    .item(dynamic_tool(
                        "grep",
                        "Grep",
                        json!({"pattern": "TODO", "path": "apps/web"}),
                        None
                    ))
            ),
            ToolGroupAction::CodeSearch
        );
    }

    #[test]
    fn serves_attachment_paths_in_place_like_any_other_host_path() {
        let path = "/Users/demo/.agent/dev/attachments/11111111-1111-4111-8111-111111111111.png";
        assert_eq!(
            resolve_viewed_image_asset(path, &thread_id(), Some("/workspace")),
            Some(ViewedImageAsset {
                thread: thread_id(),
                path: path.into(),
                alt: "11111111-1111-4111-8111-111111111111.png".into(),
                src_fragment: String::new(),
            })
        );
    }

    #[test]
    fn normalizes_workspace_image_sources() {
        assert_eq!(
            resolve_viewed_image_asset(
                "screens/logo.svg?v=2#mark",
                &thread_id(),
                Some("/workspace")
            ),
            Some(ViewedImageAsset {
                thread: thread_id(),
                path: "/workspace/screens/logo.svg".into(),
                alt: "logo.svg".into(),
                src_fragment: "#mark".into(),
            })
        );
        assert_eq!(
            resolve_viewed_image_asset("https://example.com/logo.png", &thread_id(), None),
            None
        );
    }

    #[test]
    fn resolves_relative_images_without_a_known_workspace() {
        assert_eq!(
            resolve_viewed_image_asset("shots/a.png:3", &thread_id(), None)
                .map(|asset| (asset.path, asset.alt)),
            Some(("shots/a.png".into(), "a.png".into()))
        );
        assert_eq!(
            resolve_viewed_image_asset("notes.txt", &thread_id(), None),
            None
        );
    }

    #[test]
    fn labels_context_compaction_with_compact_token_counts() {
        assert_eq!(
            context_compaction_label(&compaction("c", Some(1_234_567), Some(98_000))),
            "Context compacted 1.23M → 98K tokens"
        );
        assert_eq!(
            context_compaction_label(&compaction("c", Some(1), None)),
            "Context compacted"
        );
        assert_eq!(
            context_compaction_label(
                &compaction("c", Some(1), Some(1)).status(ItemStatus::Running)
            ),
            "Compacting context"
        );
    }

    #[test]
    fn formats_tokens_like_javascript_to_fixed() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1K");
        assert_eq!(format_tokens(1_500), "1.50K");
        assert_eq!(format_tokens(1_005), "1K");
        assert_eq!(format_tokens(99_950), "100K");
        assert_eq!(format_tokens(100_500), "101K");
        assert_eq!(format_tokens(804_000), "804K");
        assert_eq!(format_tokens(19_900_000_000), "19.9B");
        assert_eq!(format_tokens(2_000_000_000_000), "2T");
    }

    #[test]
    fn keeps_failed_edit_errors_but_drops_file_bodies_for_display() {
        let changes = json!([{"path": "a.rs", "kind": "update", "diff": "+secret"}]);
        let failed = file_change("edit", changes.clone())
            .status(ItemStatus::Failed)
            .text("Permission denied");
        let display = tool_item_for_display(&failed);
        assert_eq!(display.text, "Permission denied");
        assert_eq!(
            display.kind,
            ItemKind::FileChange {
                changes: Json(json!([{"path": "a.rs", "kind": "update"}])),
            }
        );
        assert_eq!(
            tool_item_for_display(&file_change("edit", changes).text("diff")).text,
            ""
        );
        let tool = tool_item_for_display(&dynamic_tool("t", "Read", json!({}), Some(json!("x"))));
        assert!(matches!(
            tool.kind,
            ItemKind::DynamicTool { output: None, .. }
        ));
    }

    #[test]
    fn counts_changed_files_once_across_edits() {
        let edit = |id: &str| entry(id).item_type(ItemType::FileChange);
        assert_eq!(
            summary(&[
                edit("a").changed_files(&["a.rs", "b.rs"]),
                edit("b").changed_files(&["a.rs"]),
                edit("c"),
            ]),
            "Changed 3 files"
        );
    }

    #[test]
    fn keeps_failed_and_stopped_statuses_out_of_the_present_tense() {
        assert_eq!(
            live_activity_tool_status(Some(Status::Failed), true),
            Status::Failed
        );
        assert_eq!(live_activity_tool_status(None, true), Status::InProgress);
        assert_eq!(
            live_activity_tool_status(Some(Status::Completed), true),
            Status::InProgress
        );
        assert_eq!(live_activity_tool_status(None, false), Status::Completed);
    }

    #[test]
    fn falls_back_to_the_entry_kind_when_every_call_is_another_tool() {
        let tool = |id: &str| entry(id).item_type(ItemType::DynamicTool);
        assert_eq!(
            tool_group_summary_kind(&[tool("a"), tool("b")]),
            ToolGroupSummaryKind::DynamicTool
        );
        assert_eq!(
            tool_group_summary_kind(&[tool("a"), entry("b").item_type(ItemType::Subagent)]),
            ToolGroupSummaryKind::Mixed
        );
        assert_eq!(tool_group_summary_kind(&[]), ToolGroupSummaryKind::Mixed);
        let notice = entry("n").tone(WorkTone::Info);
        assert_eq!(
            tool_group_summary_kind(std::slice::from_ref(&notice)),
            ToolGroupSummaryKind::Action(ToolGroupAction::Update)
        );
    }
}
