//! Desktop work-log row labels: the collapsed heading of a call, its live
//! present-tense form, and which calls an expanded group lists.
use crate::js_text::{JS_SPACE, is_js_space, js_trim, utf16_len};
use crate::presentation::markdown::links::{
    file_basename, format_file_path_position, is_windows_absolute_path, split_file_path_position,
    strip_slash_prefixed_windows_drive,
};
use crate::view::work_log::{
    ItemType, SourceActivity, ToolLifecycleStatus, WorkLogEntry, WorkTone,
    command_label::{command_display_text, command_program_name},
    presentation::{
        ToolGroupAction, live_activity_tool_status, normalize_compact_tool_label,
        resolve_work_entry_tool_presentation, tool_group_action, work_entry_indicates_tool_failure,
    },
    tool_activity::{
        collect_tool_file_paths, dynamic_tool_title, format_read_tool_label,
        format_search_tool_label,
    },
};
use agent_domain::ItemKind;
use regex::Regex;
use serde_json::{Value, json};
use std::sync::LazyLock;

static JS_SPACES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{JS_SPACE}+")).expect("space pattern compiles"));

/// JavaScript `text.trim().replace(/\s+/g, " ")`.
pub(super) fn collapse_whitespace(text: &str) -> String {
    JS_SPACES.replace_all(js_trim(text), " ").into_owned()
}

fn collapsed_detail(entry: &WorkLogEntry) -> Option<String> {
    entry
        .detail
        .as_deref()
        .map(collapse_whitespace)
        .filter(|detail| !detail.is_empty())
}

/// A row that presents a tool call, thought or failure rather than a status update.
pub(super) fn entry_is_tool_like(entry: &WorkLogEntry) -> bool {
    matches!(
        entry.tone,
        WorkTone::Tool | WorkTone::Thinking | WorkTone::Error
    ) || entry.command.is_some()
        || entry.request_kind.is_some()
}

pub(super) fn entry_indicates_tool_success(entry: &WorkLogEntry) -> bool {
    if !entry_is_tool_like(entry)
        || work_entry_indicates_tool_failure(entry)
        || (entry.tone == WorkTone::Thinking && entry.item_type != Some(ItemType::Reasoning))
    {
        return false;
    }
    !matches!(
        entry.tool_lifecycle_status,
        Some(
            ToolLifecycleStatus::Failed
                | ToolLifecycleStatus::Declined
                | ToolLifecycleStatus::InProgress
                | ToolLifecycleStatus::Stopped
                | ToolLifecycleStatus::Idle
        )
    )
}

/// A tool-like row with neither a clear success nor a failure.
fn entry_indicates_tool_neutral_status(entry: &WorkLogEntry) -> bool {
    entry_is_tool_like(entry)
        && !work_entry_indicates_tool_failure(entry)
        && !entry_indicates_tool_success(entry)
}

/// The structured data a call carries besides its input: a dynamic tool's
/// input and output, or a file change's change list. Other item kinds carry no
/// paths or search terms.
fn tool_data(entry: &WorkLogEntry) -> Option<Value> {
    match &entry.item.as_ref()?.kind {
        ItemKind::DynamicTool { input, output, .. } => Some(json!({
            "input": input.0,
            "output": output.as_ref().map_or(Value::Null, |output| output.0.clone()),
        })),
        ItemKind::FileChange { changes } => Some(json!({ "changes": changes.0 })),
        _ => None,
    }
}

fn read_raw_paths(entry: &WorkLogEntry) -> Vec<String> {
    if let Some(files) = entry
        .changed_files
        .as_ref()
        .filter(|files| !files.is_empty())
    {
        return files.clone();
    }
    if let Some(ItemKind::DynamicTool { input, .. }) = entry.item.as_ref().map(|item| &item.kind) {
        let paths = collect_tool_file_paths(&json!({ "input": input.0 }));
        if !paths.is_empty() {
            return paths;
        }
    }
    tool_data(entry)
        .map(|data| collect_tool_file_paths(&data))
        .unwrap_or_default()
}

/// Skips the first `units` UTF-16 code units, like JavaScript `slice(units)`.
fn utf16_slice_from(value: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in value.char_indices() {
        if count >= units {
            return &value[index..];
        }
        count += c.len_utf16();
    }
    ""
}

/// A path as the workspace names it: `workspace/src/a.ts` for paths inside
/// the workspace root, absolute paths outside it, with any `:line:column` kept.
pub(super) fn format_workspace_relative_path(
    path_with_position: &str,
    workspace_root: Option<&str>,
) -> String {
    let mut position = split_file_path_position(path_with_position, "");
    let normalized = strip_slash_prefixed_windows_drive(&position.path.replace('\\', "/"));
    let mut display = normalized.clone();
    if let Some(root) = workspace_root.filter(|root| !root.is_empty()) {
        let normalized_root = strip_slash_prefixed_windows_drive(
            &root.trim_end_matches(['/', '\\']).replace('\\', "/"),
        );
        let label = file_basename(&normalized_root);
        let case_insensitive = is_windows_absolute_path(&strip_slash_prefixed_windows_drive(root));
        let compare = |value: &str| {
            if case_insensitive {
                value.to_lowercase()
            } else {
                value.to_owned()
            }
        };
        let path_compare = compare(&normalized);
        let root_compare = compare(&normalized_root);
        if path_compare == root_compare {
            display = label;
        } else if path_compare.starts_with(&format!("{root_compare}/")) {
            let suffix = utf16_slice_from(&normalized, utf16_len(&normalized_root) + 1);
            display = format!("{label}/{suffix}");
        } else if !normalized.starts_with('/') {
            let relative = normalized
                .strip_prefix("./")
                .unwrap_or(&normalized)
                .trim_start_matches('/');
            display = if path_compare.starts_with(&format!("{}/", compare(&label))) {
                normalized.clone()
            } else {
                format!("{label}/{relative}")
            };
        }
    }
    position.path = display;
    format_file_path_position(&position)
}

fn read_paths(entry: &WorkLogEntry, workspace_root: Option<&str>) -> Vec<String> {
    read_raw_paths(entry)
        .iter()
        .map(|path| format_workspace_relative_path(path, workspace_root))
        .collect()
}

fn capitalize_first(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn dynamic_title(entry: &WorkLogEntry) -> Option<String> {
    match &entry.item.as_ref()?.kind {
        ItemKind::DynamicTool { name, input, .. } => dynamic_tool_title(Some(name), &input.0),
        _ => None,
    }
}

/// The heading a collapsed work-log row shows for one entry.
pub fn work_entry_display_label(entry: &WorkLogEntry, workspace_root: Option<&str>) -> String {
    if entry.item_type == Some(ItemType::SystemNotice) {
        return entry.label.clone();
    }
    if entry.item_type == Some(ItemType::Reasoning) || entry.tone == WorkTone::Thinking {
        return collapsed_detail(entry).unwrap_or_else(|| entry.label.clone());
    }
    if let Some(presentation) = resolve_work_entry_tool_presentation(entry, None) {
        return presentation.display_name;
    }
    if let Some(command) = entry
        .command
        .as_deref()
        .filter(|command| !command.is_empty())
    {
        return command_display_text(command);
    }
    let action = tool_group_action(entry);
    let searches = matches!(
        action,
        ToolGroupAction::CodeSearch | ToolGroupAction::Search
    );
    if searches && let Some(label) = format_search_tool_label(tool_data(entry).as_ref()) {
        return label;
    }
    let reads = action == ToolGroupAction::Read;
    if reads {
        let paths = read_paths(entry, workspace_root);
        if let Some(first) = paths.first().filter(|path| !path.is_empty()) {
            return format_read_tool_label(first, paths.len() - 1);
        }
    }
    // Retrying providers keep their progress label; other diagnostics expose
    // the retained message instead of a generic error heading. File bodies
    // are never a compact read label.
    let provider_retry = entry
        .item
        .as_ref()
        .is_some_and(|item| matches!(&item.kind, ItemKind::Error { retry, .. } if retry.is_some()));
    if let Some(title) = dynamic_title(entry) {
        return title;
    }
    let compact_detail = entry.detail.as_deref().map(js_trim).unwrap_or_default();
    let detail_is_search_output = searches && compact_detail.contains(['\r', '\n']);
    if !compact_detail.is_empty() && !provider_retry && !reads && !detail_is_search_output {
        return compact_detail.to_owned();
    }
    if let Some(files) = &entry.changed_files
        && let Some(first) = files.first().filter(|path| !path.is_empty())
    {
        let path = format_workspace_relative_path(first, workspace_root);
        return if files.len() == 1 {
            path
        } else {
            format!("{path} +{} more", files.len() - 1)
        };
    }
    if reads && entry.viewed_image_path.is_none() {
        return "Read file".into();
    }
    let heading = normalize_compact_tool_label(
        entry
            .tool_title
            .as_deref()
            .filter(|title| !title.is_empty())
            .unwrap_or(&entry.label),
    );
    capitalize_first(&heading)
}

/// Inspectable read-file output: the read paths, absolute under the
/// workspace root, one per line; never the file body.
pub fn work_entry_read_output(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
) -> Option<String> {
    let root = workspace_root.filter(|root| !root.is_empty());
    let mut paths: Vec<String> = vec![];
    for path in read_raw_paths(entry) {
        let trimmed = js_trim(&path).replace('\\', "/");
        let resolved = match root {
            Some(root) if !trimmed.starts_with('/') && !is_windows_absolute_path(&trimmed) => {
                let root = root.replace('\\', "/");
                let relative = trimmed.strip_prefix("./").unwrap_or(&trimmed);
                format!(
                    "{}/{}",
                    root.trim_end_matches('/'),
                    relative.trim_start_matches('/')
                )
            }
            _ => trimmed,
        };
        if !resolved.is_empty() && !paths.contains(&resolved) {
            paths.push(resolved);
        }
    }
    (!paths.is_empty()).then(|| paths.join("\n"))
}

/// The live activity row's label: present tense while `active`, the outcome otherwise.
pub fn live_work_entry_label(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
    active: bool,
) -> String {
    let status = live_activity_tool_status(entry.tool_lifecycle_status, active);
    if entry.item_type == Some(ItemType::Reasoning) {
        return collapsed_detail(entry).unwrap_or_else(|| {
            if status == ToolLifecycleStatus::InProgress {
                "Thinking".into()
            } else {
                "Thought".into()
            }
        });
    }
    let live = WorkLogEntry {
        tool_lifecycle_status: Some(status),
        ..entry.clone()
    };
    if let Some(presentation) = resolve_work_entry_tool_presentation(&live, None) {
        return presentation.display_name;
    }
    if let Some(command) = entry
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
        let program = command_program_name(command).unwrap_or_else(|| "command".into());
        return format!("{verb} {program}");
    }
    work_entry_display_label(entry, workspace_root)
}

/// Whether an expanded group lists the entry; `expanded_tool_group_entry`
/// also lists calls that are still running.
pub fn work_entry_is_visible_in_group(
    entry: &WorkLogEntry,
    expanded_tool_group_entry: bool,
) -> bool {
    if entry.item_type == Some(ItemType::Reasoning) {
        return entry
            .detail
            .as_deref()
            .is_some_and(|detail| !js_trim(detail).is_empty());
    }
    (expanded_tool_group_entry
        && (entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress)
            || entry.source_activity == Some(SourceActivity::TaskProgress)))
        // A stopped call is an outcome ("Stopped sleep"), not an empty row.
        || entry.tool_lifecycle_status == Some(ToolLifecycleStatus::Stopped)
        || !entry_indicates_tool_neutral_status(entry)
}

/// The label a lone settled call shows in place of a group summary.
pub(super) fn single_tool_call_label(entry: &WorkLogEntry) -> String {
    if entry.item_type == Some(ItemType::Reasoning) {
        return collapsed_detail(entry).unwrap_or_else(|| "Thought".into());
    }
    if let Some(presentation) =
        resolve_work_entry_tool_presentation(entry, Some(ToolLifecycleStatus::Completed))
    {
        return presentation.display_name;
    }
    if let Some(title) = dynamic_title(entry) {
        return title;
    }
    // A lone web search keeps its heading; the query stays in its detail.
    if entry.item_type == Some(ItemType::WebSearch) {
        return entry
            .tool_title
            .clone()
            .unwrap_or_else(|| "Web search".into());
    }
    work_entry_display_label(entry, None)
}

/// Whether the text carries Claude's `★ Insight` blocks, whose line breaks
/// must render as written.
pub fn should_preserve_assistant_line_breaks(text: &str) -> bool {
    text.split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .any(|line| {
            line.strip_prefix("★ Insight")
                .and_then(|rest| rest.chars().next())
                .is_some_and(|next| is_js_space(next) || next == '─')
        })
}

#[cfg(test)]
#[path = "desktop_labels_tests.rs"]
mod tests;
