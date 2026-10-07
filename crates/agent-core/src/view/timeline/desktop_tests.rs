use super::*;
use crate::view::timeline::desktop_labels::work_entry_display_label;
use crate::view::timeline::desktop_layout::{
    ExpandedGroupAnchor, WorkGroupScrollIndex, resolve_work_group_scroll_index,
    should_follow_work_group_append,
};
use crate::view::timeline::entries::{EntriesInput, TimelineAttempt, derive_timeline_entries};
use crate::view::work_log::fixtures::{self as fx, EntryBuilder, ItemBuilder};
use crate::view::work_log::{NativeApp, ToolSource, ToolSourceKind};
use agent_domain::{
    AttemptStatus, CheckpointFile, CheckpointStatus, InputIntent, Message, Run, State, ThreadId,
    WorktreeSetupStage,
};
use agent_domain::{WorktreeSetupStageId, WorktreeSetupStageStatus};
use serde_json::{Value, json};

fn ts(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn run_id(value: &str) -> RunId {
    RunId::new(value).unwrap()
}

fn message_id(value: &str) -> MessageId {
    MessageId::new(value).unwrap()
}

/// `2026-01-01T00:00:SSZ`.
fn second(second: i64) -> String {
    Timestamp::from_millis(ts("2026-01-01T00:00:00Z").millis() + second * 1_000)
        .unwrap()
        .as_str()
        .to_owned()
}

fn chat(id: &str, role: Role, run: Option<&str>, created: &str, updated: &str) -> ChatMessage {
    ChatMessage {
        id: message_id(id),
        role,
        text: id.into(),
        attachments: vec![],
        context: None,
        run: run.map(run_id),
        streaming: false,
        created_by: None,
        creation_source: None,
        created_at: ts(created),
        updated_at: ts(updated),
        input_intent: None,
    }
}

fn message_entry(entry_id: &str, message: ChatMessage) -> TimelineEntry {
    TimelineEntry {
        id: entry_id.into(),
        created_at: message.created_at.clone(),
        attempt: None,
        kind: TimelineEntryKind::Message {
            message,
            item: None,
        },
    }
}

fn user(entry_id: &str, id: &str, run: Option<&str>, at: &str) -> TimelineEntry {
    message_entry(entry_id, chat(id, Role::User, run, at, at))
}

fn assistant(entry_id: &str, id: &str, run: &str, created: &str, updated: &str) -> TimelineEntry {
    message_entry(
        entry_id,
        chat(id, Role::Assistant, Some(run), created, updated),
    )
}

fn with_message(mut entry: TimelineEntry, change: impl FnOnce(&mut ChatMessage)) -> TimelineEntry {
    if let TimelineEntryKind::Message { message, .. } = &mut entry.kind {
        change(message);
    }
    entry
}

fn streaming(entry: TimelineEntry) -> TimelineEntry {
    with_message(entry, |message| message.streaming = true)
}

fn intent(entry: TimelineEntry, intent: InputIntent) -> TimelineEntry {
    with_message(entry, |message| message.input_intent = Some(intent))
}

fn work(id: &str, created: &str, run: Option<&str>, label: &str, tone: WorkTone) -> WorkLogEntry {
    let mut entry = WorkLogEntry::new(id, ts(created), label, tone);
    entry.run = run.map(run_id);
    entry
}

fn work_entry(entry_id: &str, work: WorkLogEntry) -> TimelineEntry {
    TimelineEntry {
        id: entry_id.into(),
        created_at: work.created_at.clone(),
        attempt: None,
        kind: TimelineEntryKind::Work(Box::new(work)),
    }
}

fn with_work(mut entry: TimelineEntry, change: impl FnOnce(&mut WorkLogEntry)) -> TimelineEntry {
    if let TimelineEntryKind::Work(work) = &mut entry.kind {
        change(work);
    }
    entry
}

fn event_entry(id: &str, created: &str, item: Item) -> TimelineEntry {
    TimelineEntry {
        id: id.into(),
        created_at: ts(created),
        attempt: None,
        kind: TimelineEntryKind::Event(Arc::new(item)),
    }
}

fn latest(
    run: &str,
    status: RunStatus,
    started: Option<&str>,
    completed: Option<&str>,
) -> Option<TimelineLatestRun> {
    Some(TimelineLatestRun {
        run: run_id(run),
        status,
        started_at: started.map(ts),
        completed_at: completed.map(ts),
    })
}

fn input(entries: Vec<TimelineEntry>) -> DesktopTimelineInput {
    DesktopTimelineInput {
        entries,
        ..DesktopTimelineInput::default()
    }
}

fn working(entries: Vec<TimelineEntry>, started: &str) -> DesktopTimelineInput {
    DesktopTimelineInput {
        is_working: true,
        active_turn_started_at: Some(ts(started)),
        ..input(entries)
    }
}

fn runs(ids: &[&str]) -> BTreeSet<RunId> {
    ids.iter().map(|id| run_id(id)).collect()
}

fn ids(rows: &[DesktopRow]) -> Vec<&str> {
    rows.iter().map(DesktopRow::id).collect()
}

/// The reference row kind names.
fn kind(row: &DesktopRow) -> &'static str {
    match row {
        DesktopRow::WorktreeSetup { .. } => "worktree-setup",
        DesktopRow::Work { .. } => "work",
        DesktopRow::WorkLive { .. } => "work-live",
        DesktopRow::Working { .. } => "working",
        DesktopRow::Thinking { .. } => "thinking",
        DesktopRow::WorkToggle { .. } => "work-toggle",
        DesktopRow::TurnFold { .. } => "turn-fold",
        DesktopRow::AttemptFold { .. } => "attempt-fold",
        DesktopRow::ContextCompaction { .. } => "context-compaction",
        DesktopRow::Message { .. } => "message",
        DesktopRow::AssistantMeta { .. } => "assistant-meta",
        DesktopRow::Event { .. } => "event",
        DesktopRow::Handoff { .. } => "handoff",
        DesktopRow::ProposedPlan { .. } => "proposed-plan",
    }
}

fn kinds(rows: &[DesktopRow]) -> Vec<&'static str> {
    rows.iter().map(kind).collect()
}

fn row<'a>(rows: &'a [DesktopRow], id: &str) -> &'a DesktopRow {
    rows.iter()
        .find(|row| row.id() == id)
        .unwrap_or_else(|| panic!("no row {id} in {:?}", ids(rows)))
}

fn first<'a>(rows: &'a [DesktopRow], name: &str) -> &'a DesktopRow {
    rows.iter()
        .find(|row| kind(row) == name)
        .unwrap_or_else(|| panic!("no {name} row in {:?}", kinds(rows)))
}

fn has_kind(rows: &[DesktopRow], name: &str) -> bool {
    rows.iter().any(|row| kind(row) == name)
}

fn grouped_ids(row: &DesktopRow) -> Vec<&str> {
    match row {
        DesktopRow::Work {
            grouped_entries, ..
        }
        | DesktopRow::WorkLive {
            grouped_entries, ..
        } => grouped_entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect(),
        _ => panic!("not a work row: {row:?}"),
    }
}

fn live_entry_id(row: &DesktopRow) -> &str {
    match row {
        DesktopRow::WorkLive { entry, .. } => &entry.id,
        _ => panic!("not a live row: {row:?}"),
    }
}

fn live_rows(rows: &[DesktopRow]) -> Vec<&DesktopRow> {
    rows.iter().filter(|row| kind(row) == "work-live").collect()
}

fn active_live_rows(rows: &[DesktopRow]) -> Vec<&DesktopRow> {
    rows.iter()
        .filter(|row| matches!(row, DesktopRow::WorkLive { active: true, .. }))
        .collect()
}

fn message_of(row: &DesktopRow) -> &ChatMessage {
    match row {
        DesktopRow::Message { message, .. } | DesktopRow::AssistantMeta { message, .. } => message,
        _ => panic!("not a message row: {row:?}"),
    }
}

/// `(show_assistant_meta, show_assistant_copy_button, assistant_copy_streaming)`.
fn message_flags(row: &DesktopRow) -> (bool, bool, bool) {
    match row {
        DesktopRow::Message {
            show_assistant_meta,
            show_assistant_copy_button,
            assistant_copy_streaming,
            ..
        } => (
            *show_assistant_meta,
            *show_assistant_copy_button,
            *assistant_copy_streaming,
        ),
        _ => panic!("not a message row: {row:?}"),
    }
}

fn assistant_rows(rows: &[DesktopRow]) -> Vec<&DesktopRow> {
    rows.iter()
        .filter(|row| {
            matches!(row, DesktopRow::Message { message, .. } if message.role == Role::Assistant)
        })
        .collect()
}

fn fold(row: &DesktopRow) -> (&RunId, &str, bool) {
    match row {
        DesktopRow::TurnFold {
            run,
            label,
            expanded,
            ..
        } => (run, label, *expanded),
        _ => panic!("not a turn fold: {row:?}"),
    }
}

fn toggle(row: &DesktopRow) -> (&str, usize, bool, &str, bool) {
    match row {
        DesktopRow::WorkToggle {
            group_id,
            hidden_count,
            expanded,
            summary,
            has_failure,
            ..
        } => (group_id, *hidden_count, *expanded, summary, *has_failure),
        _ => panic!("not a work toggle: {row:?}"),
    }
}

fn display_label(row: &DesktopRow) -> Option<&str> {
    match row {
        DesktopRow::Work { display_label, .. } => display_label.as_deref(),
        _ => panic!("not a work row: {row:?}"),
    }
}

fn group_id(row: &DesktopRow) -> &str {
    match row {
        DesktopRow::WorkToggle { group_id, .. } | DesktopRow::WorkLive { group_id, .. } => group_id,
        DesktopRow::Thinking {
            group_id: Some(group_id),
            ..
        } => group_id,
        _ => panic!("row has no group: {row:?}"),
    }
}

fn is_expanded_group(row: &DesktopRow) -> bool {
    matches!(
        row,
        DesktopRow::Work {
            is_expanded_tool_group: true,
            ..
        }
    )
}

// Thread state the entry derivation reads.

fn domain_message(id: &str, role: Role, run: &str, text: &str) -> Message {
    Message {
        run: Some(run_id(run)),
        ..fx::message(id, role, text)
    }
}

fn thread(items: Vec<Item>, runs: Vec<Run>, messages: Vec<Message>) -> State {
    State {
        runs,
        messages,
        ..fx::state(items)
    }
}

fn timeline(state: &State) -> Vec<TimelineEntry> {
    derive_timeline_entries(state, &EntriesInput::default())
}

/// The streaming fixture: a settled history run and a live run whose
/// assistant message is still streaming `text`.
struct StreamingFixture {
    state: State,
    run: RunId,
}

const STREAMING_RUN: &str = "live-run";
const HISTORY_RUN: &str = "history-run";

/// `2026-09-04T00:00:SSZ`.
fn streaming_time(second: i64) -> String {
    Timestamp::from_millis(ts("2026-09-04T00:00:00Z").millis() + second * 1_000)
        .unwrap()
        .as_str()
        .to_owned()
}

fn streaming_item(item: Item, run: &str, second: i64) -> Item {
    let at = streaming_time(second);
    item.run(run)
        .ordinal(second as u64)
        .started(&at)
        .completed(Some(&at))
}

fn streaming_fixture(text: &str) -> StreamingFixture {
    let items = vec![
        streaming_item(
            fx::user_message("history-user-item", "history-user"),
            HISTORY_RUN,
            0,
        ),
        streaming_item(fx::command("history-work", "vp test"), HISTORY_RUN, 1)
            .status(ItemStatus::Failed)
            .exit_code(Some(1))
            .text("Test failed"),
        streaming_item(
            fx::assistant_message("history-assistant-item", "history-assistant"),
            HISTORY_RUN,
            3,
        )
        .completed(Some(&streaming_time(4))),
        streaming_item(
            fx::user_message("live-user-item", "live-user"),
            STREAMING_RUN,
            5,
        ),
        streaming_item(
            fx::dynamic_tool(
                "live-work",
                "read_file",
                json!({ "path": "/repo/src/index.ts" }),
                Some(json!("Contents")),
            ),
            STREAMING_RUN,
            6,
        ),
        streaming_item(
            fx::assistant_message("live-assistant-item", "live-assistant"),
            STREAMING_RUN,
            7,
        )
        .status(ItemStatus::Running),
    ];
    let message = |id: &str, role, run, text: &str, second: i64| Message {
        created_at: ts(&streaming_time(second)),
        updated_at: ts(&streaming_time(second)),
        ..domain_message(id, role, run, text)
    };
    let messages = vec![
        message("history-user", Role::User, HISTORY_RUN, "Inspect", 0),
        Message {
            updated_at: ts(&streaming_time(4)),
            ..message("history-assistant", Role::Assistant, HISTORY_RUN, "Done", 3)
        },
        message("live-user", Role::User, STREAMING_RUN, "Continue", 5),
        Message {
            streaming: true,
            ..message("live-assistant", Role::Assistant, STREAMING_RUN, text, 7)
        },
    ];
    StreamingFixture {
        state: thread(
            items,
            vec![
                fx::run(HISTORY_RUN, 1, RunStatus::Completed),
                fx::run(STREAMING_RUN, 2, RunStatus::Running),
            ],
            messages,
        ),
        run: run_id(STREAMING_RUN),
    }
}

impl StreamingFixture {
    fn replace_item(&mut self, id: &str, change: impl FnOnce(&mut Item)) {
        let item = self
            .state
            .items
            .iter_mut()
            .find(|item| item.id.as_str() == id)
            .expect("fixture item");
        change(item);
    }
}

// Expanded tool group scrolling.

fn names(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn follows_appended_calls_only_at_the_hard_end() {
    let entries = names(&["first", "second"]);
    let appended = names(&["first", "second", "third"]);
    assert!(should_follow_work_group_append(&entries, &appended, 0.0));
    assert!(should_follow_work_group_append(&entries, &appended, 0.5));
    assert!(should_follow_work_group_append(&entries, &appended, 1.0));
    assert!(!should_follow_work_group_append(&entries, &appended, 1.01));
    assert!(!should_follow_work_group_append(&entries, &appended, 10.0));
    assert!(!should_follow_work_group_append(
        &entries,
        &appended,
        f64::INFINITY
    ));
}

#[test]
fn does_not_follow_output_updates_prepends_or_replacements() {
    let entries = names(&["first", "second"]);
    assert!(!should_follow_work_group_append(
        &entries,
        &entries.clone(),
        0.0
    ));
    assert!(!should_follow_work_group_append(
        &entries,
        &names(&["older", "first", "second"]),
        0.0
    ));
    assert!(!should_follow_work_group_append(
        &entries,
        &names(&["replacement", "second", "third"]),
        0.0
    ));
    assert!(!should_follow_work_group_append(&[], &entries, 0.0));
}

#[test]
fn restores_the_visible_tool_and_its_offset_inside_expanded_output() {
    let anchor = ExpandedGroupAnchor {
        entry_id: "second".into(),
        offset: 120.0,
    };
    assert_eq!(
        resolve_work_group_scroll_index(&names(&["first", "second"]), Some(&anchor)),
        Some(WorkGroupScrollIndex {
            index: 1,
            view_offset: -120.0
        })
    );
    assert_eq!(
        resolve_work_group_scroll_index(&names(&["older", "first", "second"]), Some(&anchor)),
        Some(WorkGroupScrollIndex {
            index: 2,
            view_offset: -120.0
        })
    );
}

#[test]
fn starts_normally_when_the_saved_tool_no_longer_exists() {
    let entries = names(&["first", "second"]);
    assert_eq!(resolve_work_group_scroll_index(&entries, None), None);
    assert_eq!(
        resolve_work_group_scroll_index(
            &entries,
            Some(&ExpandedGroupAnchor {
                entry_id: "removed".into(),
                offset: 120.0
            })
        ),
        None
    );
}

#[test]
fn renders_a_settled_call_directly_with_its_completed_presentation() {
    let entry = fx::entry("tool-1")
        .item_type(ItemType::DynamicTool)
        .item(fx::dynamic_tool("tool-1", "task_status", json!({}), None));
    let rows = derive_desktop_rows(&input(vec![work_entry("browser-entry", entry)]));
    let direct = first(&rows, "work");
    assert_eq!(grouped_ids(direct), ["tool-1"]);
    assert!(!is_expanded_group(direct));
    assert_eq!(display_label(direct), Some("Got delegated task status"));
}

// Message duration starts.

fn duration_message(
    id: &str,
    role: Role,
    created: &str,
    updated: &str,
    streaming: bool,
) -> ChatMessage {
    ChatMessage {
        streaming,
        ..chat(id, role, None, created, updated)
    }
}

fn duration_starts(messages: &[ChatMessage]) -> Vec<(&str, String)> {
    let refs: Vec<&ChatMessage> = messages.iter().collect();
    let starts = compute_message_duration_start(&refs);
    messages
        .iter()
        .map(|message| (message.id.as_str(), starts[&message.id].as_str().to_owned()))
        .collect()
}

#[test]
fn returns_message_created_at_when_there_is_no_preceding_user_message() {
    let messages = [duration_message(
        "a1",
        Role::Assistant,
        &second(5),
        &second(10),
        false,
    )];
    assert_eq!(duration_starts(&messages), [("a1", second(5))]);
}

#[test]
fn uses_the_user_message_created_at_for_the_first_assistant_response() {
    let messages = [
        duration_message("u1", Role::User, &second(0), &second(0), false),
        duration_message("a1", Role::Assistant, &second(30), &second(30), false),
    ];
    assert_eq!(
        duration_starts(&messages),
        [("u1", second(0)), ("a1", second(0))]
    );
}

#[test]
fn uses_the_previous_completed_assistant_updated_at_for_subsequent_assistant_responses() {
    let messages = [
        duration_message("u1", Role::User, &second(0), &second(0), false),
        duration_message("a1", Role::Assistant, &second(30), &second(30), false),
        duration_message("a2", Role::Assistant, &second(55), &second(55), false),
    ];
    assert_eq!(
        duration_starts(&messages),
        [("u1", second(0)), ("a1", second(0)), ("a2", second(30))]
    );
}

#[test]
fn does_not_advance_the_boundary_for_a_streaming_message() {
    let messages = [
        duration_message("u1", Role::User, &second(0), &second(0), false),
        duration_message("a1", Role::Assistant, &second(30), &second(40), true),
        duration_message("a2", Role::Assistant, &second(55), &second(55), false),
    ];
    assert_eq!(
        duration_starts(&messages),
        [("u1", second(0)), ("a1", second(0)), ("a2", second(0))]
    );
}

#[test]
fn resets_the_boundary_on_a_new_user_message() {
    let messages = [
        duration_message("u1", Role::User, &second(0), &second(0), false),
        duration_message("a1", Role::Assistant, &second(30), &second(30), false),
        duration_message("u2", Role::User, &second(60), &second(60), false),
        duration_message("a2", Role::Assistant, &second(80), &second(80), false),
    ];
    assert_eq!(
        duration_starts(&messages),
        [
            ("u1", second(0)),
            ("a1", second(0)),
            ("u2", second(60)),
            ("a2", second(60))
        ]
    );
}

#[test]
fn handles_system_messages_without_affecting_the_boundary() {
    let messages = [
        duration_message("u1", Role::User, &second(0), &second(0), false),
        duration_message("s1", Role::System, &second(1), &second(1), false),
        duration_message("a1", Role::Assistant, &second(30), &second(30), false),
    ];
    assert_eq!(
        duration_starts(&messages),
        [("u1", second(0)), ("s1", second(0)), ("a1", second(0))]
    );
}

#[test]
fn returns_empty_map_for_empty_input() {
    assert!(compute_message_duration_start(&[]).is_empty());
}

// Row derivation.

#[test]
fn stops_stranded_thinking_after_a_steer_and_follows_the_next_thought_or_tool() {
    let run = "steered-run";
    let at = "2026-10-01T06:19:10Z";
    let thought = |id: &str| {
        work_entry(
            id,
            work(id, at, Some(run), "Thinking", WorkTone::Thinking)
                .item_type(ItemType::Reasoning)
                .detail(id)
                .status(ToolLifecycleStatus::InProgress),
        )
    };
    let steer = intent(user("steer", "steer", Some(run), at), InputIntent::Steer);
    let first_thought = thought("first-thought");
    let next = thought("next-thought");
    let tool = work_entry(
        "tool",
        work("tool", at, Some(run), "Running cat", WorkTone::Tool)
            .command("cat file")
            .status(ToolLifecycleStatus::InProgress),
    );
    let rows = |entries: Vec<TimelineEntry>| {
        derive_desktop_rows(&DesktopTimelineInput {
            running_run: Some(run_id(run)),
            ..working(entries, at)
        })
    };
    let only_thought = rows(vec![first_thought.clone()]);
    let live = first(&only_thought, "work-live");
    assert!(matches!(live, DesktopRow::WorkLive { active: true, .. }));
    assert_eq!(live_entry_id(live), "first-thought");

    let after_steer = rows(vec![first_thought.clone(), steer.clone()]);
    assert!(!has_kind(&after_steer, "work-live"));
    assert_eq!(kind(after_steer.last().unwrap()), "thinking");
    let DesktopRow::Work {
        grouped_entries, ..
    } = first(&after_steer, "work")
    else {
        unreachable!()
    };
    assert_eq!(grouped_entries.len(), 1);
    assert_eq!(grouped_entries[0].id, "first-thought");
    assert_eq!(
        grouped_entries[0].tool_lifecycle_status,
        Some(ToolLifecycleStatus::Completed)
    );

    for entries in [
        vec![first_thought.clone(), steer.clone(), next.clone()],
        vec![first_thought.clone(), next.clone()],
        vec![first_thought.clone(), steer, next, tool],
    ] {
        let last = entries.last().unwrap().id.clone();
        let rows = rows(entries);
        let live = active_live_rows(&rows);
        assert_eq!(live.len(), 1);
        assert_eq!(live_entry_id(live[0]), last);
    }
    // Presentation must not rewrite the retained provider lifecycle.
    assert_eq!(
        first_thought.work().unwrap().tool_lifecycle_status,
        Some(ToolLifecycleStatus::InProgress)
    );
}

/// The streaming fixture's tool calls, alone in a running thread.
fn live_tool_state(items: Vec<Item>) -> State {
    thread(
        items,
        vec![fx::run(STREAMING_RUN, 1, RunStatus::Running)],
        vec![],
    )
}

fn live_tool(id: &str, name: &str, input: Value, output: Option<Value>, ordinal: i64) -> Item {
    streaming_item(
        fx::dynamic_tool(id, name, input, output),
        STREAMING_RUN,
        ordinal,
    )
}

#[test]
fn shows_the_cua_action_title_for_a_retained_tool_item() {
    let state = live_tool_state(vec![live_tool(
        "live-work",
        "cua_repl.js",
        json!({ "code": "await game.getAXStateAndScreenshot();", "title": "Inspect Saga music screen" }),
        Some(json!("Contents")),
        6,
    )]);
    let entries = timeline(&state);
    let work = entries.iter().find_map(TimelineEntry::work).unwrap();
    assert_eq!(
        work_entry_display_label(work, None),
        "Inspect Saga music screen"
    );
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        running_run: Some(run_id(STREAMING_RUN)),
        active_turn_started_at: Some(ts(&streaming_time(0))),
        ..input(entries)
    });
    assert_eq!(
        display_label(first(&rows, "work")),
        Some("Inspect Saga music screen")
    );
}

#[test]
fn presents_project_mcp_calls_and_summarizes_successful_registrations() {
    let titled = |item: Item| {
        let mut item = item;
        if let ItemKind::DynamicTool { presentation, .. } = &mut item.kind {
            presentation.title = Some("Custom provider title".into());
        }
        item
    };
    let state = live_tool_state(vec![
        titled(live_tool(
            "list",
            "orchestration.project_list",
            json!({}),
            Some(json!({ "projects": [] })),
            1,
        )),
        titled(live_tool(
            "create",
            "mcp__orchestration__project_create",
            json!({}),
            Some(json!({ "id": "project-1" })),
            2,
        )),
        titled(live_tool(
            "failed-create",
            "project_create",
            json!({}),
            Some(json!({ "isError": true })),
            3,
        )),
    ]);
    let entries = timeline(&state);
    let work: Vec<&WorkLogEntry> = entries.iter().filter_map(TimelineEntry::work).collect();
    assert_eq!(work_entry_display_label(work[0], None), "Listed projects");
    assert_eq!(
        work_entry_display_label(work[1], None),
        "Registered a project"
    );
    assert_eq!(
        work_entry_display_label(work[2], None),
        "Failed to register a project"
    );
    assert_eq!(
        crate::view::work_log::tool_catalog::resolve_tool_presentation(Some(
            "mcp__orchestration__project_create"
        ))
        .map(|presentation| presentation.logo),
        Some(ToolLogo::App)
    );
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        running_run: Some(run_id(STREAMING_RUN)),
        active_turn_started_at: Some(ts(&streaming_time(0))),
        ..input(entries)
    });
    let (_, _, _, summary, has_failure) = toggle(first(&rows, "work-toggle"));
    assert_eq!(summary, "Listed projects 1 time and registered 1 project");
    assert!(has_failure);
}

#[test]
fn groups_approval_and_user_input_requests_with_commands_without_expanding_them() {
    for status in [ItemStatus::Waiting, ItemStatus::Completed] {
        let at =
            |item: Item, ordinal: i64| streaming_item(item, STREAMING_RUN, ordinal).status(status);
        let state = State {
            requests: vec![
                fx::approval("approval-request", "command", "Allow pwd?"),
                fx::questions("input-request", vec![]),
            ],
            ..live_tool_state(vec![
                at(fx::command("command", "pwd").exit_code(None), 1),
                at(fx::approval_item("approval", "approval-request"), 2),
                at(fx::user_input_item("input", "input-request"), 3),
            ])
        };
        let entries = timeline(&state);
        assert!(entries.iter().all(|entry| entry.work().is_some()));
        assert_eq!(entries.len(), 3);
        let base = DesktopTimelineInput {
            is_working: status == ItemStatus::Waiting,
            running_run: Some(run_id(STREAMING_RUN)),
            active_turn_started_at: Some(ts(&streaming_time(0))),
            ..input(entries)
        };
        let rows = derive_desktop_rows(&base);
        let work_rows: Vec<&DesktopRow> = rows
            .iter()
            .filter(|row| matches!(kind(row), "work-toggle" | "work-live"))
            .collect();
        assert_eq!(work_rows.len(), 1, "{status:?}");
        assert!(matches!(
            work_rows[0],
            DesktopRow::WorkToggle {
                expanded: false,
                ..
            } | DesktopRow::WorkLive {
                expanded: false,
                ..
            }
        ));
        let expanded_rows = derive_desktop_rows(&DesktopTimelineInput {
            expanded_work_groups: [group_id(work_rows[0]).to_owned()].into(),
            ..base.clone()
        });
        let DesktopRow::Work {
            grouped_entries, ..
        } = expanded_rows
            .iter()
            .find(|row| is_expanded_group(row))
            .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            grouped_entries
                .iter()
                .map(|entry| entry.item_type)
                .collect::<Vec<_>>(),
            [
                Some(ItemType::CommandExecution),
                Some(ItemType::ApprovalRequest),
                Some(ItemType::UserInputRequest)
            ]
        );
        assert!(!has_kind(&rows, "event"));
    }
}

#[test]
fn keeps_system_notices_visible_as_non_failing_warnings_through_work() {
    let message =
        "Claude changed its safety mode.\nReview the active permission settings before continuing.";
    let at = streaming_time(1);
    let state = live_tool_state(vec![streaming_item(
        fx::system_notice("safety-notice", message),
        STREAMING_RUN,
        1,
    )]);
    let entries = timeline(&state);
    let [notice] = entries.as_slice() else {
        panic!("expected the notice entry");
    };
    let notice_work = notice.work().expect("a notice work entry");
    assert_eq!(
        notice_work.item.as_ref().map(|item| item.id.as_str()),
        Some("safety-notice")
    );
    assert_eq!(
        notice_work.source_activity,
        Some(SourceActivity::RuntimeWarning)
    );
    assert_eq!(work_entry_display_label(notice_work, None), message);
    assert!(!work_entry_display_indicates_tool_failure(notice_work));
    for state in ["running", "completed", "superseded"] {
        let running = state == "running";
        let mut notice = notice.clone();
        if state == "superseded" {
            notice.attempt = Some(TimelineAttempt {
                id: RunAttemptId::new("old-attempt").unwrap(),
                run: run_id(STREAMING_RUN),
                ordinal: 0,
                status: AttemptStatus::Superseded,
            });
        }
        let terminal = assistant(
            "terminal-message",
            "terminal-message",
            STREAMING_RUN,
            &at,
            &at,
        );
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            is_working: running,
            active_turn_started_at: Some(ts(&at)),
            latest_run: latest(
                STREAMING_RUN,
                if running {
                    RunStatus::Running
                } else {
                    RunStatus::Completed
                },
                Some(&at),
                (!running).then_some(at.as_str()),
            ),
            ..input(vec![
                notice,
                work_entry(
                    "nearby-tool",
                    work(
                        "nearby-tool",
                        &at,
                        Some(STREAMING_RUN),
                        "Ran command",
                        WorkTone::Tool,
                    )
                    .status(ToolLifecycleStatus::Completed),
                ),
                if running {
                    streaming(terminal)
                } else {
                    terminal
                },
            ])
        });
        let notice_row = rows
            .iter()
            .find(|row| {
                matches!(row, DesktopRow::Work { grouped_entries, .. }
                    if grouped_entries.iter().any(|entry| entry.id == "safety-notice"))
            })
            .unwrap_or_else(|| panic!("{state}: {:?}", ids(&rows)));
        assert_eq!(grouped_ids(notice_row), ["safety-notice"], "{state}");
    }
}

#[test]
fn keeps_context_compaction_visible_outside_folded_work() {
    let mut compaction = work(
        "compaction",
        "2026-01-01T00:00:00Z",
        None,
        "Compacted context 899K → 19K tokens",
        WorkTone::Info,
    );
    compaction.source_activity = Some(SourceActivity::ContextCompaction);
    let rows = derive_desktop_rows(&input(vec![work_entry("compaction-entry", compaction)]));
    assert_eq!(
        rows,
        [DesktopRow::ContextCompaction {
            id: "compaction-entry".into(),
            created_at: ts("2026-01-01T00:00:00Z"),
            label: "Compacted context 899K → 19K tokens".into(),
            active: false,
        }]
    );
}

fn compaction_label_and_active(rows: &[DesktopRow]) -> (&str, bool) {
    match first(rows, "context-compaction") {
        DesktopRow::ContextCompaction { label, active, .. } => (label, *active),
        _ => unreachable!(),
    }
}

#[test]
fn gives_live_compaction_the_activity_slot_and_restores_thinking_when_it_completes() {
    let compacting = |status: ItemStatus, after: Option<u64>| {
        let mut fixture = streaming_fixture("");
        fixture.replace_item("live-assistant-item", |item| {
            item.kind = ItemKind::Compaction {
                before: Some(899_000),
                after,
            };
            item.status = status;
            item.completed_at = status.terminal().then(|| item.started_at.clone());
        });
        timeline(&fixture.state)
    };
    let base = DesktopTimelineInput {
        running_run: Some(run_id(STREAMING_RUN)),
        ..working(vec![], &streaming_time(5))
    };
    let running_entries = compacting(ItemStatus::Running, None);
    let running_rows = derive_desktop_rows(&DesktopTimelineInput {
        entries: running_entries.clone(),
        ..base.clone()
    });
    assert_eq!(
        compaction_label_and_active(&running_rows),
        ("Compacting context", true)
    );
    assert!(!has_kind(&running_rows, "thinking"));

    let completed_rows = derive_desktop_rows(&DesktopTimelineInput {
        entries: compacting(ItemStatus::Completed, Some(19_000)),
        ..base.clone()
    });
    assert_eq!(
        compaction_label_and_active(&completed_rows),
        ("Context compacted 899K → 19K tokens", false)
    );
    assert_eq!(kind(completed_rows.last().unwrap()), "thinking");

    let settled_rows = derive_desktop_rows(&DesktopTimelineInput {
        entries: running_entries,
        is_working: false,
        ..base
    });
    assert!(!compaction_label_and_active(&settled_rows).1);
}

#[test]
fn only_enables_assistant_copy_for_the_terminal_assistant_message_in_a_turn() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&["turn-1"]),
        ..input(vec![
            user("user-1-entry", "user-1", None, &second(0)),
            assistant(
                "assistant-thought-entry",
                "assistant-thought",
                "turn-1",
                &second(10),
                &second(11),
            ),
            assistant(
                "assistant-final-entry",
                "assistant-final",
                "turn-1",
                &second(20),
                &second(30),
            ),
        ])
    });
    let assistants = assistant_rows(&rows);
    assert_eq!(assistants.len(), 2);
    assert!(!message_flags(assistants[0]).1);
    assert!(message_flags(assistants[1]).1);
}

#[test]
fn marks_only_the_active_assistant_turn_as_streaming_for_copy_controls() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("turn-2", RunStatus::Running, Some(&second(19)), None),
        ..input(vec![
            assistant(
                "assistant-one-entry",
                "assistant-one",
                "turn-1",
                &second(10),
                &second(11),
            ),
            assistant(
                "assistant-two-entry",
                "assistant-two",
                "turn-2",
                &second(20),
                &second(30),
            ),
        ])
    });
    let assistants = assistant_rows(&rows);
    assert!(!message_flags(assistants[0]).2);
    assert!(message_flags(assistants[1]).2);
}

#[test]
fn projects_assistant_diff_summaries_and_user_revert_counts_onto_the_affected_rows() {
    let files = vec![CheckpointFile {
        path: "src/index.ts".into(),
        kind: "modified".into(),
        additions: 3,
        deletions: 1,
    }];
    let summary = TurnDiffSummary {
        checkpoint: agent_domain::CheckpointId::new("checkpoint-1").unwrap(),
        run: run_id("turn-1"),
        checkpoint_turn_count: 2,
        status: CheckpointStatus::Ready,
        files: files.clone(),
        assistant_message: Some(message_id("assistant-1")),
    };
    let checkpoint = Checkpoint {
        status: CheckpointStatus::Ready,
        scope: None,
        id: summary.checkpoint.clone(),
        run: Some(run_id("turn-1")),
        run_ordinal: 2,
        native_heads: BTreeMap::new(),
        file_ref: "checkpoint-1".into(),
        files,
    };
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        turn_diffs: vec![AssistantTurnDiff {
            message: message_id("assistant-1"),
            summary: summary.clone(),
        }],
        checkpoints: vec![checkpoint],
        supports_conversation_rollback: true,
        ..input(vec![
            intent(
                user("user-entry", "user-1", Some("turn-1"), &second(0)),
                InputIntent::TurnStart,
            ),
            assistant(
                "assistant-entry",
                "assistant-1",
                "turn-1",
                &second(20),
                &second(30),
            ),
        ])
    });
    let DesktopRow::Message {
        revert_turn_count, ..
    } = row(&rows, "user-entry")
    else {
        unreachable!()
    };
    assert_eq!(*revert_turn_count, Some(1));
    let DesktopRow::Message {
        assistant_turn_diff,
        ..
    } = row(&rows, "assistant-entry")
    else {
        unreachable!()
    };
    assert_eq!(assistant_turn_diff.as_ref(), Some(&summary));
}

fn ran_command(id: &str, created: &str, run: &str) -> WorkLogEntry {
    work(id, created, Some(run), "Ran command", WorkTone::Tool)
}

#[test]
fn folds_the_first_assistant_message_and_settled_work_before_the_terminal_response() {
    let entries = vec![
        user("user-entry", "user-1", None, &second(0)),
        with_message(
            assistant(
                "assistant-first-entry",
                "assistant-first",
                "turn-1",
                &second(5),
                &second(6),
            ),
            |message| {
                message.text =
                    "Synthetic deployment checklist\n1. Confirm the deployment is ready.".into()
            },
        ),
        work_entry("work-entry-1", ran_command("work-1", &second(8), "turn-1")),
        assistant(
            "assistant-final-entry",
            "assistant-final",
            "turn-1",
            &second(20),
            &second(22),
        ),
    ];
    let collapsed = derive_desktop_rows(&input(entries.clone()));
    let (run, label, expanded) = fold(first(&collapsed, "turn-fold"));
    assert_eq!(run.as_str(), "turn-1");
    assert!(!expanded);
    // User message boundary (00:00:00) → terminal message updatedAt (00:00:22).
    assert_eq!(label, "Worked for 22s");
    assert_eq!(
        ids(&collapsed),
        ["user-entry", "turn-fold:turn-1", "assistant-final-entry"]
    );

    let expanded_rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&["turn-1"]),
        ..input(entries)
    });
    assert_eq!(
        ids(&expanded_rows),
        [
            "user-entry",
            "turn-fold:turn-1",
            "assistant-first-entry",
            "work-entry-1",
            "assistant-final-entry"
        ]
    );
    assert!(
        expanded_rows
            .iter()
            .any(|row| matches!(row, DesktopRow::TurnFold { expanded: true, .. }))
    );
}

#[test]
fn folds_completed_activities_after_the_terminal_response() {
    for count in 1..=3 {
        let mut entries = vec![
            work_entry(
                "work-entry-before-text",
                work(
                    "work-before-text",
                    &second(1),
                    Some("turn-1"),
                    "Status updated",
                    WorkTone::Info,
                ),
            ),
            assistant(
                "assistant-final-entry",
                "assistant-final",
                "turn-1",
                &second(5),
                &second(6),
            ),
        ];
        for index in 0..count {
            entries.push(work_entry(
                &format!("work-entry-after-text-{index}"),
                ran_command(
                    &format!("work-after-text-{index}"),
                    &second(index + 7),
                    "turn-1",
                )
                .item_type(ItemType::CommandExecution)
                .status(ToolLifecycleStatus::Completed),
            ));
        }
        let base = DesktopTimelineInput {
            latest_run: latest(
                "turn-1",
                RunStatus::Completed,
                Some(&second(0)),
                Some(&second(10)),
            ),
            ..input(entries.clone())
        };
        let rows = derive_desktop_rows(&base);
        assert_eq!(ids(&rows), ["turn-fold:turn-1", "assistant-final-entry"]);
        let expanded = derive_desktop_rows(&DesktopTimelineInput {
            expanded_runs: runs(&["turn-1"]),
            ..base.clone()
        });
        assert!(expanded.iter().any(|row| {
            row.id() == "work-entry-after-text-0"
                || matches!(row, DesktopRow::WorkToggle { id, hidden_count, .. }
                    if id == "work-toggle:work-entry-after-text-0" && *hidden_count == count as usize)
        }));

        // A late failure must remain visible even though successful work is folded.
        let with_status = |status| {
            entries
                .iter()
                .cloned()
                .map(|entry| {
                    if entry.id == "work-entry-after-text-0" {
                        with_work(entry, |work| work.tool_lifecycle_status = Some(status))
                    } else {
                        entry
                    }
                })
                .collect::<Vec<_>>()
        };
        let failed_rows = derive_desktop_rows(&DesktopTimelineInput {
            entries: with_status(ToolLifecycleStatus::Failed),
            ..base.clone()
        });
        assert!(
            failed_rows
                .iter()
                .any(|row| row.id() == "work-entry-after-text-0")
        );

        let pending_rows = derive_desktop_rows(&DesktopTimelineInput {
            entries: with_status(ToolLifecycleStatus::InProgress),
            latest_run: latest("turn-1", RunStatus::Running, Some(&second(0)), None),
            is_working: true,
            active_turn_started_at: Some(ts(&second(0))),
            ..base
        });
        assert!(
            live_rows(&pending_rows)
                .iter()
                .any(|row| live_entry_id(row) == "work-after-text-0")
        );
    }
}

#[test]
fn folds_all_assistant_messages_before_the_terminal_message() {
    let rows = derive_desktop_rows(&input(vec![
        assistant(
            "assistant-first-entry",
            "assistant-first",
            "turn-1",
            &second(1),
            &second(2),
        ),
        assistant(
            "assistant-middle-entry",
            "assistant-middle",
            "turn-1",
            &second(3),
            &second(4),
        ),
        assistant(
            "assistant-final-entry",
            "assistant-final",
            "turn-1",
            &second(5),
            &second(6),
        ),
    ]));
    assert_eq!(ids(&rows), ["turn-fold:turn-1", "assistant-final-entry"]);
}

#[test]
fn derives_a_sane_duration_for_a_steer_superseded_turn_with_one_instant_commentary_message() {
    // A steer ends the previous turn early: its only message completes the
    // instant it is created, and trailing work lands after it. The fold spans
    // from the user message that started the turn to the last entry.
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("turn-2", RunStatus::Running, Some(&second(14)), None),
        ..working(
            vec![
                user("user-entry", "user-1", None, &second(0)),
                work_entry(
                    "work-entry-before-message",
                    work(
                        "work-before-message",
                        &second(7),
                        Some("turn-1"),
                        "Status updated",
                        WorkTone::Info,
                    ),
                ),
                assistant(
                    "assistant-commentary-entry",
                    "assistant-commentary",
                    "turn-1",
                    &second(9),
                    &second(9),
                ),
                work_entry("work-entry-1", ran_command("work-1", &second(12), "turn-1")),
                user("steer-user-entry", "user-2", None, &second(14)),
                streaming(assistant(
                    "assistant-next-turn-entry",
                    "assistant-next",
                    "turn-2",
                    &second(17),
                    &second(17),
                )),
            ],
            &second(14),
        )
    });
    let (run, label, _) = fold(first(&rows, "turn-fold"));
    // User message (00:00:00) → trailing work entry (00:00:12).
    assert_eq!(run.as_str(), "turn-1");
    assert_eq!(label, "Worked for 12s");
}

#[test]
fn uses_latest_turn_timings_and_the_stopped_label_for_an_interrupted_latest_turn() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest(
            "turn-1",
            RunStatus::Interrupted,
            Some(&second(0)),
            Some(&second(47)),
        ),
        ..input(vec![work_entry(
            "work-entry-1",
            ran_command("work-1", &second(5), "turn-1"),
        )])
    });
    assert_eq!(rows.len(), 1);
    let (run, label, expanded) = fold(&rows[0]);
    assert_eq!(run.as_str(), "turn-1");
    assert_eq!(label, "You stopped after 47s");
    assert!(!expanded);
}

#[test]
fn keeps_the_working_header_below_the_initiating_prompt_across_repeated_steers() {
    let run = "steered-run";
    let started = second(0);
    let mut entries = vec![user(
        "initial-prompt",
        "initial-prompt",
        Some(run),
        &started,
    )];
    for index in 0..3 {
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            latest_run: latest(run, RunStatus::Running, Some(&started), None),
            ..working(entries.clone(), &started)
        });
        assert_eq!(ids(&rows[..2]), ["initial-prompt", "working-indicator-row"]);
        let working_rows: Vec<&DesktopRow> =
            rows.iter().filter(|row| kind(row) == "working").collect();
        assert_eq!(working_rows.len(), 1);
        assert_eq!(working_rows[0].created_at(), Some(&ts(&started)));
        assert_eq!(
            rows.iter()
                .filter(|row| kind(row) == "message")
                .map(DesktopRow::id)
                .collect::<Vec<_>>(),
            entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>()
        );
        let id = format!("steer-{index}");
        entries.push(intent(
            user(&id, &id, Some(run), &started),
            InputIntent::Steer,
        ));
    }
}

#[test]
fn keeps_the_duration_header_at_the_initiating_prompt_when_a_steer_arrives_before_output() {
    let run = "steered-run";
    for steer_intent in [InputIntent::Steer, InputIntent::PromotedQueuedToSteer] {
        let entries = vec![
            intent(
                user("initial-prompt", "initial-prompt", Some(run), &second(0)),
                InputIntent::TurnStart,
            ),
            intent(user("steer", "steer", Some(run), &second(5)), steer_intent),
            work_entry(
                "work",
                work("work", &second(8), Some(run), "Ran command", WorkTone::Tool),
            ),
            assistant("final", "final", run, &second(20), &second(20)),
        ];
        for is_working in [true, false] {
            for expanded in [true, false] {
                let rows = derive_desktop_rows(&DesktopTimelineInput {
                    latest_run: latest(
                        run,
                        if is_working {
                            RunStatus::Running
                        } else {
                            RunStatus::Completed
                        },
                        Some(&second(0)),
                        (!is_working).then(|| second(20)).as_deref(),
                    ),
                    is_working,
                    active_turn_started_at: Some(ts(&second(0))),
                    expanded_runs: if expanded { runs(&[run]) } else { runs(&[]) },
                    ..input(entries.clone())
                });
                let fold_id = format!("turn-fold:{run}");
                assert_eq!(
                    ids(&rows[..3]),
                    [
                        "initial-prompt",
                        if is_working {
                            "working-indicator-row"
                        } else {
                            fold_id.as_str()
                        },
                        "steer"
                    ]
                );
                assert_eq!(rows[1].created_at(), Some(&ts(&second(0))));
                if !is_working {
                    let (_, label, fold_expanded) = fold(&rows[1]);
                    assert_eq!(label, "Worked for 20s");
                    assert_eq!(fold_expanded, expanded);
                }
                assert!(rows.iter().any(|row| row.id() == "final"));
                assert_eq!(
                    rows.iter().any(|row| row.id() == "work"),
                    is_working || expanded
                );
            }
        }
    }
}

#[test]
fn keeps_the_previous_turn_folded_while_a_newly_sent_message_awaits_its_turn() {
    // Right after a send the client is working but the latest run is still the
    // previous, settled one; it must stay folded through that window.
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest(
            "turn-1",
            RunStatus::Completed,
            Some(&second(0)),
            Some(&second(22)),
        ),
        ..working(
            vec![
                work_entry("work-entry-1", ran_command("work-1", &second(5), "turn-1")),
                assistant(
                    "assistant-final-entry",
                    "assistant-final",
                    "turn-1",
                    &second(20),
                    &second(22),
                ),
                user("user-followup-entry", "user-followup", None, &second(60)),
            ],
            &second(60),
        )
    });
    assert_eq!(
        ids(&rows),
        [
            "turn-fold:turn-1",
            "assistant-final-entry",
            "user-followup-entry",
            "working-indicator-row",
            "live-activity-row"
        ]
    );
    assert!(message_flags(row(&rows, "assistant-final-entry")).0);
    assert_eq!(kind(rows.last().unwrap()), "thinking");
}

#[test]
fn does_not_fold_the_active_in_progress_turn() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("turn-1", RunStatus::Running, Some(&second(0)), None),
        ..working(
            vec![
                assistant(
                    "assistant-thought-entry",
                    "assistant-thought",
                    "turn-1",
                    &second(5),
                    &second(6),
                ),
                work_entry("work-entry-1", ran_command("work-1", &second(8), "turn-1")),
            ],
            &second(0),
        )
    });
    assert!(!has_kind(&rows, "turn-fold"));
    assert_eq!(
        ids(&rows),
        [
            "working-indicator-row",
            "assistant-thought-entry",
            "live-activity-row"
        ]
    );
}

#[test]
fn keeps_a_promptless_restart_in_one_active_visual_response() {
    let command = |id: &str, created: &str, run: &str, label: &str, command: &str, status| {
        work_entry(
            &format!("{id}-entry"),
            work(id, created, Some(run), label, WorkTone::Tool)
                .command(command)
                .status(status),
        )
    };
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest(
            "turn-after-restart",
            RunStatus::Running,
            Some(&second(60)),
            None,
        ),
        ..working(
            vec![
                user("user-entry", "user-1", None, &second(0)),
                command(
                    "old-work",
                    &second(5),
                    "turn-before-restart",
                    "Searched files",
                    "rg restart",
                    ToolLifecycleStatus::Completed,
                ),
                command(
                    "old-stale-work",
                    &second(6),
                    "turn-before-restart",
                    "Running stale command",
                    "rg stale",
                    ToolLifecycleStatus::InProgress,
                ),
                assistant(
                    "old-commentary-entry",
                    "old-commentary",
                    "turn-before-restart",
                    &second(8),
                    &second(8),
                ),
                command(
                    "new-work",
                    &second(65),
                    "turn-after-restart",
                    "Running tests",
                    "vp test run",
                    ToolLifecycleStatus::InProgress,
                ),
            ],
            &second(60),
        )
    });
    assert!(!has_kind(&rows, "turn-fold"));
    let row_ids = ids(&rows);
    assert_eq!(
        row_ids
            .iter()
            .filter(|id| **id == "working-indicator-row")
            .count(),
        1
    );
    let position = |id: &str| row_ids.iter().position(|row| *row == id).unwrap();
    assert!(position("working-indicator-row") < position("old-work-entry"));
    assert_eq!(
        row(&rows, "working-indicator-row").created_at(),
        Some(&ts(&second(60)))
    );
    assert_eq!(
        message_flags(row(&rows, "old-commentary-entry")),
        (false, false, true)
    );
    let live = active_live_rows(&rows);
    assert_eq!(live.len(), 1);
    assert_eq!(live_entry_id(live[0]), "new-work");
    assert!(!has_kind(&rows, "thinking"));
}

#[test]
fn keeps_an_actually_running_tool_in_the_shared_activity_row() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("turn-1", RunStatus::Running, Some(&second(0)), None),
        ..working(
            vec![
                work_entry(
                    "running-command-entry",
                    work(
                        "running-command",
                        &second(5),
                        Some("turn-1"),
                        "Running rg",
                        WorkTone::Tool,
                    )
                    .command("rg toolCall")
                    .request_kind("command")
                    .status(ToolLifecycleStatus::InProgress),
                ),
                work_entry(
                    "completed-edit-entry",
                    work(
                        "completed-edit",
                        &second(6),
                        Some("turn-1"),
                        "Edited files",
                        WorkTone::Tool,
                    )
                    .request_kind("file-change")
                    .changed_files(&["src/one.ts", "src/two.ts"])
                    .status(ToolLifecycleStatus::Completed),
                ),
                work_entry(
                    "completed-command-entry",
                    work(
                        "completed-command",
                        &second(7),
                        Some("turn-1"),
                        "Ran tests",
                        WorkTone::Tool,
                    )
                    .command("vp test run")
                    .request_kind("command")
                    .status(ToolLifecycleStatus::Completed),
                ),
            ],
            &second(0),
        )
    });
    assert_eq!(kinds(&rows), ["working", "work-live"]);
    assert!(!has_kind(&rows, "thinking"));
    let live = first(&rows, "work-live");
    assert_eq!(live_entry_id(live), "running-command");
    assert!(matches!(live, DesktopRow::WorkLive { active: true, .. }));
    assert_eq!(
        grouped_ids(live),
        ["running-command", "completed-edit", "completed-command"]
    );
}

/// The row shapes the subagent fold tests compare.
fn shape(rows: &[DesktopRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row {
            DesktopRow::TurnFold { label, .. } => format!("fold:{label}"),
            DesktopRow::Message { message, .. } => format!(
                "{}:{}",
                match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                },
                message.id
            ),
            row => kind(row).into(),
        })
        .collect()
}

#[test]
fn folds_each_run_of_a_provider_native_subagent_thread_like_a_normal_turn() {
    // A subagent's child thread: no runs, a user prompt for the launch and for
    // a resume.
    let at = |second: i64| {
        Timestamp::from_millis(ts("2026-09-25T22:51:00Z").millis() + second * 1_000)
            .unwrap()
            .as_str()
            .to_owned()
    };
    let base = |item: Item, ordinal: u64, second: i64| {
        item.ordinal(ordinal)
            .started(&at(second))
            .completed(Some(&at(second)))
    };
    #[derive(Clone, Copy, PartialEq)]
    enum Resume {
        Running,
        Completed,
        Failed,
    }
    let items = |resume: Resume| {
        let mut items = vec![
            base(fx::user_message("launch", "launch"), 1, 0).text("Prompt launch"),
            base(fx::command("launch-ls", "ls src"), 2, 4),
            base(fx::reasoning("launch-thinking", "Not there."), 3, 8),
            base(
                fx::assistant_message("launch-answer", "launch-answer"),
                4,
                8,
            )
            .text("Answer launch-answer"),
            base(fx::user_message("resume", "resume"), 5, 72).text("Prompt resume"),
        ];
        let resume_ls = base(fx::command("resume-ls", "ls src"), 6, 77);
        items.push(if resume == Resume::Running {
            resume_ls.status(ItemStatus::Running)
        } else {
            resume_ls
        });
        if resume == Resume::Failed {
            let mut error = base(fx::error("resume-error", "Subagent failed"), 7, 80)
                .status(ItemStatus::Failed);
            if let ItemKind::Error { class, .. } = &mut error.kind {
                *class = Some("provider_error".into());
            }
            items.push(error);
        }
        if resume != Resume::Running {
            items.push(
                base(
                    fx::assistant_message("resume-answer", "resume-answer"),
                    8,
                    80,
                )
                .text("Answer resume-answer"),
            );
        }
        items
    };
    let rows = |resume: Resume, working: bool, expanded_runs: BTreeSet<RunId>| {
        derive_desktop_rows(&DesktopTimelineInput {
            entries: timeline(&fx::state(items(resume))),
            is_working: working,
            runless_work_active: working,
            expanded_runs,
            active_turn_started_at: working.then(|| ts(&at(72))),
            ..DesktopTimelineInput::default()
        })
    };

    // Settled: each run folds its work, keeping its prompt and final answer.
    let settled = rows(Resume::Completed, false, BTreeSet::new());
    assert_eq!(
        shape(&settled),
        [
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "fold:Worked for 8.0s",
            "assistant:resume-answer"
        ]
    );

    // Each fold opens on its own.
    let (launch_run, _, _) = fold(first(&settled, "turn-fold"));
    let expanded = rows(
        Resume::Completed,
        false,
        [launch_run.clone()].into_iter().collect(),
    );
    assert_eq!(
        shape(&expanded),
        [
            "user:launch",
            "fold:Worked for 8.0s",
            "work-toggle",
            "assistant:launch-answer",
            "user:resume",
            "fold:Worked for 8.0s",
            "assistant:resume-answer"
        ]
    );

    // While the resume runs, only the settled launch folds.
    assert_eq!(
        shape(&rows(Resume::Running, true, BTreeSet::new())),
        [
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "working",
            "work-live"
        ]
    );

    // A failed run stays open, as on a normal thread.
    assert_eq!(
        shape(&rows(Resume::Failed, false, BTreeSet::new())),
        [
            "user:launch",
            "fold:Worked for 8.0s",
            "assistant:launch-answer",
            "user:resume",
            "work",
            "work",
            "assistant:resume-answer"
        ]
    );
}

#[test]
fn keeps_runless_turns_folded_once_the_threads_first_run_starts() {
    let message = |id: &str, role: Role, at: i64, run: Option<&str>| {
        message_entry(id, chat(id, role, run, &second(at), &second(at)))
    };
    let rows = |tail: Vec<TimelineEntry>| {
        let mut entries = vec![
            message("imported-prompt", Role::User, 0, None),
            message("imported-update", Role::Assistant, 4, None),
            work_entry(
                "imported-command",
                work(
                    "imported-command",
                    &second(5),
                    None,
                    "Ran git",
                    WorkTone::Tool,
                )
                .command("git status")
                .request_kind("command")
                .status(ToolLifecycleStatus::Completed),
            ),
            message("imported-answer", Role::Assistant, 8, None),
        ];
        entries.extend(tail);
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            latest_run: latest("run-1", RunStatus::Running, Some(&second(20)), None),
            ..working(entries, &second(20))
        });
        shape(&rows)
            .into_iter()
            .map(|row| {
                if row.starts_with("fold:") {
                    "turn-fold".to_owned()
                } else {
                    row
                }
            })
            .collect::<Vec<_>>()
    };

    // Work starts from a sent prompt, or with no new prompt (a wake or a resume).
    assert_eq!(
        rows(vec![message("new-prompt", Role::User, 20, Some("run-1"))])[..4],
        [
            "user:imported-prompt",
            "turn-fold",
            "assistant:imported-answer",
            "user:new-prompt"
        ]
    );
    let without_prompt = rows(vec![]);
    assert!(without_prompt.contains(&"turn-fold".to_owned()));
    assert!(!without_prompt.contains(&"assistant:imported-update".to_owned()));
}

#[test]
fn shows_a_provider_native_subagents_runless_tools_as_live_work_while_it_works() {
    let entries = |status| {
        vec![
            user("task-entry", "task", None, &second(0)),
            work_entry(
                "command-entry",
                work("command", &second(5), None, "Running git", WorkTone::Tool)
                    .command("git diff --stat")
                    .request_kind("command")
                    .status(status),
            ),
        ]
    };
    let rows = |status, working: bool| {
        derive_desktop_rows(&DesktopTimelineInput {
            entries: entries(status),
            is_working: working,
            runless_work_active: working,
            active_turn_started_at: working.then(|| ts(&second(0))),
            ..DesktopTimelineInput::default()
        })
    };
    let running = rows(ToolLifecycleStatus::InProgress, true);
    assert_eq!(kinds(&running), ["message", "working", "work-live"]);
    let live = first(&running, "work-live");
    assert_eq!(live_entry_id(live), "command");
    assert!(matches!(live, DesktopRow::WorkLive { active: true, .. }));

    // Once the subagent settles, the same entries fold as finished history.
    let settled = rows(ToolLifecycleStatus::Completed, false);
    assert_eq!(kinds(&settled), ["message", "turn-fold"]);
}

#[test]
fn does_not_treat_runless_entries_as_live_work_on_a_thread_with_runs() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("turn-1", RunStatus::Running, Some(&second(0)), None),
        ..working(
            vec![work_entry(
                "runless-command-entry",
                work(
                    "runless-command",
                    &second(5),
                    None,
                    "Running git",
                    WorkTone::Tool,
                )
                .command("git status")
                .request_kind("command")
                .status(ToolLifecycleStatus::InProgress),
            )],
            &second(0),
        )
    });
    assert!(!has_kind(&rows, "work-live"));
    assert_eq!(kind(rows.last().unwrap()), "thinking");
}

fn command_entry(
    entry_id: &str,
    id: &str,
    created: &str,
    label: &str,
    command: &str,
    status: ToolLifecycleStatus,
) -> TimelineEntry {
    work_entry(
        entry_id,
        work(id, created, Some("turn-1"), label, WorkTone::Tool)
            .command(command)
            .request_kind("command")
            .status(status),
    )
}

fn running_turn(entries: Vec<TimelineEntry>) -> DesktopTimelineInput {
    DesktopTimelineInput {
        latest_run: latest("turn-1", RunStatus::Running, Some(&second(0)), None),
        ..working(entries, &second(0))
    }
}

#[test]
fn renders_a_single_completed_tool_call_directly() {
    let rows = derive_desktop_rows(&running_turn(vec![
        command_entry(
            "completed-command-entry",
            "completed-command",
            &second(5),
            "Ran rg",
            "rg toolCall",
            ToolLifecycleStatus::Completed,
        ),
        assistant(
            "assistant-commentary-entry",
            "assistant-commentary",
            "turn-1",
            &second(6),
            &second(6),
        ),
        command_entry(
            "running-command-entry",
            "running-command",
            &second(7),
            "Running tests",
            "vp test run",
            ToolLifecycleStatus::InProgress,
        ),
    ]));
    assert_eq!(kinds(&rows), ["working", "work", "message", "work-live"]);
    let direct = first(&rows, "work");
    let DesktopRow::Work {
        grouped_entries, ..
    } = direct
    else {
        unreachable!()
    };
    assert_eq!(grouped_ids(direct), ["completed-command"]);
    assert_eq!(grouped_entries[0].command.as_deref(), Some("rg toolCall"));
    assert!(!is_expanded_group(direct));
    assert_eq!(display_label(direct), Some("rg toolCall"));
}

#[test]
fn keeps_separated_in_progress_tool_runs_visible() {
    let rows = derive_desktop_rows(&running_turn(vec![
        command_entry(
            "first-running-entry",
            "first-running",
            &second(5),
            "Running first command",
            "rg first",
            ToolLifecycleStatus::InProgress,
        ),
        assistant(
            "assistant-commentary-entry",
            "assistant-commentary",
            "turn-1",
            &second(6),
            &second(6),
        ),
        command_entry(
            "second-running-entry",
            "second-running",
            &second(7),
            "Running second command",
            "rg second",
            ToolLifecycleStatus::InProgress,
        ),
    ]));
    assert_eq!(
        kinds(&rows),
        ["working", "work-live", "message", "work-live"]
    );
    assert_eq!(
        live_rows(&rows)
            .into_iter()
            .map(live_entry_id)
            .collect::<Vec<_>>(),
        ["first-running", "second-running"]
    );
}

#[test]
fn does_not_revive_stale_in_progress_tools_before_a_fresh_send_has_a_turn_id() {
    let rows = derive_desktop_rows(&working(
        vec![
            command_entry(
                "stale-running-entry",
                "stale-running",
                &second(5),
                "Running stale command",
                "rg stale",
                ToolLifecycleStatus::InProgress,
            ),
            user("user-followup-entry", "user-followup", None, &second(60)),
        ],
        &second(60),
    ));
    assert!(!has_kind(&rows, "work-live"));
}

fn task_progress(id: &str, created: &str, run: &str, label: &str) -> WorkLogEntry {
    let mut entry = work(id, created, Some(run), label, WorkTone::Thinking);
    entry.source_activity = Some(SourceActivity::TaskProgress);
    entry
}

#[test]
fn does_not_revive_separated_historical_task_progress() {
    let rows = derive_desktop_rows(&running_turn(vec![
        work_entry(
            "stale-progress-entry",
            task_progress("stale-progress", &second(5), "turn-1", "Old progress"),
        ),
        assistant(
            "assistant-commentary-entry",
            "assistant-commentary",
            "turn-1",
            &second(6),
            &second(6),
        ),
        command_entry(
            "running-command-entry",
            "running-command",
            &second(7),
            "Running command",
            "rg current",
            ToolLifecycleStatus::InProgress,
        ),
    ]));
    assert_eq!(
        live_rows(&rows)
            .into_iter()
            .map(live_entry_id)
            .collect::<Vec<_>>(),
        ["running-command"]
    );
}

#[test]
fn respects_the_lifecycle_of_trailing_task_progress() {
    for (status, active) in [
        (None, Some(true)),
        (Some(ToolLifecycleStatus::InProgress), Some(true)),
        (Some(ToolLifecycleStatus::Completed), Some(false)),
        (Some(ToolLifecycleStatus::Failed), None),
        (Some(ToolLifecycleStatus::Declined), Some(false)),
        (Some(ToolLifecycleStatus::Stopped), Some(false)),
    ] {
        let run = "turn-task-progress";
        let mut progress = task_progress("task-progress", &second(5), run, "Task progress");
        progress.tool_lifecycle_status = status;
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            latest_run: latest(run, RunStatus::Running, Some(&second(0)), None),
            ..working(
                vec![work_entry("task-progress-entry", progress)],
                &second(0),
            )
        });
        match active {
            None => {
                assert!(!has_kind(&rows, "work-live"), "{status:?}");
                let last = rows.last().unwrap();
                assert_eq!((kind(last), last.id()), ("thinking", "live-activity-row"));
            }
            Some(active) => assert!(
                matches!(first(&rows, "work-live"), DesktopRow::WorkLive { active: row_active, .. } if *row_active == active),
                "{status:?}"
            ),
        }
    }
}

#[test]
fn reuses_one_activity_row_for_initial_thinking_and_the_latest_tool() {
    let derive = |status: Option<ToolLifecycleStatus>, expanded: BTreeSet<String>| {
        let entries = match status {
            None => vec![],
            Some(status) => {
                let running = status == ToolLifecycleStatus::InProgress;
                let mut command = work(
                    "latest-command",
                    &second(5),
                    Some("turn-1"),
                    if running { "Running rg" } else { "Ran rg" },
                    WorkTone::Tool,
                )
                .command("rg toolCall")
                .request_kind("command")
                .status(status);
                if running {
                    command = command.detail("exit code 1");
                }
                vec![work_entry("latest-command-entry", command)]
            }
        };
        derive_desktop_rows(&DesktopTimelineInput {
            expanded_work_groups: expanded,
            ..running_turn(entries)
        })
    };
    let initial = derive(None, BTreeSet::new());
    let running = derive(Some(ToolLifecycleStatus::InProgress), BTreeSet::new());
    let completed = derive(Some(ToolLifecycleStatus::Completed), BTreeSet::new());
    let failed = derive(Some(ToolLifecycleStatus::Failed), BTreeSet::new());
    let declined = derive(Some(ToolLifecycleStatus::Declined), BTreeSet::new());

    assert_eq!(kind(row(&initial, "live-activity-row")), "thinking");
    assert!(matches!(
        row(&running, "live-activity-row"),
        DesktopRow::WorkLive { active: true, .. }
    ));
    assert!(matches!(
        row(&completed, "live-activity-row"),
        DesktopRow::WorkLive { active: true, .. }
    ));
    assert!(!has_kind(&failed, "work-live"));
    let failed_thinking = failed.last().unwrap();
    assert_eq!(
        (kind(failed_thinking), failed_thinking.id()),
        ("thinking", "live-activity-row")
    );
    let failed_group = group_id(failed_thinking).to_owned();
    let expanded_failed = derive(
        Some(ToolLifecycleStatus::Failed),
        [failed_group].into_iter().collect(),
    );
    let tail = &expanded_failed[expanded_failed.len() - 2..];
    assert!(matches!(
        &tail[0],
        DesktopRow::Thinking { id, expanded: true, .. } if id == "live-activity-row"
    ));
    assert!(is_expanded_group(&tail[1]));
    assert_eq!(grouped_ids(&tail[1]), ["latest-command"]);
    assert!(matches!(
        first(&declined, "work-live"),
        DesktopRow::WorkLive { active: false, .. }
    ));
    let declined_last = declined.last().unwrap();
    assert_eq!(
        (kind(declined_last), declined_last.id()),
        ("thinking", "live-activity-row")
    );
    for rows in [&initial, &running, &completed, &failed, &declined] {
        assert_eq!(
            rows.iter()
                .filter(|row| row.id() == "live-activity-row")
                .count(),
            1
        );
    }
}

#[test]
fn does_not_fold_the_sessions_running_turn_when_latest_run_regresses() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest(
            "turn-1",
            RunStatus::Completed,
            Some(&second(0)),
            Some(&second(25)),
        ),
        running_run: Some(run_id("turn-2")),
        ..working(
            vec![
                work_entry(
                    "previous-work-entry",
                    work(
                        "previous-work",
                        &second(5),
                        Some("turn-1"),
                        "Read files",
                        WorkTone::Tool,
                    ),
                ),
                user("user-followup-entry", "user-followup", None, &second(60)),
                work_entry(
                    "running-work-entry",
                    work(
                        "running-work",
                        &second(65),
                        Some("turn-2"),
                        "Searched files",
                        WorkTone::Tool,
                    ),
                ),
            ],
            &second(60),
        )
    });
    assert_eq!(
        rows.iter()
            .filter_map(|row| match row {
                DesktopRow::TurnFold { run, .. } => Some(run.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["turn-1"]
    );
    assert!(ids(&rows).contains(&"live-activity-row"));
}

#[test]
fn only_shows_assistant_metadata_on_the_terminal_assistant_message() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&["turn-1"]),
        ..input(vec![
            assistant(
                "assistant-thought-entry",
                "assistant-thought",
                "turn-1",
                &second(10),
                &second(11),
            ),
            assistant(
                "assistant-final-entry",
                "assistant-final",
                "turn-1",
                &second(20),
                &second(30),
            ),
        ])
    });
    assert_eq!(
        assistant_rows(&rows)
            .into_iter()
            .map(|row| message_flags(row).0)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

#[test]
fn withholds_assistant_metadata_while_the_active_turn_is_still_in_progress() {
    let rows = derive_desktop_rows(&running_turn(vec![assistant(
        "assistant-thought-entry",
        "assistant-thought",
        "turn-1",
        &second(10),
        &second(11),
    )]));
    let (meta, copy, _) = message_flags(assistant_rows(&rows)[0]);
    assert!(!meta);
    assert!(!copy);
    assert_eq!(kind(rows.last().unwrap()), "thinking");
}

fn website(page_url: &str) -> ToolIcon {
    ToolIcon::Website {
        page_url: page_url.into(),
        favicon_url: None,
        favicon_url_dark: None,
    }
}

fn toggle_presentation(row: &DesktopRow) -> (Option<ToolSurface>, Option<&ToolIcon>) {
    match row {
        DesktopRow::WorkToggle {
            tool_surface,
            tool_icon,
            ..
        } => (*tool_surface, tool_icon.as_ref()),
        _ => panic!("not a work toggle: {row:?}"),
    }
}

#[test]
fn expands_tools_through_the_same_activity_group() {
    for (middle_tone, summary) in [
        (WorkTone::Tool, "Used 3 tools"),
        (WorkTone::Info, "Used 2 tools and received 1 update"),
    ] {
        let mut second_entry = work("work-2", &second(2), None, "Status updated", middle_tone)
            .detail("Editing MessagesTimeline.tsx");
        second_entry.tool_surface = Some(ToolSurface::Computer);
        let mut third_entry =
            work("work-3", &second(3), None, "test", WorkTone::Tool).detail("Running tests");
        third_entry.tool_surface = Some(ToolSurface::Browser);
        third_entry.tool_icon = Some(website("https://example.com/checkout"));
        let works = vec![
            work("work-1", &second(1), None, "read", WorkTone::Tool).detail("Reading package.json"),
            second_entry,
            third_entry,
        ];
        let entries: Vec<TimelineEntry> = works
            .iter()
            .enumerate()
            .map(|(index, entry)| work_entry(&format!("work-entry-{}", index + 1), entry.clone()))
            .collect();
        let collapsed = derive_desktop_rows(&input(entries.clone()));
        let expanded = derive_desktop_rows(&DesktopTimelineInput {
            expanded_work_groups: ["work-group:work-entry-1".to_owned()].into(),
            ..input(entries)
        });
        assert_eq!(ids(&collapsed), ["work-toggle:work-entry-1"]);
        let toggle_row = first(&collapsed, "work-toggle");
        assert_eq!(
            toggle(toggle_row),
            ("work-group:work-entry-1", 3, false, summary, false)
        );
        assert_eq!(
            toggle_presentation(toggle_row),
            (
                Some(ToolSurface::Browser),
                Some(&website("https://example.com/checkout"))
            )
        );
        assert_eq!(
            ids(&expanded),
            [
                "work-toggle:work-entry-1",
                "work-group:work-entry-1:details"
            ]
        );
        let DesktopRow::Work {
            grouped_entries,
            is_expanded_tool_group: true,
            ..
        } = first(&expanded, "work")
        else {
            panic!("expected the expanded group");
        };
        assert_eq!(grouped_entries, &works);
        assert!(toggle(first(&expanded, "work-toggle")).2);
    }
}

#[test]
fn deduplicates_integration_sources_and_uses_the_first_source_icon_for_the_group() {
    let chrome = ToolSource {
        key: "browser-use:chrome".into(),
        name: "Chrome".into(),
        kind: ToolSourceKind::Integration,
        icon: Some(ToolIcon::NativeApp(NativeApp::DisplayName(
            "Google Chrome".into(),
        ))),
    };
    let browser = |id: &str, at: i64, label: &str, page_url: &str| {
        let mut entry = work(id, &second(at), None, label, WorkTone::Tool);
        entry.tool_surface = Some(ToolSurface::Browser);
        entry.tool_source = Some(chrome.clone());
        entry.tool_icon = Some(website(page_url));
        work_entry(id, entry)
    };
    let rows = derive_desktop_rows(&input(vec![
        browser(
            "browser-1",
            1,
            "Open MATLAB",
            "https://www.mathworks.com/help/matlab/",
        ),
        browser(
            "browser-2",
            2,
            "Show summary",
            "https://www.mathworks.com/help/matlab/summary.html",
        ),
        work_entry(
            "command-1",
            work("command-1", &second(3), None, "Ran command", WorkTone::Tool)
                .command("git status")
                .item_type(ItemType::CommandExecution),
        ),
    ]));
    assert_eq!(kind(&rows[0]), "work-toggle");
    assert_eq!(
        toggle(&rows[0]).3,
        "Used Chrome integration and ran 1 command"
    );
    assert_eq!(
        toggle_presentation(&rows[0]),
        (
            Some(ToolSurface::Browser),
            Some(&website("https://www.mathworks.com/help/matlab/"))
        )
    );
}

#[test]
fn keeps_a_large_expanded_tool_run_inside_one_timeline_item() {
    for is_working in [true, false] {
        let run = "turn-many-tools";
        let created = "2026-09-01T12:00:00Z";
        let entries: Vec<TimelineEntry> = (0..1_000)
            .map(|index| {
                work_entry(
                    &format!("tool-entry-{index}"),
                    work(
                        &format!("tool-{index}"),
                        created,
                        Some(run),
                        "orchestration.thread_read",
                        WorkTone::Tool,
                    )
                    .status(if is_working && index == 999 {
                        ToolLifecycleStatus::InProgress
                    } else {
                        ToolLifecycleStatus::Completed
                    }),
                )
            })
            .collect();
        let base = DesktopTimelineInput {
            is_working,
            expanded_runs: runs(&[run]),
            running_run: is_working.then(|| run_id(run)),
            active_turn_started_at: is_working.then(|| ts(created)),
            ..input(entries.clone())
        };
        let collapsed = derive_desktop_rows(&base);
        let group = rows_group(&collapsed);
        let expanded = derive_desktop_rows(&DesktopTimelineInput {
            expanded_work_groups: [group.clone()].into(),
            ..base.clone()
        });
        let group_rows: Vec<&DesktopRow> =
            expanded.iter().filter(|row| kind(row) == "work").collect();
        assert_eq!(group_rows.len(), 1, "live={is_working}");
        assert_eq!(
            grouped_ids(group_rows[0]),
            entries
                .iter()
                .map(|entry| entry.work().unwrap().id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(group_rows[0].id(), format!("{group}:details"));
        assert!(!has_kind(&derive_desktop_rows(&base), "work"));
    }
}

fn rows_group(rows: &[DesktopRow]) -> String {
    group_id(
        rows.iter()
            .find(|row| matches!(kind(row), "work-toggle" | "work-live"))
            .expect("a collapsed group"),
    )
    .to_owned()
}

#[test]
fn uses_the_final_call_for_tool_groups() {
    use ToolLifecycleStatus::{Completed, Failed};
    for (statuses, has_failure) in [
        ([Failed, Completed], false),
        ([Completed, Failed], true),
        ([Failed, Failed], true),
    ] {
        let entries = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                work_entry(
                    &format!("work-entry-{index}"),
                    work(
                        &format!("work-{index}"),
                        &second(index as i64),
                        None,
                        "Ran command",
                        WorkTone::Tool,
                    )
                    .item_type(ItemType::CommandExecution)
                    .status(*status),
                )
            })
            .collect();
        let rows = derive_desktop_rows(&input(entries));
        let (_, hidden_count, _, _, failure) = toggle(first(&rows, "work-toggle"));
        assert_eq!((hidden_count, failure), (2, has_failure), "{statuses:?}");
    }
}

#[test]
fn uses_the_final_tool_call_for_mixed_work_groups() {
    for (statuses, has_failure) in [
        (["failed", "completed", "info"], false),
        (["failed", "info", "completed"], false),
        (["error", "info", "completed"], false),
        (["completed", "failed", "info"], true),
        (["failed", "info", "failed"], true),
        (["completed", "info", "failed"], true),
    ] {
        let entries = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let id = format!("work-{index}");
                let at = second(index as i64);
                let entry = match *status {
                    "info" => work(&id, &at, None, "Status updated", WorkTone::Info),
                    "error" => work(&id, &at, None, "Command failed", WorkTone::Error),
                    status => work(&id, &at, None, "Ran command", WorkTone::Tool).status(
                        if status == "failed" {
                            ToolLifecycleStatus::Failed
                        } else {
                            ToolLifecycleStatus::Completed
                        },
                    ),
                };
                work_entry(&format!("work-entry-{index}"), entry)
            })
            .collect();
        let rows = derive_desktop_rows(&input(entries));
        let has_error = statuses.contains(&"error");
        let (_, hidden_count, _, summary, failure) = toggle(first(&rows, "work-toggle"));
        assert_eq!(
            (hidden_count, summary, failure),
            (
                if has_error { 2 } else { 3 },
                if has_error {
                    "Received 1 update and used 1 tool"
                } else {
                    "Used 2 tools and received 1 update"
                },
                has_failure
            ),
            "{statuses:?}"
        );
        if has_error {
            let DesktopRow::Work {
                grouped_entries, ..
            } = &rows[0]
            else {
                panic!("expected the error row first");
            };
            assert_eq!(grouped_entries.len(), 1);
            assert_eq!(grouped_entries[0].tone, WorkTone::Error);
            assert_eq!(grouped_entries[0].label, "Command failed");
        }
    }
}

#[test]
fn keeps_thinking_after_assistant_content_grows() {
    for text in ["", " \n"] {
        let started = second(0);
        let derive = |text: &str| {
            derive_desktop_rows(&DesktopTimelineInput {
                running_run: Some(run_id("turn-1")),
                ..working(
                    vec![with_message(
                        streaming(assistant(
                            "assistant-entry",
                            "assistant-1",
                            "turn-1",
                            &started,
                            &started,
                        )),
                        |message| message.text = text.into(),
                    )],
                    &started,
                )
            })
        };
        let initial = derive(text);
        let updated = derive("I will inspect the repository.");
        let initial_thinking = row(&initial, "live-activity-row");
        assert_eq!(kind(initial_thinking), "thinking");
        assert_eq!(row(&updated, "live-activity-row"), initial_thinking);
        assert_eq!(updated.last(), Some(initial_thinking));
    }
}

// Run and attempt history.

#[test]
fn folds_settled_turn_commentary_and_work_behind_a_worked_for_row() {
    let entries = vec![
        user("user-entry", "user-1", None, &second(0)),
        assistant(
            "assistant-thought-entry",
            "assistant-thought",
            "turn-1",
            &second(5),
            &second(6),
        ),
        work_entry("work-entry-1", ran_command("work-1", &second(8), "turn-1")),
        work_entry(
            "provider-recovered-entry",
            work(
                "provider-recovered",
                &second(9),
                Some("turn-1"),
                "Provider recovered (2/5 retries)",
                WorkTone::Info,
            )
            .item_type(ItemType::Error)
            .status(ToolLifecycleStatus::Completed),
        ),
        event_entry(
            "thread-created-entry",
            &second(10),
            fx::thread_created("thread-created", "child", "Child"),
        ),
        assistant(
            "assistant-final-entry",
            "assistant-final",
            "turn-1",
            &second(20),
            &second(22),
        ),
    ];
    let collapsed = derive_desktop_rows(&input(entries.clone()));
    let (run, label, expanded) = fold(first(&collapsed, "turn-fold"));
    assert_eq!(run.as_str(), "turn-1");
    assert!(!expanded);
    // User message boundary (00:00:00) → terminal message updatedAt (00:00:22).
    assert_eq!(label, "Worked for 22s");
    assert_eq!(
        ids(&collapsed),
        [
            "user-entry",
            "turn-fold:turn-1",
            "thread-created-entry",
            "assistant-final-entry"
        ]
    );

    let expanded_rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&["turn-1"]),
        ..input(entries.clone())
    });
    assert_eq!(
        ids(&expanded_rows),
        [
            "user-entry",
            "turn-fold:turn-1",
            "assistant-thought-entry",
            "work-toggle:work-entry-1",
            "thread-created-entry",
            "assistant-final-entry"
        ]
    );
    assert!(
        expanded_rows
            .iter()
            .any(|row| matches!(row, DesktopRow::TurnFold { expanded: true, .. }))
    );
    let opened = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&["turn-1"]),
        expanded_work_groups: ["work-group:work-entry-1".to_owned()].into(),
        ..input(entries)
    });
    // Expanding the activity preserves tool output and provider recovery details.
    assert_eq!(
        grouped_ids(first(&opened, "work")),
        ["work-1", "provider-recovered"]
    );
}

#[test]
fn hides_subagents_in_folded_turns_when_they_arrive_before_commentary() {
    let rows = derive_desktop_rows(&input(vec![
        user("user-entry", "user-1", None, &second(0)),
        event_entry(
            "subagent-card-entry",
            &second(3),
            fx::subagent("subagent-card", "task").run("turn-1"),
        ),
        assistant(
            "assistant-commentary-entry",
            "assistant-commentary",
            "turn-1",
            &second(5),
            &second(6),
        ),
        assistant(
            "assistant-final-entry",
            "assistant-final",
            "turn-1",
            &second(20),
            &second(22),
        ),
    ]));
    assert_eq!(
        ids(&rows),
        ["user-entry", "turn-fold:turn-1", "assistant-final-entry"]
    );
}

fn attempt(id: &str, run: &str, ordinal: u64, status: AttemptStatus) -> TimelineAttempt {
    TimelineAttempt {
        id: RunAttemptId::new(id).unwrap(),
        run: run_id(run),
        ordinal,
        status,
    }
}

fn in_attempt(mut entry: TimelineEntry, attempt: &TimelineAttempt) -> TimelineEntry {
    entry.attempt = Some(attempt.clone());
    entry
}

#[test]
fn collapses_only_output_from_a_superseded_v2_attempt_within_the_active_logical_run() {
    let run = "run-steered";
    let superseded = attempt("attempt-1", run, 1, AttemptStatus::Superseded);
    let active = attempt("attempt-2", run, 2, AttemptStatus::Running);
    let entries = vec![
        in_attempt(
            intent(
                user("initial-user-entry", "initial-user", Some(run), &second(0)),
                InputIntent::TurnStart,
            ),
            &superseded,
        ),
        in_attempt(
            assistant(
                "superseded-assistant-entry",
                "superseded-assistant",
                run,
                &second(2),
                &second(3),
            ),
            &superseded,
        ),
        in_attempt(
            work_entry(
                "superseded-work-entry",
                work(
                    "superseded-work",
                    &second(4),
                    Some(run),
                    "Old command",
                    WorkTone::Tool,
                ),
            ),
            &superseded,
        ),
        in_attempt(
            event_entry(
                "superseded-thread-created-entry",
                "2026-01-01T00:00:04.500Z",
                fx::thread_created("superseded-thread-created", "child", "Child"),
            ),
            &superseded,
        ),
        in_attempt(
            intent(
                user("steer-user-entry", "steer-user", Some(run), &second(5)),
                InputIntent::Steer,
            ),
            &active,
        ),
        in_attempt(
            streaming(assistant(
                "active-assistant-entry",
                "active-assistant",
                run,
                &second(6),
                &second(7),
            )),
            &active,
        ),
    ];
    let base = DesktopTimelineInput {
        latest_run: latest(run, RunStatus::Running, Some(&second(0)), None),
        ..input(entries)
    };
    let collapsed = derive_desktop_rows(&base);
    assert_eq!(
        ids(&collapsed),
        [
            "initial-user-entry",
            "attempt-fold:attempt-1",
            "superseded-thread-created-entry",
            "steer-user-entry",
            "active-assistant-entry"
        ]
    );
    let DesktopRow::AttemptFold {
        attempt,
        run: fold_run,
        label,
        expanded,
        ..
    } = first(&collapsed, "attempt-fold")
    else {
        unreachable!()
    };
    assert_eq!(
        (
            attempt.as_str(),
            fold_run.as_str(),
            label.as_str(),
            *expanded
        ),
        ("attempt-1", run, "Superseded attempt", false)
    );

    let expanded_rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_attempts: [RunAttemptId::new("attempt-1").unwrap()].into(),
        ..base
    });
    assert_eq!(
        ids(&expanded_rows),
        [
            "initial-user-entry",
            "attempt-fold:attempt-1",
            "superseded-assistant-entry",
            "superseded-work-entry",
            "superseded-thread-created-entry",
            "steer-user-entry",
            "active-assistant-entry"
        ]
    );
}

#[test]
fn hides_the_interruption_request_while_keeping_intervening_work_and_the_result() {
    let run = "turn-1";
    let interrupt_request = fx::run_interrupt_request("item-run_interrupt_request")
        .run(run)
        .text("Stopping");
    let interrupt_result =
        fx::run_interrupt_result("item-run_interrupt_result", "item-run_interrupt_request")
            .run(run)
            .text("Stopped");
    let interrupted = latest(
        run,
        RunStatus::Interrupted,
        Some(&second(0)),
        Some(&second(3)),
    );
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: interrupted.clone(),
        ..input(vec![
            event_entry("interrupt-request", &second(1), interrupt_request),
            work_entry(
                "work-entry",
                work(
                    "work-1",
                    &second(2),
                    Some(run),
                    "Finishing tool output",
                    WorkTone::Tool,
                ),
            ),
            event_entry("interrupt-result", &second(3), interrupt_result.clone()),
        ])
    });
    assert_eq!(ids(&rows), ["work-entry", "interrupt-result"]);
    assert!(!has_kind(&rows, "turn-fold"));

    // The command the interrupt cut short stays visible with its outcome.
    let stopped_rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: interrupted,
        ..input(vec![
            work_entry(
                "stopped-command-entry",
                ran_command("stopped-command", &second(1), run)
                    .item_type(ItemType::CommandExecution)
                    .command("/bin/bash -lc 'sleep 90 && echo slept'")
                    .status(ToolLifecycleStatus::Stopped),
            ),
            event_entry("interrupt-result", &second(3), interrupt_result),
        ])
    });
    assert_eq!(
        ids(&stopped_rows),
        ["stopped-command-entry", "interrupt-result"]
    );
    assert_eq!(kind(&stopped_rows[0]), "work");
    assert_eq!(
        display_label(&stopped_rows[0]),
        Some("sleep 90 && echo slept")
    );
}

// Streaming rows.

#[test]
fn keeps_row_parity_and_history_identity_while_assistant_text_streams() {
    for (before, after) in [
        ("", "Now visible"),
        (" \n", "Now visible"),
        ("Visible", ""),
        ("", " \t\n"),
        ("Visible", "★ Insight\nKeep these line breaks"),
    ] {
        let derive = |text: &str| {
            let fixture = streaming_fixture(text);
            derive_desktop_rows(&DesktopTimelineInput {
                latest_run: latest(
                    STREAMING_RUN,
                    RunStatus::Running,
                    Some(&streaming_time(5)),
                    None,
                ),
                running_run: Some(fixture.run.clone()),
                supports_conversation_rollback: true,
                ..working(timeline(&fixture.state), &streaming_time(5))
            })
        };
        let previous = derive(before);
        let next = derive(after);
        assert_eq!(ids(&previous), ids(&next));
        for (previous, next) in previous.iter().zip(&next) {
            let live_message = matches!(
                previous,
                DesktopRow::Message { .. } | DesktopRow::AssistantMeta { .. }
            ) && message_of(previous).id.as_str() == "live-assistant";
            if live_message {
                assert_eq!(message_of(previous).text, before);
                assert_eq!(message_of(next).text, after);
            } else {
                assert_eq!(previous, next);
            }
        }
    }
}

// Linked timeline resources.

const RESOURCE_RUN: &str = "resource-run";

fn resource_event(id: &str, item: Item, run: &str) -> TimelineEntry {
    event_entry(id, "2026-09-08T10:00:02Z", item.run(run))
}

fn subagent_event(id: &str) -> TimelineEntry {
    resource_event(id, fx::subagent(id, id), RESOURCE_RUN)
}

fn subagent_ids(row: &DesktopRow) -> Vec<&str> {
    match row {
        DesktopRow::Event {
            subagents: Some(subagents),
            ..
        } => subagents.iter().map(|item| item.id.as_str()).collect(),
        _ => panic!("not a grouped subagent card: {row:?}"),
    }
}

fn continues_work_log(row: &DesktopRow) -> bool {
    matches!(
        row,
        DesktopRow::Work {
            continues_work_log: true,
            ..
        } | DesktopRow::WorkLive {
            continues_work_log: true,
            ..
        } | DesktopRow::WorkToggle {
            continues_work_log: true,
            ..
        } | DesktopRow::Thinking {
            continues_work_log: true,
            ..
        }
    )
}

#[test]
fn previews_a_thought_and_separates_subagent_cards_from_worklogs() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&[RESOURCE_RUN]),
        ..input(vec![
            work_entry(
                "thought",
                work(
                    "thought",
                    "2026-09-08T10:00:01Z",
                    Some(RESOURCE_RUN),
                    "Thought",
                    WorkTone::Thinking,
                )
                .item_type(ItemType::Reasoning)
                .detail("First paragraph.\n\nSecond paragraph."),
            ),
            subagent_event("child"),
            assistant(
                "answer",
                "answer",
                RESOURCE_RUN,
                "2026-09-08T10:00:03Z",
                "2026-09-08T10:00:03Z",
            ),
        ])
    });
    let thought = first(&rows, "work");
    assert_eq!(
        display_label(thought),
        Some("First paragraph. Second paragraph.")
    );
    assert!(!continues_work_log(thought));
    assert!(!continues_work_log(row(&rows, "child")));
}

#[test]
fn groups_adjacent_subagents_without_merging_across_a_resource_or_run_boundary() {
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&[RESOURCE_RUN, "other-run"]),
        ..input(vec![
            subagent_event("a"),
            subagent_event("b"),
            resource_event("c", fx::thread_created("c", "child", "Child"), RESOURCE_RUN),
            subagent_event("d"),
            resource_event("e", fx::subagent("e", "e"), "other-run"),
        ])
    });
    assert_eq!(
        ids(&rows),
        [
            "turn-fold:resource-run",
            "a",
            "c",
            "d",
            "turn-fold:other-run",
            "e"
        ]
    );
    assert_eq!(subagent_ids(&rows[1]), ["a", "b"]);
}

#[test]
fn matches_delegation_calls_by_child_identity() {
    let cases = [
        ("completed", "direct", "general"),
        ("completed", "structured", "general"),
        ("completed", "text", "general"),
        ("completed", "structured", "research"),
        ("running", "direct", "general"),
        ("running", "direct", "research"),
    ];
    for (status, envelope, role) in cases {
        let completed = status == "completed";
        let delegation = |id: &str, task_id: &str, failed: bool| {
            let output = completed.then(|| match envelope {
                "structured" => json!({
                    "content": json!({ "taskId": task_id }).to_string(),
                    "structuredContent": { "taskId": task_id },
                }),
                "text" => json!({
                    "content": [{ "type": "text", "text": json!({ "taskId": task_id }).to_string() }],
                }),
                _ => json!({ "taskId": task_id }),
            });
            let item = fx::dynamic_tool(
                id,
                "orchestration.delegate_task",
                json!({ "task": if task_id == "b" { "a" } else { task_id }, "role": role }),
                output,
            )
            .run(RESOURCE_RUN)
            .status(if failed {
                ItemStatus::Failed
            } else if completed {
                ItemStatus::Completed
            } else {
                ItemStatus::Running
            });
            let entry = work(
                id,
                "2026-09-08T10:00:02Z",
                Some(RESOURCE_RUN),
                "Delegated a child task",
                if failed {
                    WorkTone::Error
                } else {
                    WorkTone::Tool
                },
            )
            .item_type(ItemType::DynamicTool)
            .status(if failed {
                ToolLifecycleStatus::Failed
            } else if completed {
                ToolLifecycleStatus::Completed
            } else {
                ToolLifecycleStatus::InProgress
            })
            .item(item);
            work_entry(id, entry)
        };
        let mut entries = vec![subagent_event("a"), delegation("delegate-a", "a", false)];
        if completed {
            entries.push(subagent_event("b"));
        }
        entries.extend([
            delegation("delegate-b", "b", false),
            delegation("unmatched", "other-child", false),
            subagent_event("c"),
            delegation("failed", "c", true),
            subagent_event("d"),
        ]);
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            is_working: !completed,
            running_run: (!completed).then(|| run_id(RESOURCE_RUN)),
            expanded_runs: runs(&[RESOURCE_RUN]),
            app_owned_tasks: ["a", "b", "c", "d"]
                .into_iter()
                .map(|task| NodeId::new(task).unwrap())
                .collect(),
            ..input(entries)
        });
        let case = format!("{status} {role} {envelope}");
        if completed {
            assert_eq!(subagent_ids(row(&rows, "a")), ["a", "b"], "{case}");
        } else {
            row(&rows, "a");
        }
        assert!(!ids(&rows).contains(&"b"), "{case}");
        row(&rows, "c");
        row(&rows, "d");
        let visible_tools: Vec<&str> = rows
            .iter()
            .filter(|row| matches!(kind(row), "work" | "work-live"))
            .flat_map(grouped_ids)
            .collect();
        assert!(visible_tools.contains(&"unmatched"), "{case}");
        assert!(visible_tools.contains(&"failed"), "{case}");
        assert_eq!(visible_tools.contains(&"delegate-a"), !completed, "{case}");
        assert_eq!(visible_tools.contains(&"delegate-b"), !completed, "{case}");
    }
}

#[test]
fn keeps_created_chat_summaries_after_the_final_answer_and_folds_only_their_timeline_rows() {
    let entries = vec![
        assistant(
            "intro",
            "intro",
            RESOURCE_RUN,
            "2026-09-08T10:00:00Z",
            "2026-09-08T10:00:00Z",
        ),
        resource_event(
            "created",
            fx::thread_created("created", "child", "Child"),
            RESOURCE_RUN,
        ),
        assistant(
            "final",
            "final",
            RESOURCE_RUN,
            "2026-09-08T10:00:04Z",
            "2026-09-08T10:00:04Z",
        ),
    ];
    let collapsed = derive_desktop_rows(&input(entries.clone()));
    assert_eq!(
        ids(&collapsed),
        [
            "turn-fold:resource-run",
            "final",
            "summary:created",
            "assistant-meta:final"
        ]
    );
    assert!(matches!(
        row(&collapsed, "summary:created"),
        DesktopRow::Event {
            resource_summary: true,
            ..
        }
    ));
    let expanded = derive_desktop_rows(&DesktopTimelineInput {
        expanded_runs: runs(&[RESOURCE_RUN]),
        ..input(entries)
    });
    assert_eq!(
        ids(&expanded),
        [
            "turn-fold:resource-run",
            "intro",
            "created",
            "final",
            "summary:created",
            "assistant-meta:final"
        ]
    );
}

#[test]
fn keeps_a_wake_notification_visible_outside_folded_work() {
    for is_working in [true, false] {
        let mut fixture = streaming_fixture("");
        fixture.replace_item("history-user-item", |item| {
            let notification = fx::notification("wake", "Delegated task finished");
            item.kind = notification.kind;
        });
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            is_working,
            running_run: Some(fixture.run.clone()),
            active_turn_started_at: Some(ts(&streaming_time(0))),
            ..input(timeline(&fixture.state))
        });
        assert!(
            rows.iter()
                .any(|row| matches!(row, DesktopRow::Work { grouped_entries, .. }
                if grouped_entries.iter().any(|entry| entry.label == "Delegated task finished"))),
            "working={is_working}"
        );
    }
}

#[test]
fn keeps_completed_responses_folded_across_notification_wakes() {
    for is_working in [true, false] {
        let event = |id: &str, run: &str, at: i64, notification: bool| {
            let entry = work(id, &second(at), Some(run), id, WorkTone::Info);
            work_entry(
                id,
                if notification {
                    entry.item_type(ItemType::Notification)
                } else {
                    entry
                },
            )
        };
        let answer =
            |id: &str, run: &str, at: i64| assistant(id, id, run, &second(at), &second(at));
        let mut entries = vec![
            event("launch-tools", "initial", 1, false),
            answer("launched", "initial", 5),
            event("notification-a", "wake-a", 20, true),
            event("read-a", "wake-a", 21, false),
            answer("ack-a", "wake-a", 25),
            event("notification-b", "wake-b", 40, true),
            event("read-b", "wake-b", 41, false),
        ];
        if !is_working {
            entries.push(answer("ack-b", "wake-b", 45));
        }
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            latest_run: latest(
                "wake-b",
                if is_working {
                    RunStatus::Running
                } else {
                    RunStatus::Completed
                },
                Some(&second(40)),
                (!is_working).then(|| second(45)).as_deref(),
            ),
            is_working,
            active_turn_started_at: Some(ts(&second(40))),
            ..input(entries)
        });
        let row_ids = ids(&rows);
        let position = |id: &str| {
            row_ids
                .iter()
                .position(|row| *row == id)
                .unwrap_or_else(|| panic!("no {id} in {row_ids:?}"))
        };
        assert!(row_ids.contains(&"turn-fold:initial"));
        assert!(!row_ids.contains(&"launch-tools"));
        assert!(position("notification-a") < position("turn-fold:wake-a"));
        assert!(position("turn-fold:wake-a") < position("ack-a"));
        let boundary = if is_working {
            "working-indicator-row"
        } else {
            "turn-fold:wake-b"
        };
        assert!(position("notification-b") < position(boundary));
        if is_working {
            assert_eq!(row(&rows, boundary).created_at(), Some(&ts(&second(40))));
        }
    }
}

fn setup_stage(id: WorktreeSetupStageId, status: WorktreeSetupStageStatus) -> WorktreeSetupStage {
    let done = status == WorktreeSetupStageStatus::Done;
    WorktreeSetupStage {
        id,
        status,
        started_at: Some(ts(&second(10))),
        ended_at: done.then(|| ts(&second(11))),
        percent: None,
        detail: None,
        tail: vec![],
    }
}

#[test]
fn keeps_the_working_header_in_place_across_worktree_setup_handoff() {
    let snapshot = WorktreeSetupSnapshot {
        thread: ThreadId::new("thread-setup").unwrap(),
        phase: WorktreeSetupPhase::Running,
        started_at: ts(&second(0)),
        ended_at: None,
        branch: Some("feature".into()),
        base_ref: Some("main".into()),
        worktree_path: None,
        setup_script: None,
        stages: vec![],
        error: None,
        sequence: 3,
    };
    let user_entry = user("user-entry", "user-1", None, &second(0));
    let assistant_entry = streaming(assistant(
        "assistant-entry",
        "assistant-1",
        "turn-1",
        &second(30),
        &second(30),
    ));
    let setup = |entries: Vec<TimelineEntry>,
                 snapshot: WorktreeSetupSnapshot,
                 latest_run: Option<TimelineLatestRun>| {
        derive_desktop_rows(&DesktopTimelineInput {
            worktree_setup: Some(snapshot),
            latest_run,
            ..working(entries, &second(0))
        })
    };
    let without_messages = setup(vec![], snapshot.clone(), None);
    assert_eq!(
        without_messages,
        [
            DesktopRow::Working {
                id: "working-indicator-row".into(),
                created_at: Some(snapshot.started_at.clone()),
            },
            DesktopRow::WorktreeSetup {
                id: "worktree-setup-row".into(),
                created_at: ts(&second(0)),
                snapshot: snapshot.clone(),
                embedded: false,
            }
        ]
    );

    // A failed setup never handed off, so the card stays under the send.
    let with_messages = setup(
        vec![user_entry.clone(), assistant_entry],
        WorktreeSetupSnapshot {
            phase: WorktreeSetupPhase::Failed,
            ..snapshot.clone()
        },
        None,
    );
    assert_eq!(
        kinds(&with_messages),
        [
            "message",
            "worktree-setup",
            "working",
            "message",
            "thinking"
        ]
    );

    // Once the agent stage is done the setup script may still run in the
    // background: the header owns its progress, so no setup row remains.
    let async_snapshot = WorktreeSetupSnapshot {
        stages: vec![
            setup_stage(
                WorktreeSetupStageId::SetupScript,
                WorktreeSetupStageStatus::Running,
            ),
            setup_stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
        ],
        ..snapshot.clone()
    };
    let live_turn = latest("turn-1", RunStatus::Running, Some(&second(11)), None);
    assert_eq!(
        kinds(&setup(
            vec![user_entry.clone()],
            async_snapshot.clone(),
            live_turn.clone()
        )),
        ["message", "working", "thinking"]
    );

    // Dispatched but not yet visible as a run: the full card stays put so
    // nothing collapses during the handoff.
    let handoff = setup(vec![user_entry.clone()], async_snapshot.clone(), None);
    assert_eq!(kinds(&handoff), ["message", "working", "worktree-setup"]);
    assert!(matches!(
        handoff[2],
        DesktopRow::WorktreeSetup {
            embedded: false,
            ..
        }
    ));

    // A clean finish before the run is live keeps that same layout, so the card
    // does not jump above the header in the gap before the run starts.
    let finished_stages = vec![
        setup_stage(
            WorktreeSetupStageId::SetupScript,
            WorktreeSetupStageStatus::Done,
        ),
        setup_stage(WorktreeSetupStageId::Agent, WorktreeSetupStageStatus::Done),
    ];
    let settled = setup(
        vec![user_entry.clone()],
        WorktreeSetupSnapshot {
            phase: WorktreeSetupPhase::Done,
            stages: finished_stages.clone(),
            ..snapshot.clone()
        },
        None,
    );
    assert_eq!(kinds(&settled), ["message", "working", "worktree-setup"]);

    // A script that already finished has nothing left to show once the run is live.
    let finished = setup(
        vec![user_entry],
        WorktreeSetupSnapshot {
            stages: finished_stages,
            ..async_snapshot
        },
        live_turn,
    );
    assert_eq!(kinds(&finished), ["message", "working", "thinking"]);
}

#[test]
fn keeps_historical_failures_and_preceding_work_visible_without_disclosures() {
    for failure_class in ["provider_error", "usage_limit"] {
        let run = "failed-run";
        let at = "2026-09-20T12:00:00Z";
        let base = |item: Item, ordinal: u64| {
            item.run(run)
                .ordinal(ordinal)
                .started(at)
                .completed(Some(at))
        };
        let mut failure = base(
            fx::error("failure", "The provider stopped this turn.\nRetry later."),
            2,
        )
        .status(ItemStatus::Failed);
        if let ItemKind::Error {
            class, retryable, ..
        } = &mut failure.kind
        {
            *class = Some(failure_class.into());
            *retryable = Some(true);
        }
        let user_item = base(fx::user_message("user", "user"), 0);
        let user_message = Message {
            created_at: ts(at),
            updated_at: ts(at),
            ..domain_message("user", Role::User, run, "Build it")
        };
        let state = |items: Vec<Item>| {
            thread(
                items,
                vec![fx::run(run, 1, RunStatus::Failed)],
                vec![user_message.clone()],
            )
        };
        let superseded = attempt("superseded-attempt", run, 1, AttemptStatus::Superseded);
        let entries = timeline(&state(vec![
            user_item.clone(),
            base(fx::command("command", "pwd"), 1),
            failure.clone(),
        ]))
        .into_iter()
        .map(|entry| in_attempt(entry, &superseded))
        .collect();
        let rows = derive_desktop_rows(&DesktopTimelineInput {
            latest_run: latest("newer-run", RunStatus::Completed, Some(at), Some(at)),
            ..input(entries)
        });
        assert!(
            !rows
                .iter()
                .any(|row| matches!(kind(row), "turn-fold" | "attempt-fold" | "work-toggle")),
            "{failure_class}"
        );
        let work: Vec<&WorkLogEntry> = rows
            .iter()
            .filter_map(|row| match row {
                DesktopRow::Work {
                    grouped_entries, ..
                } => Some(grouped_entries),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            work.iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["command", "failure"]
        );
        let last = work.last().unwrap();
        assert_eq!(
            last.detail.as_deref(),
            Some("The provider stopped this turn.\nRetry later.")
        );
        assert_eq!(last.created_at.as_str(), "2026-09-20T12:00:00.000Z");

        let mut without_tools = timeline(&state(vec![user_item, failure]));
        without_tools.insert(
            1,
            with_message(
                assistant(
                    "assistant-before-failure",
                    "assistant-before-failure",
                    run,
                    at,
                    at,
                ),
                |message| message.text = "Checking the workspace.".into(),
            ),
        );
        let compact = derive_desktop_rows(&input(without_tools));
        assert_eq!(
            kinds(&compact),
            ["message", "message", "work", "assistant-meta"]
        );
        assert!(!message_flags(&compact[1]).0);
        let DesktopRow::AssistantMeta {
            show_assistant_copy_button,
            message,
            ..
        } = compact.last().unwrap()
        else {
            unreachable!()
        };
        assert!(*show_assistant_copy_button);
        assert_eq!(message.id.as_str(), "assistant-before-failure");
    }
}

#[test]
fn excludes_checkpoint_only_work_from_the_timeline() {
    let at = "2026-09-04T12:00:00Z";
    let run = "native-run";
    let state = State {
        checkpoints: vec![Checkpoint {
            status: CheckpointStatus::Ready,
            scope: None,
            id: agent_domain::CheckpointId::new("checkpoint").unwrap(),
            run: Some(run_id(run)),
            run_ordinal: 1,
            native_heads: BTreeMap::new(),
            file_ref: "checkpoint".into(),
            files: vec![],
        }],
        ..thread(
            vec![
                fx::assistant_message("native-item", "done")
                    .run(run)
                    .started(at)
                    .completed(Some(at))
                    .text("Done"),
            ],
            vec![fx::run(run, 1, RunStatus::Completed)],
            vec![],
        )
    };
    let entries = timeline(&state);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].message().is_some());
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest(run, RunStatus::Completed, Some(at), Some(at)),
        ..input(entries)
    });
    assert_eq!(kinds(&rows), ["message"]);
}

#[test]
fn keeps_a_diagnostic_separate_from_adjacent_tool_summaries() {
    let message = "2026-03-14T16:11:12.550224Z ERROR codex_core::codex: failed to load skill /home/sebherrerabe/repos/devsuite/.agent/skills/monorepo-scaffolding/SKILL.md: invalid YAML: mapping va...";
    let at = "2026-09-05T00:00:00Z";
    let base = |item: Item, ordinal: u64| {
        item.run("run-1")
            .ordinal(ordinal)
            .started(at)
            .completed(Some(at))
    };
    let state = thread(
        vec![
            base(fx::command("command-before", "git status"), 0).text("clean"),
            base(fx::error("diagnostic", message), 1),
            base(fx::command("command-after", "git diff"), 2),
        ],
        vec![fx::run("run-1", 1, RunStatus::Running)],
        vec![],
    );
    let rows = derive_desktop_rows(&DesktopTimelineInput {
        latest_run: latest("run-1", RunStatus::Running, Some(at), None),
        ..working(timeline(&state), at)
    });
    let diagnostic = row(&rows, "diagnostic");
    let DesktopRow::Work {
        grouped_entries,
        is_expanded_tool_group: false,
        ..
    } = diagnostic
    else {
        panic!("expected a diagnostic row: {diagnostic:?}");
    };
    assert_eq!(grouped_entries.len(), 1);
    assert_eq!(work_entry_display_label(&grouped_entries[0], None), message);
}
