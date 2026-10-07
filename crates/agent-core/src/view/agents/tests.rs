use super::*;
use crate::sync::fixtures::{run, thread_state};
use agent_domain::{CompletionWake, DeliveryState, NodeId, RunAttemptId, RunStatus, Timestamp};
use rstest::rstest;

fn at(iso: &str) -> Timestamp {
    Timestamp::parse(iso).unwrap()
}

fn task(id: &str) -> Task {
    Task {
        original_message: None,
        native_task: None,
        background: false,
        id: NodeId::new(id).unwrap(),
        native_key: id.into(),
        run: Some(RunId::new("run-1").unwrap()),
        attempt: RunAttemptId::new("attempt-1").unwrap(),
        child_thread: ThreadId::new("thread-child").unwrap(),
        parent_task: None,
        prompt: "Do the thing".into(),
        title: Some("Worker".into()),
        started_at: at("2026-06-20T00:00:00.000Z"),
        completed_at: None,
        model: Some("gpt-5.4".into()),
        status: ItemStatus::Running,
        result: None,
        progress: None,
        wake: CompletionWake::Always,
        delivery: DeliveryState::Pending,
        generation: 0,
    }
}

fn with(id: &str, change: impl FnOnce(&mut Task)) -> Task {
    let mut task = task(id);
    change(&mut task);
    task
}

fn state(runs: Vec<agent_domain::Run>, tasks: Vec<Task>) -> State {
    let mut state = thread_state("Thread");
    state.runs = runs;
    state.tasks = tasks;
    state
}

fn active_run() -> agent_domain::Run {
    run("run-1", 1, RunStatus::Running)
}

fn finished_run() -> agent_domain::Run {
    run("run-1", 1, RunStatus::Completed)
}

fn turn(state: &State) -> Option<AgentRoster> {
    roster(state, 0, |_| RowContext::default())
}

fn ids(roster: &AgentRoster) -> Vec<&str> {
    roster.rows.iter().map(|row| row.id.as_str()).collect()
}

fn row(task: Task) -> AgentRow {
    agent_row(&task, 0, RowContext::default())
}

#[test]
fn returns_null_when_the_thread_has_never_spawned_an_agent() {
    assert_eq!(turn(&state(vec![active_run()], vec![])), None);
}

#[test]
fn scopes_the_roster_to_the_active_run_and_orders_it_by_start_time() {
    let roster = turn(&state(
        vec![run("run-0", 0, RunStatus::Completed), active_run()],
        vec![
            with("b", |t| t.started_at = at("2026-06-20T00:00:05.000Z")),
            with("old", |t| t.run = Some(RunId::new("run-0").unwrap())),
            with("a", |t| t.started_at = at("2026-06-20T00:00:02.000Z")),
        ],
    ))
    .unwrap();

    assert_eq!(roster.run_id.as_deref(), Some("run-1"));
    assert_eq!(ids(&roster), ["a", "b"]);
    assert!(roster.turn_active);
}

#[test]
fn falls_back_to_the_most_recently_updated_agents_run_once_the_turn_settles() {
    let roster = turn(&state(
        vec![finished_run(), run("run-2", 2, RunStatus::Completed)],
        vec![
            with("old", |t| {
                t.status = ItemStatus::Completed;
                t.completed_at = Some(at("2026-06-20T00:00:01.000Z"));
            }),
            with("recent", |t| {
                t.run = Some(RunId::new("run-2").unwrap());
                t.status = ItemStatus::Completed;
                t.completed_at = Some(at("2026-06-20T00:00:09.000Z"));
            }),
        ],
    ))
    .unwrap();

    assert_eq!(roster.run_id.as_deref(), Some("run-2"));
    assert!(!roster.turn_active);
    assert_eq!(ids(&roster), ["recent"]);
}

#[test]
fn shows_how_many_of_the_turns_agents_are_still_working() {
    let roster = turn(&state(
        vec![active_run()],
        vec![
            with("a", |t| t.status = ItemStatus::Running),
            with("b", |t| t.status = ItemStatus::Waiting),
            with("c", |t| t.status = ItemStatus::Completed),
        ],
    ))
    .unwrap();

    assert_eq!(
        roster.pill,
        Some(AgentPill {
            label: "2/3".into(),
            accessibility_label: "2 of 3 agents working".into(),
        })
    );
}

#[test]
fn reports_the_finished_roster_while_the_turn_is_still_running() {
    let roster = turn(&state(
        vec![active_run()],
        vec![with("a", |t| t.status = ItemStatus::Completed)],
    ))
    .unwrap();

    assert_eq!(
        roster.pill,
        Some(AgentPill {
            label: "1 done".into(),
            accessibility_label: "1 agent done".into(),
        })
    );
}

#[test]
fn hides_itself_once_the_turn_is_over_and_nothing_is_working() {
    let roster = turn(&state(
        vec![finished_run()],
        vec![with("a", |t| t.status = ItemStatus::Completed)],
    ))
    .unwrap();

    assert_eq!(roster.pill, None);
    assert_eq!(turn(&state(vec![finished_run()], vec![])), None);
}

#[test]
fn stays_visible_after_the_turn_ends_while_an_agent_is_still_working() {
    let roster = turn(&state(
        vec![finished_run()],
        vec![with("a", |t| t.status = ItemStatus::Running)],
    ))
    .unwrap();

    assert_eq!(roster.pill.unwrap().label, "1/1");
}

#[rstest]
#[case(ItemStatus::Pending)]
#[case(ItemStatus::Running)]
#[case(ItemStatus::Waiting)]
fn keeps_a_mixed_group_live_while_a_member_is_active(#[case] status: ItemStatus) {
    assert_eq!(
        subagent_group_summary(&[ItemStatus::Completed, status]),
        SubagentGroupSummary {
            label: "Kicked off 2 subagents".into(),
            active: true,
            failed: false,
        }
    );
}

#[test]
fn settles_the_label_without_disguising_a_failed_member_as_success() {
    assert_eq!(
        subagent_group_summary(&[
            ItemStatus::Completed,
            ItemStatus::Failed,
            ItemStatus::Cancelled,
            ItemStatus::Interrupted,
        ]),
        SubagentGroupSummary {
            label: "Ran 4 subagents".into(),
            active: false,
            failed: true,
        }
    );
}

const CODEX: Driver = Driver::Codex;

#[test]
fn keeps_unknown_model_identities_and_does_not_invent_an_unreported_model() {
    let label = |input: MetadataInput| subagent_metadata(input).model_label;
    assert_eq!(
        label(MetadataInput {
            model: Some(" custom/model "),
            ..Default::default()
        }),
        "custom/model"
    );
    assert_eq!(
        label(MetadataInput {
            model: None,
            provider: Some((CODEX, &[])),
            ..Default::default()
        }),
        "Not reported"
    );
    assert_eq!(
        label(MetadataInput {
            model: Some(" "),
            ..Default::default()
        }),
        "Not reported"
    );
}

const PARENT_THREAD: ThreadWorkspace = ThreadWorkspace {
    project: "parent",
    worktree_path: None,
    branch: None,
};
const PARENT_PROJECT: ProjectWorkspace = ProjectWorkspace {
    id: "parent",
    title: "Parent",
    workspace_root: Some("/repo"),
};

fn entries(pairs: &[(&str, &str)]) -> Vec<AgentWorkspaceEntry> {
    pairs
        .iter()
        .map(|(label, value)| AgentWorkspaceEntry {
            label: (*label).into(),
            value: (*value).into(),
        })
        .collect()
}

fn workspace(
    child_thread: Option<ThreadWorkspace>,
    child_project: Option<ProjectWorkspace>,
) -> Vec<AgentWorkspaceEntry> {
    subagent_metadata(MetadataInput {
        parent_thread: Some(PARENT_THREAD),
        parent_project: Some(PARENT_PROJECT),
        child_thread,
        child_project,
        ..Default::default()
    })
    .workspace
}

#[test]
fn shows_another_project_and_its_branch_when_the_child_has_a_different_workspace() {
    assert_eq!(
        workspace(
            Some(ThreadWorkspace {
                project: "child",
                branch: Some("fix/agents"),
                worktree_path: Some("/worktrees/agents"),
            }),
            Some(ProjectWorkspace {
                id: "child",
                title: "Other project",
                workspace_root: Some("/other"),
            }),
        ),
        entries(&[("Project", "Other project"), ("Branch", "fix/agents")])
    );
}

#[test]
fn labels_a_detached_worktree_or_another_project_workspace_without_a_branch() {
    assert_eq!(
        workspace(
            Some(ThreadWorkspace {
                project: "parent",
                branch: None,
                worktree_path: Some("/worktrees/agents"),
            }),
            None,
        ),
        entries(&[("Worktree", "agents")])
    );
    assert_eq!(
        workspace(
            None,
            Some(ProjectWorkspace {
                id: "parent",
                title: "Same project",
                workspace_root: Some("/other"),
            }),
        ),
        entries(&[("Workspace", "other")])
    );
}

#[test]
fn hides_redundant_workspace_metadata_and_tolerates_unavailable_child_shells() {
    assert_eq!(
        workspace(
            Some(ThreadWorkspace {
                project: "parent",
                branch: Some("main"),
                worktree_path: Some("/repo"),
            }),
            Some(ProjectWorkspace {
                id: "parent",
                title: "Same project",
                workspace_root: Some("/repo"),
            }),
        ),
        vec![]
    );
    assert_eq!(workspace(None, None), vec![]);
}

#[test]
fn prefers_progress_for_live_work_and_results_for_settled_work() {
    let result = Some("Found two\n  problems");
    let progress = Some("Reading files");
    assert_eq!(
        subagent_detail_preview(ItemStatus::Running, result, progress).as_deref(),
        Some("Reading files")
    );
    assert_eq!(
        subagent_detail_preview(ItemStatus::Completed, result, progress).as_deref(),
        Some("Found two problems")
    );
    assert_eq!(
        subagent_detail_preview(ItemStatus::Failed, Some(" "), Some("Last progress")).as_deref(),
        Some("Last progress")
    );
    assert_eq!(
        subagent_detail_preview(ItemStatus::Pending, None, None),
        None
    );
}

#[rstest]
#[case(ItemStatus::Cancelled, "1 stopped", StatusTone::Inactive)]
#[case(ItemStatus::Interrupted, "1 stopped", StatusTone::Inactive)]
#[case(ItemStatus::Failed, "1 failed", StatusTone::Failed)]
#[case(ItemStatus::Completed, "✓ completed", StatusTone::Completed)]
fn reports_a_state_accurately_alongside_a_completed_agent(
    #[case] state: ItemStatus,
    #[case] status: &str,
    #[case] tone: StatusTone,
) {
    let summary = agent_spawn_summary(&[ItemStatus::Completed, state], 2);
    assert!(!summary.live);
    assert_eq!(summary.status, status);
    assert_eq!(summary.tone, tone);
}

#[test]
fn does_not_claim_completion_when_the_roster_is_missing_a_member() {
    let summary = agent_spawn_summary(&[ItemStatus::Completed], 2);
    assert_eq!(summary.status, "Status unavailable");
    assert_eq!(summary.tone, StatusTone::Inactive);
}

fn audit(change: impl FnOnce(&mut Task)) -> Task {
    with("agent", |t| {
        t.title = None;
        t.prompt = "Audit the timestamps".into();
        change(t);
    })
}

#[test]
fn leads_with_progress_while_the_agent_is_working() {
    let row = row(audit(|t| {
        t.progress = Some("Reading files".into());
        t.result = Some("stale result".into());
    }));

    assert_eq!(row.detail.as_deref(), Some("Reading files"));
    assert_eq!(row.tone, StatusTone::Working);
    assert!(row.live);
}

#[test]
fn leads_with_the_result_once_the_agent_has_settled() {
    let row = row(audit(|t| {
        t.status = ItemStatus::Completed;
        t.progress = Some("Reading files".into());
        t.result = Some("Found two\n  problems".into());
    }));

    assert_eq!(row.detail.as_deref(), Some("Found two problems"));
    assert_eq!(row.tone, StatusTone::Completed);
    assert_eq!(row.status_label, "Completed");
}

#[test]
fn shows_a_failures_text_since_that_is_where_the_error_lands() {
    let row = row(audit(|t| {
        t.status = ItemStatus::Failed;
        t.result = Some("Timed out".into());
    }));

    assert_eq!(row.detail.as_deref(), Some("Timed out"));
    assert_eq!(row.tone, StatusTone::Failed);
}

#[test]
fn falls_back_to_a_trimmed_prompt_when_the_agent_has_no_title() {
    let long = row(audit(|t| t.prompt = "x".repeat(200)));
    let titled = row(audit(|t| {
        t.title = Some("Subagent: /root/my_worker".into())
    }));

    assert_eq!(long.title.chars().count(), 80);
    assert!(long.title.ends_with("..."));
    assert_eq!(titled.title, "My Worker");
}

#[test]
fn uses_the_status_label_when_there_is_nothing_to_report_yet() {
    let row = row(audit(|t| t.status = ItemStatus::Pending));

    assert_eq!(row.detail, None);
    assert_eq!(row.status_label, "Working");
}

#[test]
fn bounds_long_result_previews_without_dropping_the_agents_status() {
    let row = row(audit(|t| {
        t.status = ItemStatus::Failed;
        t.result = Some("x".repeat(400));
    }));

    assert_eq!(row.detail, Some(format!("{}…", "x".repeat(280))));
    assert_eq!(row.status_label, "Failed");
}

#[test]
fn shows_readable_result_text_and_suppresses_generic_completion_messages() {
    assert_eq!(
        subagent_card_detail(Some(
            "- Updated `app.ts` with [the fix](https://example.com).\n- Checked tests."
        ))
        .as_deref(),
        Some("Updated app.ts with the fix. Checked tests.")
    );
    assert_eq!(
        subagent_card_detail(Some("Child task ended with status failed.")),
        None
    );
    assert_eq!(subagent_card_detail(Some("  ")), None);
}
const STARTED_AT: i64 = 1_790_000_000_000;
const COMPLETED_AT: i64 = STARTED_AT + 10_000;
const NOW: i64 = STARTED_AT + 3_600_000;

#[rstest]
#[case(ItemStatus::Completed)]
#[case(ItemStatus::Failed)]
#[case(ItemStatus::Cancelled)]
#[case(ItemStatus::Interrupted)]
fn does_not_count_the_age_of_settled_work_with_unknown_completion_timing(
    #[case] status: ItemStatus,
) {
    assert_eq!(
        subagent_elapsed_ms(status, Some(STARTED_AT), None, NOW),
        None
    );
    assert_eq!(
        subagent_elapsed_ms(status, Some(STARTED_AT), Some(COMPLETED_AT), NOW),
        Some(10_000)
    );
}

#[test]
fn counts_a_resumed_activation_despite_a_stale_previous_completion_timestamp() {
    assert_eq!(
        subagent_elapsed_ms(
            ItemStatus::Running,
            Some(STARTED_AT),
            Some(COMPLETED_AT),
            NOW
        ),
        Some(3_600_000)
    );
    assert_eq!(
        subagent_elapsed_ms(ItemStatus::Running, None, None, NOW),
        None
    );
}

#[test]
fn live_rows_count_to_now_and_settled_rows_stop_at_completion() {
    let started = at("2026-06-20T00:00:00.000Z").millis();
    let live = agent_row(&task("a"), started + 65_000, RowContext::default());
    let settled = agent_row(
        &with("b", |t| {
            t.status = ItemStatus::Completed;
            t.completed_at = Some(at("2026-06-20T00:00:22.000Z"));
        }),
        started + 65_000,
        RowContext::default(),
    );

    assert_eq!(live.elapsed.as_deref(), Some("1m 5s"));
    assert_eq!(settled.elapsed.as_deref(), Some("22s"));
}

#[rstest]
#[case("gpt-5.5", "GPT-5.5")]
#[case("gpt-5.3-codex-spark", "GPT-5.3-Codex-Spark")]
#[case("custom/model-v1", "custom/model-v1")]
#[case("claude-opus-4-6", "Claude Opus 4.6")]
#[case("composer-2", "Composer 2")]
#[case("grok-4-fast", "Grok 4 Fast")]
#[case("gemini-3.8-flash-high", "Gemini 3.8 Flash High")]
#[case("anthropic/claude-sonnet-4-6", "anthropic/Claude Sonnet 4.6")]
fn formats_reported_models_without_a_catalog(#[case] model: &str, #[case] expected: &str) {
    assert_eq!(
        subagent_metadata(MetadataInput {
            model: Some(model),
            ..Default::default()
        })
        .model_label,
        expected
    );
}

#[test]
fn names_a_catalog_model_by_its_slug_name_or_alias() {
    let models = [CatalogModel {
        slug: "gpt-5.4".into(),
        name: "My GPT model".into(),
    }];
    let label = |model| {
        subagent_metadata(MetadataInput {
            model: Some(model),
            provider: Some((CODEX, &models)),
            ..Default::default()
        })
        .model_label
    };
    assert_eq!(label("gpt-5.4"), "My GPT model");
    assert_eq!(label("my gpt MODEL"), "My GPT model");
    assert_eq!(label("5.4"), "My GPT model");
    assert_eq!(label("gpt-5.5"), "GPT-5.5");
}

#[test]
fn formats_codex_task_paths_as_display_titles() {
    for (title, expected) in [
        ("Subagent: Review the parser", "Review the parser"),
        ("subagent:/root/fix_parser_bug", "Fix Parser Bug"),
        ("/root/a/b/write  tests/", "Write Tests"),
        ("/root/__/", "/root/__/"),
        ("/tmp/other", "/tmp/other"),
    ] {
        assert_eq!(format_subagent_display_title(title), expected, "{title}");
    }
}
