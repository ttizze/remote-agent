//! A desktop work-log row: the icon, heading, tones and expansion of one
//! entry, and the inspector panel it opens.
use super::desktop_labels::{
    entry_is_tool_like, format_workspace_relative_path, work_entry_display_label,
    work_entry_read_output,
};
use super::work_row::{
    ProviderFailureRow, WorkActivityDetail, WorkActivityRow, WorkIcon, WorkIconTone, WorkLabelTone,
    WorkLogRow, WorkRowIcon, WorkRowRole,
};
use crate::presentation::markdown::plain_text_preview;
use crate::sync::Detail;
use crate::view::work_log::item_detail::{
    tool_call_lines, turn_item_has_detail, turn_item_needs_detail_fetch, turn_item_output_text,
};
use crate::view::work_log::presentation::{
    ToolGroupAction, resolve_viewed_image_asset, resolve_work_entry_tool_presentation,
    tool_group_action, tool_item_for_display, work_entry_display_indicates_tool_failure,
    work_entry_viewed_image_path,
};
use crate::view::work_log::tool_activity::claude_skill_invocation;
use crate::view::work_log::tool_catalog::ToolLogo;
use crate::view::work_log::turn_item::workspace_preparation_retry_run_ids;
use crate::view::work_log::user_input::{
    has_question_answer, question_answer_preview, question_text_preview,
};
use crate::view::work_log::{
    ItemType, SourceActivity, ToolLifecycleStatus, ToolSurface, WorkLogEntry, WorkTone,
};
use agent_domain::{
    BackgroundKind, Item, ItemKind, ItemStatus, NotificationOutcome, NotificationSource,
    RequestBody, State, ThreadId, WORKSPACE_PREPARATION_FAILURE_CODE,
};

fn tone_icon(tone: WorkTone) -> WorkRowIcon {
    match tone {
        WorkTone::Error => WorkRowIcon::Feed(WorkIcon::Alert),
        WorkTone::Thinking => WorkRowIcon::Brain,
        WorkTone::Info => WorkRowIcon::Feed(WorkIcon::Check),
        WorkTone::Tool => WorkRowIcon::Feed(WorkIcon::Zap),
    }
}

/// The icon a summary of `action` calls draws.
pub fn tool_group_action_icon(action: ToolGroupAction) -> WorkRowIcon {
    match action {
        ToolGroupAction::Read => WorkRowIcon::Feed(WorkIcon::Eye),
        ToolGroupAction::Edit => WorkRowIcon::Feed(WorkIcon::Edit),
        ToolGroupAction::Command => WorkRowIcon::Feed(WorkIcon::Command),
        ToolGroupAction::ThreadCreate => WorkRowIcon::Logo(ToolLogo::App),
        ToolGroupAction::Search => WorkRowIcon::Feed(WorkIcon::Globe),
        ToolGroupAction::CodeSearch => WorkRowIcon::Feed(WorkIcon::Search),
        ToolGroupAction::Other => WorkRowIcon::Feed(WorkIcon::Wrench),
        ToolGroupAction::Update => WorkRowIcon::Feed(WorkIcon::Hammer),
    }
}

/// The icon of one desktop work entry.
pub fn work_entry_icon(entry: &WorkLogEntry) -> WorkRowIcon {
    if let Some(ItemKind::Notification { notification }) = entry.item.as_ref().map(|i| &i.kind) {
        if notification.outcome == NotificationOutcome::Failed {
            return WorkRowIcon::Feed(WorkIcon::Alert);
        }
        return WorkRowIcon::Feed(match notification.source {
            NotificationSource::Delegated { .. }
            | NotificationSource::Native(BackgroundKind::Subagent) => WorkIcon::Agent,
            NotificationSource::Native(BackgroundKind::Command) => WorkIcon::Command,
            NotificationSource::Native(BackgroundKind::Monitor) => WorkIcon::Eye,
            NotificationSource::Native(BackgroundKind::BackgroundTask) => WorkIcon::Zap,
        });
    }
    if matches!(
        entry.item_type,
        Some(ItemType::UserInputRequest | ItemType::ApprovalRequest)
    ) {
        return WorkRowIcon::Feed(WorkIcon::Message);
    }
    match entry.tool_surface {
        Some(ToolSurface::Browser) => return WorkRowIcon::Feed(WorkIcon::Browser),
        Some(ToolSurface::Computer) => return WorkRowIcon::Feed(WorkIcon::Computer),
        None => {}
    }
    if let Some(presentation) = resolve_work_entry_tool_presentation(entry, None) {
        return WorkRowIcon::Logo(presentation.icon);
    }
    let action = tool_group_action(entry);
    if action != ToolGroupAction::Other {
        return tool_group_action_icon(action);
    }
    match entry.item_type {
        Some(ItemType::DynamicTool) => WorkRowIcon::Feed(WorkIcon::Wrench),
        Some(ItemType::Subagent) => WorkRowIcon::Feed(WorkIcon::Agent),
        _ => tone_icon(entry.tone),
    }
}

fn raw_command(entry: &WorkLogEntry) -> Option<String> {
    let raw = entry.raw_command.as_deref()?.trim();
    let command = entry.command.as_deref()?;
    (!raw.is_empty() && raw != command.trim()).then(|| raw.into())
}

fn dynamic_input(entry: &WorkLogEntry) -> Option<&serde_json::Value> {
    match &entry.item.as_ref()?.kind {
        ItemKind::DynamicTool { input, .. } => Some(&input.0),
        _ => None,
    }
}

/// The plain text an expanded call shows: its input, command, detail and the
/// files it changed, each once and never repeating the heading.
pub fn tool_call_expanded_body(
    entry: &WorkLogEntry,
    workspace_root: Option<&str>,
    visible_label: &str,
    viewed_image_path: Option<&str>,
) -> Option<String> {
    let mut blocks: Vec<String> = vec![];
    let mut seen = vec![visible_label.trim().to_string()];
    let add = |value: Option<&str>, blocks: &mut Vec<String>, seen: &mut Vec<String>| {
        let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
            return;
        };
        if !seen.iter().any(|seen| seen == text) {
            seen.push(text.into());
            blocks.push(text.into());
        }
    };
    if entry.item_type == Some(ItemType::DynamicTool)
        && let Some(input) = dynamic_input(entry)
    {
        let input = serde_json::to_string_pretty(input).unwrap_or_default();
        add(
            Some(&format!("Tool input\n{input}")),
            &mut blocks,
            &mut seen,
        );
    }
    let command = entry.command.as_deref().map(str::trim);
    if command.is_some() && command == Some(visible_label.trim()) {
        seen.push(visible_label.trim().into());
    } else {
        let raw = raw_command(entry);
        add(raw.as_deref().or(command), &mut blocks, &mut seen);
    }
    let detail = entry.detail.as_deref().map(str::trim);
    if detail != viewed_image_path.map(str::trim) {
        add(detail, &mut blocks, &mut seen);
    }
    let image_paths: Vec<String> = viewed_image_path
        .map(|path| {
            vec![
                path.trim().to_string(),
                format_workspace_relative_path(path, workspace_root),
            ]
        })
        .unwrap_or_default();
    let mut changed: Vec<String> = vec![];
    for path in entry.changed_files.iter().flatten() {
        let formatted = format_workspace_relative_path(path, workspace_root);
        let shown = image_paths.contains(path)
            || image_paths.contains(&formatted)
            || Some(path.trim()) == detail
            || Some(formatted.as_str()) == detail;
        if !shown && !changed.contains(&formatted) {
            changed.push(formatted);
        }
    }
    if !changed.is_empty() {
        add(Some(&changed.join("\n")), &mut blocks, &mut seen);
    }
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

/// The text the inspector panel shows for item kinds without a tool call.
fn inspector_value(state: &State, item: &Item, workspace_root: Option<&str>) -> Option<String> {
    let request_body = |id| {
        state
            .requests
            .iter()
            .find(|request| &request.id == id)
            .map(|request| &request.body)
    };
    let value = match &item.kind {
        ItemKind::ApprovalRequest { request } => match request_body(request) {
            Some(RequestBody::Approval { detail, kind, .. }) => {
                Some(detail.clone().unwrap_or_else(|| kind.clone()))
            }
            _ => None,
        },
        ItemKind::UserInputRequest { request } => match request_body(request) {
            Some(RequestBody::Questions { questions }) => Some(
                questions
                    .iter()
                    .map(|question| question.question.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
            _ => None,
        },
        ItemKind::Notification { notification } => notification.detail.clone(),
        ItemKind::SystemNotice { message } | ItemKind::Error { message, .. } => {
            Some(message.clone())
        }
        ItemKind::ProposedPlan { plan } | ItemKind::TodoList { plan } => {
            state.plans.iter().find(|p| &p.id == plan).map(|plan| {
                if matches!(item.kind, ItemKind::TodoList { .. }) {
                    plan.steps
                        .iter()
                        .map(|step| {
                            let mark = if step.status == "completed" {
                                "✓"
                            } else {
                                "○"
                            };
                            format!("{mark} {}", step.text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                } else {
                    plan.markdown.clone()
                }
            })
        }
        ItemKind::FileChange { changes } => Some(
            super::entries::file_change_paths(changes)
                .iter()
                .map(|path| format_workspace_relative_path(path, workspace_root))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ItemKind::WebSearch { query, .. } => Some(query.clone()),
        _ => None,
    };
    value.filter(|value| !value.trim().is_empty())
}

/// What a row needs besides its entry.
#[derive(Debug, Clone, Copy)]
pub struct WorkRowContext<'a> {
    pub state: &'a State,
    pub thread: Option<&'a ThreadId>,
    pub workspace_root: Option<&'a str>,
}

/// The desktop row of `entry`. `display_label` is the heading of a lone call;
/// `detail` is the loaded withheld detail of its item.
pub fn desktop_work_log_row(
    context: WorkRowContext<'_>,
    entry: &WorkLogEntry,
    display_label: Option<&str>,
    expanded: bool,
    detail: Option<&Detail>,
) -> WorkLogRow {
    let WorkRowContext {
        state,
        thread,
        workspace_root,
    } = context;
    let item = entry.item.as_deref();
    if let Some(
        failure @ Item {
            kind:
                ItemKind::Error {
                    message,
                    class,
                    code,
                    reset_at,
                    ..
                },
            status: ItemStatus::Failed,
            ..
        },
    ) = item
    {
        let retry_preparation = failure
            .run
            .as_ref()
            .filter(|_| code.as_deref() == Some(WORKSPACE_PREPARATION_FAILURE_CODE))
            .filter(|run| {
                workspace_preparation_retry_run_ids(&state.runs, &state.items).contains(run)
            })
            .cloned();
        return WorkLogRow::ProviderFailure(ProviderFailureRow {
            summary: entry.label.clone(),
            warning: class.as_deref() == Some("usage_limit"),
            message: message.clone(),
            reset_at: reset_at.clone(),
            created_at: entry.created_at.clone(),
            retry_preparation,
            copy_text: String::new(),
        });
    }
    let show_warning = entry.source_activity == Some(SourceActivity::RuntimeWarning);
    let show_failed = !show_warning && work_entry_display_indicates_tool_failure(entry);
    let destructive = !show_warning
        && show_failed
        && (entry.item_type == Some(ItemType::Error) || !entry_is_tool_like(entry));
    let alert = show_warning || destructive;
    let icon = if alert {
        WorkRowIcon::Feed(WorkIcon::Alert)
    } else {
        work_entry_icon(entry)
    };
    let tool_icon = (!alert)
        .then(|| {
            entry
                .tool_icon
                .clone()
                .or_else(|| entry.tool_source.as_ref().and_then(|s| s.icon.clone()))
        })
        .flatten();
    let reasoning = entry.item_type == Some(ItemType::Reasoning);
    let answer = entry.question_answer.as_ref();
    let question_heading = answer.map(question_text_preview).unwrap_or_default();
    let preview = if reasoning && expanded {
        if entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress) {
            "Thinking".to_string()
        } else {
            "Thought".to_string()
        }
    } else if !question_heading.is_empty() {
        question_heading
    } else {
        display_label.map_or_else(
            || work_entry_display_label(entry, workspace_root),
            str::to_string,
        )
    };
    // A collapsed thought reads as plain text of its markdown.
    let label = if reasoning && !expanded {
        let fallback = if entry.tool_lifecycle_status == Some(ToolLifecycleStatus::InProgress) {
            "Thinking"
        } else {
            "Thought"
        };
        plain_text_preview(entry.detail.as_deref().unwrap_or(&preview), fallback)
    } else {
        preview.clone()
    };
    let answer_preview = answer
        .filter(|answer| has_question_answer(answer))
        .map(question_answer_preview);
    let viewed_image_path = work_entry_viewed_image_path(entry).filter(|path| {
        thread.is_some_and(|thread| {
            resolve_viewed_image_asset(path, thread, workspace_root).is_some()
        })
    });
    let skill = match item.map(|item| &item.kind) {
        Some(ItemKind::DynamicTool { name, input, .. }) => {
            claude_skill_invocation(Some(name), &input.0)
        }
        _ => None,
    };
    // Reads and skills expand to plain text instead of the item inspector.
    let plain_output: Option<Option<String>> = if tool_group_action(entry) == ToolGroupAction::Read
    {
        Some(work_entry_read_output(entry, workspace_root))
    } else {
        skill.map(|skill| skill.args)
    };
    let plain_output_fetches =
        plain_output.is_some() && item.is_some_and(turn_item_needs_detail_fetch);
    let can_expand = match (&plain_output, item) {
        (Some(output), _) => {
            output.is_some()
                || viewed_image_path.is_some()
                || answer.is_some()
                || plain_output_fetches
        }
        (None, None) => {
            !preview.trim().is_empty()
                || raw_command(entry).is_some()
                || entry
                    .command
                    .as_deref()
                    .is_some_and(|c| !c.trim().is_empty())
                || entry
                    .detail
                    .as_deref()
                    .is_some_and(|d| !d.trim().is_empty())
                || entry.changed_files.as_ref().is_some_and(|f| !f.is_empty())
                || viewed_image_path.is_some()
        }
        (None, Some(_)) if reasoning => entry
            .detail
            .as_deref()
            .is_some_and(|d| !d.trim().is_empty()),
        (None, Some(item)) => {
            viewed_image_path.is_some() || answer.is_some() || turn_item_has_detail(item, state)
        }
    };
    let (opens_thread, open_label) = match item.map(|item| &item.kind) {
        Some(ItemKind::ThreadCreated { thread, .. }) => (Some(thread.clone()), Some("Open chat")),
        Some(ItemKind::Notification { notification }) => (
            notification.child_thread.clone(),
            notification.child_thread.as_ref().map(|_| "Open subagent"),
        ),
        _ => (None, None),
    };
    let fetched = match detail {
        Some(Detail::Loaded(loaded))
            if item.is_some_and(|item| {
                std::mem::discriminant(&item.kind) == std::mem::discriminant(&loaded.kind)
            }) =>
        {
            Some(loaded.as_ref())
        }
        _ => None,
    };
    let fetch_error = match detail {
        Some(Detail::Failed(error)) => Some(error.as_str()),
        _ => None,
    };
    let fetches = item.is_some_and(turn_item_needs_detail_fetch);
    let output_text = |shown: &Item| {
        turn_item_output_text(shown)
            .or_else(|| fetch_error.map(|error| format!("Couldn't load output: {error}")))
            .or_else(|| (fetches && fetched.is_none()).then(|| "Loading output…".into()))
            .or_else(|| (fetches && fetched.is_some()).then(|| "No output.".into()))
    };
    let mut panel = WorkActivityDetail {
        reasoning: None,
        call: None,
        full_detail: None,
        output: None,
        failed_exit_code: None,
        viewed_image_path: viewed_image_path.clone(),
        shows_question_answer: answer.is_some(),
    };
    if expanded && reasoning {
        panel.reasoning = entry.detail.clone();
    } else if expanded && answer.is_none() {
        // Raw outputs show only once fetched, never from a cached copy.
        let displayed = item.map(tool_item_for_display);
        let shown = fetched.or(displayed.as_ref());
        match (&plain_output, shown) {
            (Some(output), _) => {
                panel.full_detail = output.clone();
                if plain_output_fetches {
                    panel.output = shown.and_then(output_text);
                }
            }
            (None, None) => {
                panel.full_detail = tool_call_expanded_body(
                    entry,
                    workspace_root,
                    &preview,
                    viewed_image_path.as_deref(),
                );
            }
            (None, Some(shown)) => match &shown.kind {
                ItemKind::CommandExecution {
                    command, exit_code, ..
                } => {
                    panel.call = Some(tool_call_lines(Some(command), None));
                    panel.output = output_text(shown);
                    panel.failed_exit_code = exit_code.filter(|code| *code != 0);
                }
                ItemKind::DynamicTool { input, .. } => {
                    panel.call = Some(tool_call_lines(None, Some(&input.0)));
                    panel.output = output_text(shown);
                }
                ItemKind::WebSearch { .. } => {
                    panel.full_detail = inspector_value(state, shown, workspace_root);
                    panel.output = turn_item_output_text(shown);
                }
                ItemKind::Reasoning => panel.reasoning = Some(shown.text.clone()),
                _ => panel.full_detail = inspector_value(state, shown, workspace_root),
            },
        }
    }
    let accessible = [Some(preview.clone()), answer_preview.clone()]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(": ");
    let shows_panel = expanded
        && can_expand
        && (panel.reasoning.is_some()
            || panel.call.is_some()
            || panel.full_detail.is_some()
            || panel.output.is_some()
            || panel.viewed_image_path.is_some()
            || panel.shows_question_answer);
    WorkLogRow::Activity(Box::new(WorkActivityRow {
        id: entry.id.clone(),
        role: if can_expand {
            WorkRowRole::Button
        } else if opens_thread.is_some() {
            WorkRowRole::Link
        } else {
            WorkRowRole::None
        },
        opens_thread,
        open_label: open_label.map(str::to_owned),
        can_expand,
        expanded,
        load_detail: expanded && can_expand && fetches && detail.is_none(),
        shimmer: false,
        label,
        answer_highlighted: !expanded && answer_preview.is_some(),
        answer_preview,
        failure_mark: show_failed
            && !destructive
            && (tool_icon.is_some() || icon == WorkRowIcon::Feed(WorkIcon::Computer)),
        icon,
        tool_icon,
        icon_tone: if show_warning {
            WorkIconTone::Warning
        } else if destructive {
            WorkIconTone::Destructive
        } else if show_failed {
            WorkIconTone::Failed
        } else {
            WorkIconTone::Default
        },
        label_tone: if show_warning {
            WorkLabelTone::Warning
        } else if destructive {
            WorkLabelTone::Danger
        } else {
            WorkLabelTone::Default
        },
        failed: show_failed,
        accessibility_label: if show_failed {
            format!("{accessible}, tool call failed")
        } else {
            accessible
        },
        accessibility_hint: String::new(),
        copy_text: String::new(),
        detail: shows_panel.then_some(panel),
    }))
}

#[cfg(test)]
#[path = "desktop_work_row_tests.rs"]
mod tests;
