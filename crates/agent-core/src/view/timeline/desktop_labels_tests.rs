use super::*;
use crate::view::timeline::entries::{EntriesInput, derive_timeline_entries};
use crate::view::work_log::fixtures::{self as fx, EntryBuilder, ItemBuilder};
use agent_domain::{Item, ItemStatus, RetryProgress, Timestamp};
use serde_json::json;

/// A tool entry labelled "Tool call".
fn entry() -> WorkLogEntry {
    WorkLogEntry::new(
        "tool-1",
        Timestamp::parse("2026-09-01T12:00:00Z").unwrap(),
        "Tool call",
        WorkTone::Tool,
    )
}

const STATUSES: [ToolLifecycleStatus; 5] = [
    ToolLifecycleStatus::InProgress,
    ToolLifecycleStatus::Completed,
    ToolLifecycleStatus::Failed,
    ToolLifecycleStatus::Declined,
    ToolLifecycleStatus::Stopped,
];

#[test]
fn previews_reasoning_in_the_live_row_and_falls_back_to_a_short_label_while_empty() {
    let thought = entry()
        .item_type(ItemType::Reasoning)
        .tone(WorkTone::Thinking)
        .detail("Check **ordering** first.")
        .status(ToolLifecycleStatus::InProgress);
    let detail = "Check **ordering** first.";
    assert_eq!(live_work_entry_label(&thought, None, true), detail);
    assert_eq!(
        live_work_entry_label(
            &thought
                .clone()
                .detail("First paragraph.\n\nSecond paragraph."),
            None,
            true
        ),
        "First paragraph. Second paragraph."
    );
    assert_eq!(
        live_work_entry_label(&thought.clone().detail("  "), None, true),
        "Thinking"
    );
    assert_eq!(
        live_work_entry_label(
            &thought
                .clone()
                .detail("")
                .status(ToolLifecycleStatus::Completed),
            None,
            false
        ),
        "Thought"
    );
    assert_eq!(
        live_work_entry_label(
            &thought.clone().status(ToolLifecycleStatus::Completed),
            None,
            false
        ),
        detail
    );
    assert_eq!(work_entry_display_label(&thought, None), detail);
    assert!(work_entry_is_visible_in_group(&thought, false));
    assert!(work_entry_is_visible_in_group(
        &thought.clone().status(ToolLifecycleStatus::Completed),
        false
    ));
    assert!(!work_entry_is_visible_in_group(
        &thought.clone().detail("  "),
        false
    ));
}

#[test]
fn uses_the_same_friendly_label_in_both_views() {
    let labels = [
        "Reading a thread",
        "Read a thread",
        "Failed to read a thread",
        "Declined to read a thread",
        "Stopped reading a thread",
    ];
    for (status, label) in STATUSES.into_iter().zip(labels) {
        let tool = entry()
            .tool_title("orchestration.thread_read")
            .detail(r#"{"ok":true}"#)
            .status(status);
        assert_eq!(
            live_work_entry_label(&tool, None, status == ToolLifecycleStatus::InProgress),
            label
        );
        assert_eq!(work_entry_display_label(&tool, None), label);
    }
}

#[test]
fn uses_the_active_summary_state_for_tools_without_a_lifecycle_status() {
    let tool = entry().tool_title("orchestration.thread_read");
    assert_eq!(live_work_entry_label(&tool, None, true), "Reading a thread");
    assert_eq!(live_work_entry_label(&tool, None, false), "Read a thread");
}

#[test]
fn keeps_the_latest_live_activity_in_the_present_tense_after_the_call_completes() {
    let tool = entry()
        .tool_title("orchestration.thread_read")
        .status(ToolLifecycleStatus::Completed);
    assert_eq!(live_work_entry_label(&tool, None, true), "Reading a thread");
    assert_eq!(live_work_entry_label(&tool, None, false), "Read a thread");
}

#[test]
fn labels_file_reads_with_the_path_and_never_the_file_body() {
    let read = entry()
        .item_type(ItemType::DynamicTool)
        .tool_title("Read")
        .label("Read")
        .detail("---\nname: env\n---\n cons t x = 1")
        .item(fx::dynamic_tool(
            "read",
            "Read",
            json!({ "file_path": "src/env.ts" }),
            None,
        ));
    assert_eq!(work_entry_display_label(&read, None), "Read src/env.ts");
    assert_eq!(
        work_entry_read_output(&read, None).as_deref(),
        Some("src/env.ts")
    );
    assert_eq!(
        work_entry_read_output(&read, Some("/workspace/ohseearr")).as_deref(),
        Some("/workspace/ohseearr/src/env.ts")
    );
    let from_location = entry().item(fx::dynamic_tool(
        "read",
        "Read",
        json!({ "locations": [{ "path": "src/from-location.ts" }] }),
        None,
    ));
    assert_eq!(
        work_entry_read_output(&from_location, Some("/workspace/ohseearr")).as_deref(),
        Some("/workspace/ohseearr/src/from-location.ts")
    );
    assert_eq!(work_entry_read_output(&entry().detail("---"), None), None);
}

#[test]
fn labels_claude_grep_from_structured_input_instead_of_a_generic_tool_heading() {
    let grep = entry()
        .item_type(ItemType::DynamicTool)
        .tool_title("Grep")
        .label("Grep")
        .item(fx::dynamic_tool(
            "grep",
            "Grep",
            json!({ "pattern": "TODO", "path": "apps/web" }),
            None,
        ));
    assert_eq!(
        work_entry_display_label(&grep, None),
        "Searched TODO in web"
    );
}

#[test]
fn keeps_a_multi_line_approval_prompt_as_its_label() {
    let prompt = "Allow this command?\nrm -rf dist";
    let approval = entry()
        .item_type(ItemType::ApprovalRequest)
        .request_kind("command")
        .detail(prompt);
    assert_eq!(work_entry_display_label(&approval, None), prompt);
}

#[test]
fn keeps_custom_titles_and_output_for_unrecognized_tools() {
    let unknown = entry().tool_title("mcp__github__search_issues");
    assert_eq!(
        live_work_entry_label(&unknown, None, true),
        "Mcp__github__search_issues"
    );
    assert_eq!(
        work_entry_display_label(&unknown.detail("Found 3 issues"), None),
        "Found 3 issues"
    );
}

#[test]
fn keeps_command_summaries_compact_without_replacing_the_full_command_in_expanded_rows() {
    let command = entry().command("vp test run").detail("All tests passed");
    assert_eq!(live_work_entry_label(&command, None, true), "Running vp");
    assert_eq!(live_work_entry_label(&command, None, false), "Ran vp");
    assert_eq!(work_entry_display_label(&command, None), "vp test run");
}

#[test]
fn summarizes_the_program_inside_a_shell_wrapper_while_preserving_the_expanded_command() {
    let command = "/bin/zsh -lc 'vp test run apps/web/src/session-logic.test.ts'";
    let entry = entry().command(command);
    assert_eq!(live_work_entry_label(&entry, None, true), "Running vp");
    assert_eq!(live_work_entry_label(&entry, None, false), "Ran vp");
    assert_eq!(
        work_entry_display_label(&entry, None),
        "vp test run apps/web/src/session-logic.test.ts"
    );
    assert_eq!(entry.command.as_deref(), Some(command));
}

#[test]
fn uses_present_tense_for_a_live_command_and_the_outcome_once_it_is_no_longer_live() {
    let labels = [
        ("Running vp", "Running vp"),
        ("Running vp", "Ran vp"),
        ("Failed vp", "Failed vp"),
        ("Declined vp", "Declined vp"),
        ("Stopped vp", "Stopped vp"),
    ];
    for (status, (live, settled)) in STATUSES.into_iter().zip(labels) {
        let command = entry()
            .command("/bin/bash -lc 'vp test run'")
            .status(status);
        assert_eq!(
            live_work_entry_label(&command, None, true),
            live,
            "{status:?}"
        );
        assert_eq!(
            live_work_entry_label(&command, None, false),
            settled,
            "{status:?}"
        );
    }
}

#[test]
fn preserves_claude_insight_formatting_without_changing_regular_markdown() {
    assert!(should_preserve_assistant_line_breaks(
        "★ Insight ─────────────────\\nFirst observation\\nSecond observation\\n─────────────────"
    ));
    assert!(!should_preserve_assistant_line_breaks(
        "A normal\\nmarkdown paragraph"
    ));
}

#[test]
fn removes_trailing_completion_wording_from_command_labels() {
    assert_eq!(
        normalize_compact_tool_label("Ran command complete"),
        "Ran command"
    );
}

#[test]
fn removes_trailing_completion_wording_from_other_labels() {
    assert_eq!(
        normalize_compact_tool_label("Read file completed"),
        "Read file"
    );
}

const RETAINED_MESSAGE: &str = "2026-03-14T16:11:12.550224Z ERROR codex_core::codex: failed to load skill /home/sebherrerabe/repos/devsuite/.agent/skills/monorepo-scaffolding/SKILL.md: invalid YAML: mapping va...";

fn diagnostic_entry(item: Item) -> WorkLogEntry {
    let state = fx::state(vec![item]);
    let entries = derive_timeline_entries(&state, &EntriesInput::default());
    let [entry] = entries.as_slice() else {
        panic!("expected one entry");
    };
    entry.work().expect("a work-log entry").clone()
}

#[test]
fn shows_the_retained_error_message_in_place_of_a_generic_row_label() {
    let entry = diagnostic_entry(fx::error("diagnostic", RETAINED_MESSAGE));
    assert_eq!(entry.label, "Provider error");
    assert_eq!(entry.detail.as_deref(), Some(RETAINED_MESSAGE));
    assert_eq!(work_entry_display_label(&entry, None), RETAINED_MESSAGE);
}

#[test]
fn uses_the_full_system_notice_instead_of_a_truncated_warning_title() {
    let entry = diagnostic_entry(fx::system_notice("diagnostic", RETAINED_MESSAGE));
    assert_eq!(entry.label, RETAINED_MESSAGE);
    assert_eq!(work_entry_display_label(&entry, None), RETAINED_MESSAGE);
    assert_eq!(entry.detail, None);
}

#[test]
fn keeps_retry_progress_visible_while_retaining_its_diagnostic_detail() {
    let mut item = fx::error("diagnostic", RETAINED_MESSAGE).status(ItemStatus::Running);
    if let agent_domain::ItemKind::Error { retry, .. } = &mut item.kind {
        *retry = Some(RetryProgress {
            attempt: 2,
            max_attempts: Some(5),
            delay_ms: Some(1_500),
        });
    }
    let entry = diagnostic_entry(item);
    assert_eq!(
        work_entry_display_label(&entry, None),
        "Retrying provider (2/5)"
    );
    assert_eq!(
        entry.detail,
        Some(format!("{RETAINED_MESSAGE} Retrying in 1.5s."))
    );
}
