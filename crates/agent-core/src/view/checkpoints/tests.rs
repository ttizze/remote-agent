use super::*;
use crate::sync::fixtures::{at, command_item, run, thread_id, thread_state};
use agent_domain::{
    Checkpoint, CheckpointFile, CheckpointId, Message, MessageAuthor, MessageId, RunId, RunStatus,
    Timestamp,
};
use rstest::rstest;

fn message(id: &str, run: &str, role: Role, intent: InputIntent) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: Some(RunId::new(run).unwrap()),
        role,
        text: id.into(),
        attachments: vec![],
        intent,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}

fn user_item(message: &Message, ordinal: u64) -> agent_domain::Item {
    let mut item = command_item(&format!("item-{}", message.id), ordinal);
    item.run = message.run.clone();
    item.kind = ItemKind::UserMessage {
        message: message.id.clone(),
    };
    item
}

fn checkpoint(id: &str, run: Option<&str>, ordinal: u64, status: CheckpointStatus) -> Checkpoint {
    Checkpoint {
        status,
        scope: None,
        id: CheckpointId::new(id).unwrap(),
        run: run.map(|run| RunId::new(run).unwrap()),
        run_ordinal: ordinal,
        native_heads: BTreeMap::new(),
        file_ref: format!("refs/orchestration/checkpoints/{ordinal}"),
        files: vec![],
    }
}

fn summary(run_id: &str, turn_count: u64, completed_at_ms: Option<i64>) -> CheckpointSummary {
    CheckpointSummary {
        checkpoint_id: format!("checkpoint-{run_id}"),
        scope_id: None,
        run_id: run_id.into(),
        turn_count,
        checkpoint_ref: format!("checkpoint-{run_id}"),
        status: CheckpointState::Ready,
        files: vec![],
        additions: 0,
        deletions: 0,
        changed_files_label: "0 changed files".into(),
        assistant_message_id: None,
        completed_at_ms,
        can_roll_back: true,
    }
}

fn turn(run_id: &str, file_path: Option<&str>, reveal_request_id: u64) -> DiffSelection {
    DiffSelection::Turn {
        run_id: run_id.into(),
        file_path: file_path.map(Into::into),
        reveal_request_id,
    }
}

fn branch(base_ref: Option<&str>) -> DiffSelection {
    DiffSelection::Branch {
        base_ref: base_ref.map(Into::into),
    }
}

#[test]
fn parses_turn_diff_input_when_from_turn_count_is_at_most_to_turn_count() {
    let parsed = turn_diff_request(&thread_id(), 1, 2, false).unwrap();
    assert_eq!(parsed.from_run_ordinal, 1);
    assert_eq!(parsed.to_run_ordinal, 2);
}

#[test]
fn parses_turn_diff_input_with_whitespace_ignoring_enabled() {
    let parsed = turn_diff_request(&thread_id(), 1, 2, true).unwrap();
    assert_eq!(parsed.ignore_whitespace, Some(true));
}

#[test]
fn parses_full_thread_diff_input_with_whitespace_ignoring_enabled() {
    let parsed = full_thread_diff_request(&thread_id(), 2, true);
    assert_eq!(parsed.ignore_whitespace, Some(true));
    assert_eq!((parsed.from_run_ordinal, parsed.to_run_ordinal), (0, 2));
}

#[test]
fn rejects_turn_diff_input_when_from_turn_count_exceeds_to_turn_count() {
    assert_eq!(
        turn_diff_request(&thread_id(), 3, 2, true),
        Err(TURN_DIFF_RANGE_ERROR.into())
    );
}

#[test]
fn rejects_thread_turn_diff_when_from_turn_count_exceeds_to_turn_count() {
    let diff = |from_run_ordinal, to_run_ordinal| TurnDiff {
        thread_id: thread_id(),
        from_run_ordinal,
        to_run_ordinal,
        diff: "patch".into(),
    };
    assert!(!accepts_turn_diff(&diff(3, 2)));
    assert!(accepts_turn_diff(&diff(2, 2)));
}

#[test]
fn defaults_each_thread_to_changes_without_requiring_git_status() {
    assert_eq!(DiffPanelSelection::default().selection, branch(None));
}

#[test]
fn keeps_a_custom_base_when_a_generic_open_selects_changes_again() {
    let mut store = DiffPanelSelection::default();
    store.select_branch_base_ref(Some("origin/release"));
    store.select_git_scope(DiffGitScope::Branch);
    store.select_turn("turn-1", None);
    store.select_git_scope(DiffGitScope::Branch);
    assert_eq!(store.selection, branch(Some("origin/release")));
}

#[test]
fn preserves_an_explicit_branch_selection() {
    let mut store = DiffPanelSelection::default();
    store.select_git_scope(DiffGitScope::Branch);
    assert_eq!(store.selection, branch(None));
}

#[test]
fn clears_incompatible_selection_fields_when_changing_scopes() {
    let mut store = DiffPanelSelection::default();
    store.select_turn("turn-1", Some("src/app.ts"));
    store.select_git_scope(DiffGitScope::Unstaged);
    assert_eq!(store.selection, DiffSelection::Unstaged);

    store.select_branch_base_ref(Some(" origin/main "));
    assert_eq!(store.selection, branch(Some("origin/main")));
}

#[test]
fn clears_a_threads_turn_and_file_when_selecting_working_tree_without_changing_another_threads_branch_base()
 {
    let mut store = DiffPanelSelection::default();
    let mut other = DiffPanelSelection::default();
    store.select_branch_base_ref(Some("origin/release"));
    store.select_turn("turn-1", Some("src/app.ts"));
    other.select_branch_base_ref(Some("origin/main"));

    store.select_git_scope(DiffGitScope::Unstaged);

    assert_eq!(store.selection, DiffSelection::Unstaged);
    assert_eq!(other.selection, branch(Some("origin/main")));

    store.select_git_scope(DiffGitScope::Branch);
    assert_eq!(store.selection, branch(Some("origin/release")));
}

#[test]
fn increments_the_reveal_request_when_opening_the_same_turn_file_again() {
    let mut store = DiffPanelSelection::default();
    store.select_turn("turn-1", Some("src/app.ts"));
    store.select_turn("turn-1", Some("src/app.ts"));
    assert_eq!(store.selection, turn("turn-1", Some("src/app.ts"), 2));
}

#[test]
fn restores_the_selected_branch_base_after_visiting_another_scope() {
    let mut store = DiffPanelSelection::default();
    store.select_branch_base_ref(Some("origin/main"));
    store.select_git_scope(DiffGitScope::Unstaged);
    store.select_git_scope(DiffGitScope::Branch);
    assert_eq!(store.selection, branch(Some("origin/main")));
}

#[test]
fn reconciles_a_missing_turn_selection_to_the_latest_available_turn() {
    let mut store = DiffPanelSelection::default();
    store.select_turn("turn-missing", Some("src/app.ts"));
    store.reconcile_turn_selection(&["turn-latest".into()]);
    assert_eq!(store.selection, turn("turn-latest", Some("src/app.ts"), 1));
}

#[test]
fn assigns_run_rollback_to_the_turn_start_message_instead_of_a_later_steer() {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-steered", 1, RunStatus::Completed));
    let start = message(
        "message-turn-start",
        "run-steered",
        Role::User,
        InputIntent::TurnStart,
    );
    let steer = message(
        "message-steer",
        "run-steered",
        Role::User,
        InputIntent::Steer,
    );
    let assistant = message(
        "message-assistant",
        "run-steered",
        Role::Assistant,
        InputIntent::TurnStart,
    );
    state.items = vec![user_item(&start, 0), user_item(&steer, 1)];
    state.messages = vec![start, steer, assistant];
    state.checkpoints = vec![checkpoint(
        "checkpoint-run-1",
        Some("run-steered"),
        1,
        CheckpointStatus::Ready,
    )];

    let targets = revert_targets(&state, &checkpoint_summaries(&state));

    assert_eq!(
        targets,
        vec![RevertTarget {
            message_id: "message-turn-start".into(),
            turn_count: 0,
        }]
    );
}

#[rstest]
#[case(CheckpointStatus::Missing)]
#[case(CheckpointStatus::Error)]
#[case(CheckpointStatus::Stale)]
fn only_ready_checkpoints_offer_rollback(#[case] status: CheckpointStatus) {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-1", 1, RunStatus::Completed));
    let start = message("message-1", "run-1", Role::User, InputIntent::QueuedTurn);
    state.items = vec![user_item(&start, 0)];
    state.messages = vec![start];
    state.checkpoints = vec![checkpoint("checkpoint-1", Some("run-1"), 1, status)];

    let summaries = checkpoint_summaries(&state);

    assert!(!summaries[0].can_roll_back);
    assert_eq!(revert_targets(&state, &summaries), vec![]);
}

#[test]
fn summarizes_run_checkpoints_with_their_files_and_last_assistant_message() {
    let mut state = thread_state("Thread");
    let mut completed = run("run-1", 1, RunStatus::Completed);
    completed.completed_at = Some(Timestamp::from_millis(5_000).unwrap());
    state.runs.push(completed);
    state.messages = vec![
        message("first", "run-1", Role::Assistant, InputIntent::TurnStart),
        message("last", "run-1", Role::Assistant, InputIntent::TurnStart),
    ];
    let mut captured = checkpoint("checkpoint-1", Some("run-1"), 1, CheckpointStatus::Ready);
    captured.files = vec![
        CheckpointFile {
            path: "a.rs".into(),
            kind: "modified".into(),
            additions: 3,
            deletions: 1,
        },
        CheckpointFile {
            path: "b.rs".into(),
            kind: "modified".into(),
            additions: 2,
            deletions: 0,
        },
    ];
    state.checkpoints = vec![
        checkpoint("baseline", None, 0, CheckpointStatus::Ready),
        captured,
    ];

    let summaries = checkpoint_summaries(&state);

    assert_eq!(summaries.len(), 1);
    let summary = &summaries[0];
    assert_eq!(summary.run_id, "run-1");
    assert_eq!(summary.turn_count, 1);
    assert_eq!(summary.checkpoint_ref, "refs/orchestration/checkpoints/1");
    assert_eq!(summary.assistant_message_id.as_deref(), Some("last"));
    assert_eq!((summary.additions, summary.deletions), (5, 1));
    assert_eq!(summary.changed_files_label, "2 changed files");
    assert_eq!(summary.completed_at_ms, Some(5_000));
    assert_eq!(quantity(1, "changed file"), "1 changed file");
}

#[test]
fn orders_turns_newest_first_by_turn_count_then_completion() {
    let ordered = ordered_turns(&[
        summary("a", 1, Some(1)),
        summary("b", 3, Some(1)),
        summary("c", 2, Some(1)),
        summary("d", 3, Some(9)),
    ]);
    let ids: Vec<_> = ordered.iter().map(|turn| turn.run_id.as_str()).collect();
    assert_eq!(ids, ["d", "b", "c", "a"]);
}

#[test]
fn defaults_to_changes_with_whitespace_hidden() {
    let view = diff_panel(
        &[summary("run-1", 1, None)],
        &DiffPanelSelection::default(),
        DEFAULT_DIFF_IGNORE_WHITESPACE,
    );
    assert_eq!(view.scope_label, "Changes");
    assert_eq!(view.section_title, "Changes");
    let scopes: Vec<_> = view
        .scopes
        .iter()
        .map(|scope| (scope.label.as_str(), scope.selected))
        .collect();
    assert_eq!(
        scopes,
        [
            ("Changes", true),
            ("Uncommitted", false),
            ("Latest turn", false)
        ]
    );
    assert_eq!(
        view.request,
        Some(DiffRequest::Branch {
            base_ref: None,
            ignore_whitespace: true,
        })
    );
    assert_eq!(view.whitespace_toggle_label, "Show whitespace changes");
    assert!(view.request.unwrap().intent(Some("/repo")).is_none());
}

#[test]
fn labels_the_latest_turn_and_requests_its_checkpoint_range() {
    let summaries = [summary("run-1", 1, None), summary("run-2", 2, None)];
    let mut selection = DiffPanelSelection::default();
    selection.select_scope(&DiffScopeChoice::LatestTurn, &ordered_turns(&summaries));

    let view = diff_panel(&summaries, &selection, false);

    assert_eq!(view.scope_label, "Latest turn");
    assert_eq!(view.section_title, "Turn 2");
    assert!(view.scopes[2].selected);
    let turns: Vec<_> = view
        .turns
        .iter()
        .map(|turn| (turn.label.as_str(), turn.selected))
        .collect();
    assert_eq!(turns, [("Turn 2", true), ("Turn 1", false)]);
    let request = view.request.unwrap();
    assert_eq!(
        request,
        DiffRequest::Turn {
            run_id: "run-2".into(),
            from_run_ordinal: 1,
            to_run_ordinal: 2,
            ignore_whitespace: false,
        }
    );
    assert!(matches!(
        request.intent(None),
        Some(Intent::ReadTurnDiff {
            from_run_ordinal: 1,
            to_run_ordinal: 2,
            ignore_whitespace: false,
        })
    ));
    assert_eq!(view.whitespace_toggle_label, "Hide whitespace changes");
}

#[test]
fn labels_an_older_turn_by_its_count_and_marks_only_the_submenu() {
    let summaries = [summary("run-1", 1, None), summary("run-2", 2, None)];
    let mut selection = DiffPanelSelection::default();
    selection.select_scope(
        &DiffScopeChoice::Turn {
            run_id: "run-1".into(),
        },
        &ordered_turns(&summaries),
    );

    let view = diff_panel(&summaries, &selection, true);

    assert_eq!(view.scope_label, "Turn 1");
    assert!(view.scopes.iter().all(|scope| !scope.selected));
    assert!(view.turns[1].selected);
    assert!(matches!(
        view.request,
        Some(DiffRequest::Turn {
            from_run_ordinal: 0,
            to_run_ordinal: 1,
            ..
        })
    ));
}

#[test]
fn shows_no_completed_turns_when_a_turn_is_selected_without_checkpoints() {
    let mut selection = DiffPanelSelection::default();
    selection.select_turn("run-gone", None);

    let view = diff_panel(&[], &selection, true);

    assert_eq!(
        view.empty_message.as_deref(),
        Some("No completed turns yet.")
    );
    assert_eq!(view.request, None);
    assert_eq!(view.scope_label, "Latest turn");
    assert_eq!(view.section_title, "Changes");
}

#[test]
fn uncommitted_loads_the_workspace_review() {
    let mut selection = DiffPanelSelection::default();
    selection.select_scope(&DiffScopeChoice::Unstaged, &[]);

    let view = diff_panel(&[], &selection, true);

    assert_eq!(view.scope_label, "Uncommitted");
    assert!(view.scopes[1].selected);
    assert!(matches!(
        view.request.unwrap().intent(Some("/repo")),
        Some(Intent::ReviewWorkspace { cwd }) if cwd == "/repo"
    ));
}
