use super::*;
use crate::view::work_log::QuestionAnswer;
use crate::view::work_log::fixtures::*;
use agent_domain::{RetryProgress, RuntimeRequestId};
use serde_json::json;
use std::sync::Arc;

fn row(state: &State, entry: &WorkLogEntry, expanded: bool) -> WorkActivityRow {
    row_in(state, entry, expanded, None, None)
}

fn row_in(
    state: &State,
    entry: &WorkLogEntry,
    expanded: bool,
    root: Option<&str>,
    detail: Option<&Detail>,
) -> WorkActivityRow {
    let context = WorkRowContext {
        state,
        thread: None,
        workspace_root: root,
    };
    match desktop_work_log_row(context, entry, None, expanded, detail) {
        WorkLogRow::Activity(row) => *row,
        WorkLogRow::ProviderFailure(failure) => panic!("unexpected failure row {failure:?}"),
    }
}

fn with_item(entry: WorkLogEntry, item: Item) -> WorkLogEntry {
    WorkLogEntry {
        item: Some(Arc::new(item)),
        ..entry
    }
}

#[test]
fn shows_dynamic_tool_input_without_cached_output_when_the_row_is_expanded() {
    let item = dynamic_tool(
        "tool-with-cached-output",
        "Example tool",
        json!({"query": "KEEP_TOOL_INPUT"}),
        Some(json!({"text": "RAW_CACHED_TOOL_OUTPUT"})),
    );
    let entry = with_item(
        entry("tool-with-cached-output")
            .label("Example tool")
            .tool_title("Example tool")
            .item_type(ItemType::DynamicTool)
            .status(ToolLifecycleStatus::Completed),
        item,
    );
    let state = state(vec![]);
    let collapsed = row(&state, &entry, false);
    assert_eq!(collapsed.accessibility_label, "Example tool");
    assert!(collapsed.can_expand);
    assert_eq!(collapsed.detail, None);
    let expanded = row(&state, &entry, true);
    let visible = format!("{:?}", expanded.detail);
    assert!(visible.contains("KEEP_TOOL_INPUT"), "{visible}");
    assert!(!visible.contains("RAW_CACHED_TOOL_OUTPUT"), "{visible}");
}

#[test]
fn leads_an_unanswered_question_row_with_the_question_text() {
    let mut entry = entry("question-work").label("User input requested");
    entry.question_answer = Some(QuestionAnswer {
        request: RuntimeRequestId::new("question-request").unwrap(),
        answers: vec![],
        attachments: vec![],
        question_text: vec![("scope".into(), "Which repository?".into())],
    });
    let state = state(vec![]);
    let collapsed = row(&state, &entry, false);
    assert_eq!(collapsed.label, "Which repository?");
    assert_eq!(collapsed.accessibility_label, "Which repository?");
    assert!(!collapsed.expanded);
    let expanded = row(&state, &entry, true);
    assert!(expanded.detail.unwrap().shows_question_answer);
}

#[test]
fn renders_provider_retries_in_the_normal_work_log() {
    let mut item = error("provider-error", "The response stream disconnected.")
        .run("run-1")
        .status(ItemStatus::Running);
    if let ItemKind::Error { retry, class, .. } = &mut item.kind {
        *retry = Some(RetryProgress {
            attempt: 2,
            max_attempts: Some(5),
            delay_ms: None,
        });
        *class = Some("transport_error".into());
    }
    let state = state(vec![item.clone()]);
    let entry = crate::view::timeline::entries::work_entry(&state, &Arc::new(item));
    let collapsed = row(&state, &entry, false);
    assert_eq!(collapsed.label, "Retrying provider (2/5)");
    // The failure message stays behind the row's expander.
    assert!(collapsed.can_expand);
    assert!(!collapsed.expanded);
    assert_eq!(collapsed.detail, None);
}

#[test]
fn renders_app_tools_with_the_product_logo_and_pretty_name() {
    let item = dynamic_tool(
        "tool-thread-read",
        "mcp__orchestration__thread_read",
        json!({"threadId": "thread-child"}),
        Some(json!({"messages": []})),
    );
    let state = state(vec![item.clone()]);
    let entry = crate::view::timeline::entries::work_entry(&state, &Arc::new(item));
    let row = row(&state, &entry, false);
    assert_eq!(row.icon, WorkRowIcon::Logo(ToolLogo::App));
    assert_eq!(row.label, "Read a thread");
}

#[test]
fn formats_changed_file_paths_from_the_workspace_root() {
    let entry = entry("work-1")
        .label("Updated files")
        .item_type(ItemType::FileChange)
        .status(ToolLifecycleStatus::Completed)
        .changed_files(&["C:/Users/mike/dev-stuff/app/apps/web/src/session-logic.ts"]);
    let row = row_in(
        &state(vec![]),
        &entry,
        false,
        Some("C:/Users/mike/dev-stuff/app"),
        None,
    );
    assert!(row.label.contains("app/apps/web/src/session-logic.ts"));
    assert!(!row.label.contains("C:/Users/mike/dev-stuff/app/apps"));
}

#[test]
fn renders_a_muted_failure_marker_for_failed_tool_lifecycle_entries() {
    let entry = entry("work-1")
        .label("Glob")
        .status(ToolLifecycleStatus::Failed)
        .detail("No files found");
    let row = row(&state(vec![]), &entry, false);
    assert_eq!(row.icon, WorkRowIcon::Feed(WorkIcon::Zap));
    assert!(row.failed);
    assert_eq!(row.accessibility_label, "No files found, tool call failed");
    // Ordinary tool failures render muted, not red.
    assert_eq!(row.icon_tone, WorkIconTone::Failed);
    assert_eq!(row.label_tone, WorkLabelTone::Default);
}

#[test]
fn keeps_the_red_treatment_for_severe_orchestration_failures() {
    let entry = entry("work-turn-failed")
        .label("Provider turn start failed")
        .tone(WorkTone::Error)
        .item_type(ItemType::Error)
        .status(ToolLifecycleStatus::Failed);
    let row = row(&state(vec![]), &entry, false);
    assert_eq!(row.icon, WorkRowIcon::Feed(WorkIcon::Alert));
    assert_eq!(row.icon_tone, WorkIconTone::Destructive);
    assert_eq!(row.label_tone, WorkLabelTone::Danger);
}

#[test]
fn shows_plain_text_for_a_reasoning_preview() {
    let markdown =
        "**Viewing image first** with *care*, ~~old~~ `code` and [context](https://example.com)";
    let entry = entry("reasoning-preview")
        .label(markdown)
        .detail(markdown)
        .tone(WorkTone::Thinking)
        .item_type(ItemType::Reasoning)
        .status(ToolLifecycleStatus::Completed);
    let state = state(vec![]);
    assert_eq!(
        row(&state, &entry, false).label,
        "Viewing image first with care, old code and context"
    );
    let expanded = row(&state, &entry, true);
    assert_eq!(expanded.label, "Thought");
    assert_eq!(
        expanded.detail.unwrap().reasoning.as_deref(),
        Some(markdown)
    );
}

#[test]
fn expands_and_collapses_a_tool_call_through_its_header() {
    let entry = entry("work-standalone")
        .label("Run lint")
        .item_type(ItemType::CommandExecution)
        .command("pnpm lint")
        .status(ToolLifecycleStatus::Completed);
    let state = state(vec![]);
    let collapsed = row(&state, &entry, false);
    assert_eq!(collapsed.role, WorkRowRole::Button);
    assert!(!collapsed.expanded);
    let expanded = row(&state, &entry, true);
    assert!(expanded.expanded);
    // The heading already shows the command, so the body adds nothing.
    assert_eq!(expanded.label, "pnpm lint");
    assert_eq!(expanded.detail, None);
}

#[test]
fn loads_withheld_command_output_on_expansion_and_shows_it_once_loaded() {
    let item = command("command", "cargo test")
        .text("cached output")
        .output_omitted()
        .exit_code(Some(101));
    let state = state(vec![item.clone()]);
    let entry = crate::view::timeline::entries::work_entry(&state, &Arc::new(item.clone()));
    let loading = row(&state, &entry, true);
    assert!(loading.load_detail);
    let panel = loading.detail.unwrap();
    assert_eq!(panel.output.as_deref(), Some("Loading output…"));
    assert_eq!(panel.failed_exit_code, Some(101));
    let loaded = Detail::Loaded(Box::new(item.text("test result: FAILED")));
    let shown = row_in(&state, &entry, true, None, Some(&loaded));
    assert!(!shown.load_detail);
    assert_eq!(
        shown.detail.unwrap().output.as_deref(),
        Some("test result: FAILED")
    );
    let failed = Detail::Failed("timeout".into());
    let error = row_in(&state, &entry, true, None, Some(&failed));
    assert_eq!(
        error.detail.unwrap().output.as_deref(),
        Some("Couldn't load output: timeout")
    );
}

#[test]
fn a_failed_turn_draws_its_provider_failure_with_a_preparation_retry() {
    let mut item = error("failure", "Could not create the worktree.")
        .run("run-1")
        .status(ItemStatus::Failed);
    if let ItemKind::Error { code, .. } = &mut item.kind {
        *code = Some(WORKSPACE_PREPARATION_FAILURE_CODE.into());
    }
    let mut state = state(vec![item.clone()]);
    state
        .runs
        .push(run("run-1", 1, agent_domain::RunStatus::Failed));
    let entry = crate::view::timeline::entries::work_entry(&state, &Arc::new(item));
    let context = WorkRowContext {
        state: &state,
        thread: None,
        workspace_root: None,
    };
    let WorkLogRow::ProviderFailure(failure) =
        desktop_work_log_row(context, &entry, None, false, None)
    else {
        panic!("a provider failure row")
    };
    assert_eq!(failure.summary, "Provider error");
    assert_eq!(failure.message, "Could not create the worktree.");
    assert!(!failure.warning);
    assert_eq!(failure.retry_preparation.unwrap().as_str(), "run-1");
}
