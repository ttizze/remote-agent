use super::*;
use crate::sync::fixtures::*;
use agent_domain::{
    PlanStep, Question, QuestionOption, Request, RequestBody, RequestStatus, ResponseCapability,
    RunAttemptId, RuntimeRequestId, Timestamp,
};
use rstest::rstest;

fn todo(id: &str, run: &str, steps: &[(&str, &str)]) -> Plan {
    Plan {
        kind: PlanKind::Todo,
        steps: steps
            .iter()
            .map(|(text, status)| PlanStep {
                text: (*text).into(),
                status: (*status).into(),
            })
            .collect(),
        ..plan(id, run)
    }
}

fn plan_item(id: &str, kind: ItemKind, text: &str, at_ms: i64) -> agent_domain::Item {
    let mut item = command_item(id, 0);
    item.kind = kind;
    item.text = text.into();
    item.completed_at = Some(Timestamp::from_millis(at_ms).unwrap());
    item
}

#[test]
fn selects_the_latest_proposed_plan_for_a_run() {
    let mut state = thread_state("Thread");
    let mut proposed = plan("plan-1", "run-1");
    proposed.markdown = "Plan".into();
    state.plans.push(proposed);
    state.items.push(plan_item(
        "item-plan",
        ItemKind::ProposedPlan {
            plan: PlanId::new("plan-1").unwrap(),
        },
        "Plan",
        1_000,
    ));
    let latest = latest_proposed_plan(&state, Some(&RunId::new("run-1").unwrap())).unwrap();
    assert_eq!(latest.markdown, "Plan");
}

#[rstest]
#[case::pending("pending", PlanStepStatus::Pending)]
#[case::running_codex("inProgress", PlanStepStatus::InProgress)]
#[case::running_claude("in_progress", PlanStepStatus::InProgress)]
#[case::completed("completed", PlanStepStatus::Completed)]
fn keeps_task_progress_available_to_the_composer(
    #[case] status: &str,
    #[case] expected: PlanStepStatus,
) {
    let mut state = thread_state("Thread");
    state.plans.push(todo(
        "plan-tasks",
        "run-tasks",
        &[("Verify the change", status)],
    ));
    state.items.push(plan_item(
        "item-tasks",
        ItemKind::TodoList {
            plan: PlanId::new("plan-tasks").unwrap(),
        },
        "",
        5_000,
    ));
    let plan = active_plan_state(&state, Some(&RunId::new("run-tasks").unwrap())).unwrap();
    assert_eq!(plan.run_id, "run-tasks");
    assert_eq!(plan.created_at_ms, 5_000);
    assert_eq!(
        plan.steps,
        [PlanStepView {
            step: "Verify the change".into(),
            status: expected,
        }]
    );
}

fn gates() -> PlanFollowUpGates {
    PlanFollowUpGates {
        pending_user_input_count: 0,
        interaction_mode: InteractionMode::Plan,
        latest_turn_settled: true,
        has_actionable_proposed_plan: true,
        has_composer_attachments: false,
    }
}

#[test]
fn shows_plan_actions_for_a_settled_actionable_plan_without_attachments() {
    assert!(should_show_plan_follow_up_prompt(gates()));
}

#[test]
fn hides_plan_actions_while_the_composer_has_staged_attachments() {
    assert!(!should_show_plan_follow_up_prompt(PlanFollowUpGates {
        has_composer_attachments: true,
        ..gates()
    }));
}

#[test]
fn preserves_the_existing_plan_follow_up_gates() {
    for closed in [
        PlanFollowUpGates {
            pending_user_input_count: 1,
            ..gates()
        },
        PlanFollowUpGates {
            interaction_mode: InteractionMode::Default,
            ..gates()
        },
        PlanFollowUpGates {
            latest_turn_settled: false,
            ..gates()
        },
        PlanFollowUpGates {
            has_actionable_proposed_plan: false,
            ..gates()
        },
    ] {
        assert!(!should_show_plan_follow_up_prompt(closed));
    }
}

#[test]
fn uses_run_status_as_the_settlement_boundary() {
    let run = RunId::new("run-1").unwrap();
    let active = RunId::new("run-active").unwrap();
    assert!(is_latest_run_settled(
        Some((&run, RunStatus::Completed)),
        None
    ));
    assert!(!is_latest_run_settled(
        Some((&run, RunStatus::Running)),
        Some(&run)
    ));
    assert!(!is_latest_run_settled(
        Some((&run, RunStatus::Queued)),
        Some(&active)
    ));
}

fn planned(status: RunStatus) -> State {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-1", 1, status));
    let mut proposed = plan("plan-1", "run-1");
    proposed.markdown = "# Ship the importer\n\n- Parse\n- Store".into();
    state.plans.push(proposed);
    state
}

#[test]
fn offers_the_plan_follow_up_once_the_planning_run_settles() {
    let state = planned(RunStatus::Completed);
    let view = plan_view(&state, InteractionMode::Plan, false);
    assert!(view.show_plan_follow_up_prompt);
    let proposed = view.active_proposed_plan.unwrap();
    assert_eq!(proposed.id, "plan-1");
    assert_eq!(proposed.title.as_deref(), Some("Ship the importer"));
    assert_eq!(
        view.banner,
        Some(PlanFollowUpBanner {
            label: "Plan ready".into(),
            title: Some("Ship the importer".into()),
        })
    );
    assert!(!plan_view(&state, InteractionMode::Default, false).show_plan_follow_up_prompt);
    assert!(!plan_view(&state, InteractionMode::Plan, true).show_plan_follow_up_prompt);

    let running = plan_view(&planned(RunStatus::Running), InteractionMode::Plan, false);
    assert_eq!(running.active_proposed_plan, None);
    assert!(!running.show_plan_follow_up_prompt && running.banner.is_none());

    let mut implemented = planned(RunStatus::Completed);
    implemented.plans[0].implemented_by = Some(RunId::new("run-2").unwrap());
    let view = plan_view(&implemented, InteractionMode::Plan, false);
    assert!(!view.active_proposed_plan.unwrap().actionable);
    assert!(!view.show_plan_follow_up_prompt);
}

#[test]
fn a_pending_question_holds_back_the_plan_follow_up() {
    let mut state = planned(RunStatus::Completed);
    state.requests.push(Request {
        owner_path: vec![],
        id: RuntimeRequestId::new("question").unwrap(),
        attempt: RunAttemptId::new("attempt").unwrap(),
        native_key: "question".into(),
        body: RequestBody::Questions {
            questions: vec![Question {
                required: true,
                id: "q".into(),
                header: "Q".into(),
                question: "Which?".into(),
                multiple: false,
                options: vec![QuestionOption {
                    label: "A".into(),
                    description: None,
                }],
            }],
        },
        capability: ResponseCapability::Message,
        status: RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: Default::default(),
        created_at: at(),
        resolved_at: None,
    });
    assert!(!plan_view(&state, InteractionMode::Plan, false).show_plan_follow_up_prompt);
}

#[test]
fn a_bounded_snapshot_reads_the_plan_text_from_its_item() {
    let mut state = planned(RunStatus::Completed);
    state.plans[0].markdown.clear();
    state.items.push(plan_item(
        "item-plan",
        ItemKind::ProposedPlan {
            plan: PlanId::new("plan-1").unwrap(),
        },
        "## From the item",
        1_000,
    ));
    let proposed = latest_proposed_plan(&state, None).unwrap();
    assert_eq!(proposed.markdown, "## From the item");
    assert_eq!(proposed.title.as_deref(), Some("From the item"));
}

#[test]
fn prefers_the_latest_runs_plan_and_orders_by_item_time() {
    let mut state = thread_state("Thread");
    state.plans.extend([
        plan("late", "run-1"),
        plan("early", "run-1"),
        plan("other", "run-2"),
    ]);
    for (id, at_ms) in [("late", 3_000), ("early", 1_000), ("other", 9_000)] {
        state.items.push(plan_item(
            &format!("item-{id}"),
            ItemKind::ProposedPlan {
                plan: PlanId::new(id).unwrap(),
            },
            "",
            at_ms,
        ));
    }
    let latest = |run: Option<&str>| {
        latest_proposed_plan(&state, run.map(|run| RunId::new(run).unwrap()).as_ref())
            .unwrap()
            .id
    };
    assert_eq!(latest(Some("run-1")), "late");
    assert_eq!(latest(None), "other");
    assert_eq!(latest(Some("run-without-plans")), "other");
}

#[test]
fn task_progress_follows_only_the_working_runs_own_list() {
    let mut state = thread_state("Thread");
    state.runs.push(run("run-1", 1, RunStatus::Running));
    state.plans.push(todo(
        "tasks",
        "run-1",
        &[
            ("Read", "completed"),
            ("Edit", "in_progress"),
            ("Test", "pending"),
        ],
    ));
    let view = plan_view(&state, InteractionMode::Default, false);
    assert_eq!(
        view.tasks,
        Some(TasksProgressView {
            step: "Edit".into(),
            completed_steps: 1,
            total_steps: 3,
        })
    );
    state.runs[0].status = RunStatus::Completed;
    state.runs.push(run("run-2", 2, RunStatus::Running));
    let view = plan_view(&state, InteractionMode::Default, false);
    assert_eq!(view.active_plan.unwrap().run_id, "run-1");
    assert_eq!(view.tasks, None);
    state.runs[1].status = RunStatus::Completed;
    state.plans[0]
        .steps
        .iter_mut()
        .for_each(|step| step.status = "completed".into());
    state.plans[0].run = RunId::new("run-2").unwrap();
    assert_eq!(
        plan_view(&state, InteractionMode::Default, false).tasks,
        None
    );
}
