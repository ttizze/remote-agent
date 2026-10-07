use super::*;
use crate::sync::fixtures::selection;
use crate::view::work_log::fixtures::*;
use crate::view::work_log::presentation::summarize_tool_group;
use crate::view::work_log::{NativeApp, ToolSourceKind};
use agent_domain::*;
use rstest::rstest;
use serde_json::json;

const RUN: &str = "run-1";

fn ts(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn run_id(id: &str) -> RunId {
    RunId::new(id).unwrap()
}

/// An item of run-1 at `at`, the reference `base` item.
fn base(item: Item, at: &str, ordinal: u64) -> Item {
    item.run(RUN)
        .ordinal(ordinal)
        .started(at)
        .completed(Some(at))
}

fn runless(item: Item) -> Item {
    Item { run: None, ..item }
}

fn running(item: Item) -> Item {
    item.status(ItemStatus::Running)
}

fn with_id(item: Item, id: &str) -> Item {
    Item {
        id: TurnItemId::new(id).unwrap(),
        ..item
    }
}

fn user_message_at(at: &str) -> Item {
    base(user_message("item-user", "message-user"), at, 0).text("Run checks")
}

fn user() -> Item {
    user_message_at("2026-06-20T00:00:01.000Z")
}

fn command_at(at: &str) -> Item {
    base(command("item-command", "vp check"), at, 1).text("ok")
}

fn command_item() -> Item {
    command_at("2026-06-20T00:00:02.000Z")
}

fn assistant_at(at: &str) -> Item {
    base(
        assistant_message("item-assistant", "message-assistant"),
        at,
        2,
    )
    .text("Done")
}

fn assistant() -> Item {
    assistant_at("2026-06-20T00:00:03.000Z")
}

fn user_with(id: &str, message: &str, at: &str, ordinal: u64) -> Item {
    base(user_message(id, message), at, ordinal).text("Run checks")
}

fn assistant_with(id: &str, message: &str, text: &str, at: &str, ordinal: u64) -> Item {
    base(assistant_message(id, message), at, ordinal).text(text)
}

/// Every item order is its position.
fn positioned(items: Vec<Item>) -> Vec<Item> {
    items
        .into_iter()
        .enumerate()
        .map(|(position, item)| item.ordinal(position as u64))
        .collect()
}

/// A thread holding the items, with a completed run for each run they name.
fn thread(items: Vec<Item>) -> State {
    let mut runs: Vec<Run> = vec![];
    for (index, item) in items.iter().enumerate() {
        if let Some(id) = &item.run
            && !runs.iter().any(|run| &run.id == id)
        {
            runs.push(run(id.as_str(), index as u64 + 1, RunStatus::Completed));
        }
    }
    State {
        runs,
        ..state(items)
    }
}

fn feed(items: Vec<Item>) -> Vec<FeedRow> {
    build_thread_feed(&thread(items))
}

fn activities(rows: &[FeedRow]) -> Vec<&FeedActivity> {
    rows.iter().flat_map(FeedRow::activities).collect()
}

fn latest(status: RunStatus, started: Option<&str>, completed: Option<&str>) -> FeedLatestRun {
    FeedLatestRun {
        run: run_id(RUN),
        status,
        started_at: started.map(ts),
        completed_at: completed.map(ts),
    }
}

fn running_run(started: Option<&str>) -> Option<FeedLatestRun> {
    Some(latest(RunStatus::Running, started, None))
}

fn completed_run() -> Option<FeedLatestRun> {
    Some(latest(
        RunStatus::Completed,
        Some("2026-06-20T00:00:01.000Z"),
        Some("2026-06-20T00:00:03.000Z"),
    ))
}

fn runs(ids: &[&str]) -> BTreeSet<RunId> {
    ids.iter().map(|id| run_id(id)).collect()
}

fn groups(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

fn present(
    feed: &[FeedRow],
    latest_run: Option<FeedLatestRun>,
    expanded_runs: BTreeSet<RunId>,
) -> Vec<FeedRow> {
    present_working(
        feed,
        latest_run,
        expanded_runs,
        BTreeSet::new(),
        None,
        false,
    )
}

fn present_working(
    feed: &[FeedRow],
    latest_run: Option<FeedLatestRun>,
    expanded_runs: BTreeSet<RunId>,
    expanded_work_groups: BTreeSet<String>,
    started_at: Option<&str>,
    runless_work_active: bool,
) -> Vec<FeedRow> {
    derive_thread_feed_presentation(
        feed,
        &FeedInput {
            latest_run,
            expanded_runs,
            expanded_work_groups,
            active_work_started_at: started_at.map(ts),
            runless_work_active,
            pending: vec![],
        },
    )
}

fn kind(row: &FeedRow) -> &'static str {
    match row {
        FeedRow::Message { .. } => "message",
        FeedRow::ActivityGroup(_) => "activity-group",
        FeedRow::WorkToggle(_) => "work-toggle",
        FeedRow::RunFold { .. } => "run-fold",
        FeedRow::Thinking { .. } => "thinking",
        FeedRow::Handoff { .. } => "handoff",
        FeedRow::PendingMessage(_) => "pending-message",
    }
}

fn kinds(rows: &[FeedRow]) -> Vec<&'static str> {
    rows.iter().map(kind).collect()
}

fn ids(rows: &[FeedRow]) -> Vec<&str> {
    rows.iter().map(FeedRow::id).collect()
}

fn toggle(rows: &[FeedRow]) -> &WorkToggle {
    rows.iter()
        .find_map(|row| match row {
            FeedRow::WorkToggle(toggle) => Some(toggle),
            _ => None,
        })
        .expect("a work toggle")
}

fn message(row: &FeedRow) -> &ChatMessage {
    match row {
        FeedRow::Message { message, .. } => message,
        row => panic!("expected a message: {row:?}"),
    }
}

fn item_ids(row: &FeedRow) -> Vec<&str> {
    row.activities()
        .iter()
        .map(|activity| activity.item.id.as_str())
        .collect()
}

fn group_item_ids(rows: &[FeedRow]) -> Vec<Vec<&str>> {
    rows.iter()
        .filter(|row| matches!(row, FeedRow::ActivityGroup(_)))
        .map(item_ids)
        .collect()
}

fn titled(item: Item, title: &str) -> Item {
    let mut item = item;
    match &mut item.kind {
        ItemKind::CommandExecution { title: slot, .. } => *slot = Some(title.into()),
        ItemKind::DynamicTool { presentation, .. } => presentation.title = Some(title.into()),
        _ => unreachable!("untitled item kind"),
    }
    item
}

fn stored_message(id: &str, role: Role, text: &str) -> Message {
    agent_domain::Message {
        run: Some(run_id(RUN)),
        ..crate::view::work_log::fixtures::message(id, role, text)
    }
}

fn provider_error(id: &str, class: &str, message: &str) -> Item {
    let mut item = error(id, message);
    if let ItemKind::Error { class: slot, .. } = &mut item.kind {
        *slot = Some(class.into());
    }
    item
}

#[test]
fn keeps_historical_plan_detail_accessible_from_its_paged_turn_item() {
    let item = base(
        proposed_plan("historical-plan", "plan-historical"),
        "2026-08-29T00:00:00.000Z",
        1,
    )
    .text("Full historical plan text");
    let rows = feed(vec![item]);
    let activity = activities(&rows)[0];
    assert_eq!(
        activity.detail.as_deref(),
        Some("Full historical plan text")
    );
    assert!(
        activity
            .full_detail
            .as_deref()
            .unwrap()
            .contains("Full historical plan text")
    );
}

#[test]
fn shows_only_the_structured_path_in_expanded_mobile_read_details() {
    let item = titled(
        base(
            dynamic_tool(
                "read-detail",
                "Read",
                json!({ "path": "src/env.ts" }),
                Some(json!("---\nname: env\n---\nsecret content")),
            ),
            "2026-06-20T00:00:03.000Z",
            2,
        ),
        "Read src/env.ts",
    );
    let rows = feed(vec![item.clone()]);
    let activity = activities(&rows)[0];
    assert_eq!(activity.full_detail.as_deref(), Some("src/env.ts"));
    assert!(activity.can_expand);
    assert!(!activity.copy_text.contains("secret content"));
    assert!(
        !activity
            .full_detail
            .as_deref()
            .unwrap()
            .contains("\"visibility\"")
    );

    let mut without_path = with_id(item, "read-without-path");
    if let ItemKind::DynamicTool { input, .. } = &mut without_path.kind {
        *input = Json(json!({}));
    }
    let rows = feed(vec![without_path]);
    let activity = activities(&rows)[0];
    assert_eq!(activity.full_detail, None);
    assert!(!activity.can_expand);
}

fn approval_with_prompt(id: &str, kind: &str, prompt: &str) -> Request {
    request(
        &format!("request-{id}"),
        RequestBody::Approval {
            kind: kind.into(),
            title: String::new(),
            detail: Some(prompt.into()),
            options: vec![],
            input: Json(json!(null)),
        },
    )
}

#[test]
fn keeps_approval_prompts_rather_than_presenting_them_as_tool_work() {
    let approvals = [
        ("approve-read", "file-read"),
        ("approve-command", "command"),
        ("approve-edit", "file-change"),
    ];
    let items = approvals
        .iter()
        .enumerate()
        .map(|(index, (id, _))| {
            base(
                approval_item(id, &format!("request-{id}")),
                &format!("2026-06-20T00:00:0{}.000Z", index + 1),
                index as u64 + 1,
            )
        })
        .collect();
    let state = State {
        requests: approvals
            .iter()
            .map(|(id, kind)| approval_with_prompt(id, kind, &format!("Allow {kind}?")))
            .collect(),
        ..thread(items)
    };
    let rows = build_thread_feed(&state);
    let activities = activities(&rows);
    assert_eq!(
        activities
            .iter()
            .map(|activity| work_entry_row_label(&activity.work_entry, false))
            .collect::<Vec<_>>(),
        ["Allow file-read?", "Allow command?", "Allow file-change?"]
    );
    assert!(activities[0].can_expand);
    let summary = summarize_tool_group(
        &activities[1..]
            .iter()
            .map(|activity| activity.work_entry.clone())
            .collect::<Vec<_>>(),
    )
    .summary;
    assert!(
        !summary.contains("Ran") && !summary.contains("changed"),
        "{summary}"
    );
}

mod build_thread_feed {
    use super::*;

    #[test]
    fn keeps_async_answers_in_question_history_instead_of_user_bubbles() {
        let question = base(
            user_input_item("question", "question"),
            "2026-06-20T00:00:01.000Z",
            0,
        );
        let reply = with_id(
            base(
                user_message("answer", "async-answer:question"),
                "2026-06-20T00:00:01.000Z",
                1,
            ),
            "answer",
        )
        .text("Run checks");
        let request = Request {
            answers: Some(BTreeMap::from([(
                "color".to_owned(),
                Answer::Text("Blue".into()),
            )])),
            ..questions("question", vec![])
        };
        let state = State {
            requests: vec![request],
            ..thread(vec![question, reply.clone()])
        };
        let rows = build_thread_feed(&state);
        assert_eq!(rows.len(), 1);
        let answer = rows[0].activities()[0]
            .work_entry
            .question_answer
            .as_ref()
            .expect("the question answer");
        assert_eq!(answer.request.as_str(), "question");
        assert_eq!(
            answer.answers,
            [("color".to_owned(), Answer::Text("Blue".into()))]
        );
        assert_eq!(kind(&feed(vec![reply])[0]), "message");
    }

    #[test]
    fn omits_cached_tool_output_and_patch_bodies_from_expanded_and_copied_activity() {
        let raw = "RAW_TOOL_OUTPUT";
        let items = vec![
            command_item().text(raw),
            base(
                dynamic_tool(
                    "dynamic-output",
                    "example",
                    json!({ "query": "keep input" }),
                    Some(json!({ "text": raw })),
                ),
                "2026-06-20T00:00:03.000Z",
                2,
            ),
            base(
                file_change(
                    "file-output",
                    json!([{ "path": "src/example.ts", "kind": "update", "diff": raw }]),
                ),
                "2026-06-20T00:00:04.000Z",
                3,
            )
            .text(raw),
        ];
        let rows = feed(items);
        let activities = activities(&rows);
        assert_eq!(activities.len(), 3);
        for activity in &activities {
            assert_eq!(activity.work_entry.detail, None);
            assert!(!activity.full_detail.as_deref().unwrap_or("").contains(raw));
            assert!(!activity.copy_text.contains(raw));
        }
        assert_eq!(activities[0].detail.as_deref(), Some("vp check"));
        assert!(
            activities[1]
                .full_detail
                .as_deref()
                .unwrap()
                .contains("keep input")
        );
        assert_eq!(activities[2].detail.as_deref(), Some("src/example.ts"));
    }

    #[test]
    fn expands_tool_rows_only_when_they_have_detail_or_withheld_output() {
        let items = vec![
            base(command("item-command", ""), "2026-06-20T00:00:02.000Z", 1).output_omitted(),
            base(
                dynamic_tool("dynamic-empty", "example", json!({}), None),
                "2026-06-20T00:00:03.000Z",
                2,
            ),
            base(
                dynamic_tool(
                    "read-omitted",
                    "Read",
                    json!({ "path": "src/env.ts" }),
                    None,
                ),
                "2026-06-20T00:00:04.000Z",
                3,
            )
            .output_omitted(),
        ];
        let rows = feed(items);
        assert_eq!(
            activities(&rows)
                .iter()
                .map(|activity| (activity.can_expand, activity.fetches_detail))
                .collect::<Vec<_>>(),
            [(true, true), (false, false), (true, true)]
        );
    }

    #[test]
    fn keeps_prominent_activity_visible_while_it_is_running() {
        assert!(thread_feed_activity_is_visible(
            true,
            Some(FeedStatus::Neutral),
            true,
            None
        ));
        assert!(!thread_feed_activity_is_visible(
            false,
            Some(FeedStatus::Neutral),
            true,
            None
        ));
    }

    #[test]
    fn keeps_provider_notices_visible_outside_completed_work_folds_without_failure_styling() {
        let message = "Safeguards flagged this message. Switched to Opus 4.8.";
        let item = base(
            system_notice("item-system-notice", message),
            "2026-06-20T00:00:02.000Z",
            1,
        );
        let rows = feed(vec![item]);
        let presented = present(&rows, None, BTreeSet::new());
        let activities = activities(&presented);
        assert_eq!(activities.len(), 1);
        let activity = activities[0];
        assert_eq!(activity.summary, message);
        assert_eq!(activity.detail.as_deref(), Some(message));
        assert!(activity.prominent);
        assert!(!activity.tool_like);
        assert_eq!(activity.status, None);
        assert_eq!(activity.icon, WorkIcon::Warning);
        assert_eq!(activity.work_entry.tone, WorkTone::Info);
        assert_eq!(activity.work_entry.item_type, Some(ItemType::SystemNotice));
        assert!(!kinds(&presented).contains(&"run-fold"));
    }

    #[test]
    fn presents_a_usage_limit_stop_as_a_warning_while_preserving_its_explanation() {
        let message = "Plan usage limit reached. Try again after reset.";
        let mut item = base(
            provider_error("item-limit", "usage_limit", message),
            "2026-06-20T00:00:02.000Z",
            1,
        )
        .status(ItemStatus::Failed)
        .completed(Some("2026-06-20T00:00:02.000Z"));
        if let ItemKind::Error { code, .. } = &mut item.kind {
            *code = Some("usageLimitExceeded".into());
        }
        let rows = feed(vec![item]);
        let activity = activities(&rows)[0];
        assert_eq!(activity.summary, "Usage limit reached");
        assert_eq!(activity.status, Some(FeedStatus::Neutral));
        assert_eq!(activity.icon, WorkIcon::Warning);
        assert!(activity.full_detail.as_deref().unwrap().contains(message));
    }

    /// The Host titles a failed retry by its failure ("Provider error" or
    /// "Usage limit reached"); the reference fixture names it "Provider retry failed".
    #[rstest]
    #[case::transport_error("transport_error", "Provider error")]
    #[case::usage_limit("usage_limit", "Usage limit reached")]
    fn presents_retries_and_clears_warning_markers_on_recovery(
        #[case] class: &str,
        #[case] failed_title: &str,
    ) {
        let retry = |status: ItemStatus| {
            let mut item = base(
                provider_error(
                    "item-provider-retry",
                    class,
                    "The response stream disconnected.",
                ),
                "2026-06-20T00:00:02.000Z",
                1,
            )
            .status(status);
            if status.terminal() {
                item = item.completed(Some("2026-06-20T00:00:02.000Z"));
            }
            if let ItemKind::Error {
                retry,
                code,
                retryable,
                ..
            } = &mut item.kind
            {
                *retry = Some(RetryProgress {
                    attempt: 2,
                    max_attempts: Some(5),
                    delay_ms: None,
                });
                *code = Some("responseStreamDisconnected".into());
                *retryable = Some(true);
            }
            item
        };
        let running_rows = feed(vec![retry(ItemStatus::Running)]);
        let recovered_rows = feed(vec![retry(ItemStatus::Completed)]);
        let recovered = activities(&recovered_rows)[0];
        if class == "usage_limit" {
            assert_eq!(recovered.status, Some(FeedStatus::Success));
            assert_eq!(recovered.icon, WorkIcon::Check);
        }
        let failed_rows = feed(vec![
            retry(ItemStatus::Failed),
            command_at("2026-06-20T00:00:03.000Z").ordinal(2),
        ]);
        let running = activities(&running_rows)[0];
        assert_eq!(running.summary, "Provider retry");
        assert_eq!(running.status, Some(FeedStatus::Neutral));
        assert!(!running.tool_like);
        assert!(thread_feed_activity_is_visible(
            running.prominent,
            running.status,
            running.tool_like,
            Some(running.lifecycle_status)
        ));
        assert_eq!(recovered.summary, "Provider recovered");
        assert_eq!(recovered.status, Some(FeedStatus::Success));
        assert!(!recovered.tool_like);
        let failed = present(&failed_rows, running_run(None), BTreeSet::new());
        assert_eq!(kinds(&failed), ["activity-group", "activity-group"]);
        assert_eq!(failed[0].activities()[0].summary, failed_title);
    }

    #[rstest]
    fn omits_task_progress_without_hiding_adjacent_conversation_items(
        #[values("pending", "running", "completed")] step_status: &str,
    ) {
        let todo = base(
            todo_list("item-tasks", "plan-tasks"),
            "2026-06-20T00:00:02.500Z",
            2,
        );
        let plan = Plan {
            kind: PlanKind::Todo,
            steps: vec![PlanStep {
                text: "Verify the change".into(),
                status: step_status.into(),
            }],
            ..plan("plan-tasks", RUN)
        };
        let with_plan = |items| {
            build_thread_feed(&State {
                plans: vec![plan.clone()],
                ..thread(items)
            })
        };
        assert_eq!(
            with_plan(vec![user(), command_item(), todo, assistant().ordinal(3)]),
            with_plan(vec![user(), command_item(), assistant().ordinal(3)])
        );
    }

    #[test]
    fn hides_synthetic_workspace_preparation_activity() {
        let preparation = titled(
            base(
                command("item-command", WORKSPACE_PREPARATION_INPUT),
                "2026-06-20T00:00:02.000Z",
                0,
            ),
            "Workspace ready",
        )
        .text("Workspace preparation completed.");
        assert_eq!(feed(vec![preparation]), []);
    }

    #[test]
    fn does_not_treat_a_queued_only_run_as_live_feed_activity() {
        let run = |status, started: Option<&str>| FeedLatestRun {
            run: run_id(RUN),
            status,
            started_at: started.map(ts),
            completed_at: None,
        };
        assert!(!thread_feed_run_is_unsettled(Some(&run(
            RunStatus::Queued,
            None
        ))));
        for status in [RunStatus::Running, RunStatus::Completed, RunStatus::Waiting] {
            assert!(thread_feed_run_is_unsettled(Some(&run(
                status,
                Some("2026-06-20T00:00:01.000Z")
            ))));
        }
    }

    #[test]
    fn adds_queued_input_only_after_dispatch_creates_its_turn_item() {
        assert_eq!(feed(vec![]), []);
        let item = user_message("item-dispatched-queued", "message-dispatched-queued")
            .run("run-dispatched-queued")
            .text("Run checks");
        let state = State {
            messages: vec![agent_domain::Message {
                intent: InputIntent::TurnStart,
                ..stored_message("message-dispatched-queued", Role::User, "Run checks")
            }],
            ..thread(vec![item])
        };
        let rows = build_thread_feed(&state);
        assert_eq!(ids(&rows), ["message-dispatched-queued"]);
        assert_eq!(message(&rows[0]).input_intent, Some(InputIntent::TurnStart));
    }

    #[test]
    fn hides_the_interruption_request_and_keeps_the_terminal_result() {
        let request = base(
            run_interrupt_request("item-interrupt-request"),
            "2026-06-20T00:00:02.000Z",
            1,
        )
        .text("Interrupt requested");
        let result = base(
            run_interrupt_result("item-interrupt-result", "item-interrupt-request"),
            "2026-06-20T00:00:03.000Z",
            2,
        )
        .text("Run interrupted before provider start");
        let rows = feed(vec![request, result]);
        let activities = activities(&rows);
        assert_eq!(activities.len(), 1);
        assert_eq!(activities[0].summary, "Run interrupted");
        assert_eq!(
            activities[0].detail.as_deref(),
            Some("Run interrupted before provider start")
        );
        let presented = present(
            &rows,
            Some(latest(
                RunStatus::Interrupted,
                Some("2026-06-20T00:00:01.000Z"),
                Some("2026-06-20T00:00:03.000Z"),
            )),
            BTreeSet::new(),
        );
        assert!(!kinds(&presented).contains(&"run-fold"));
    }

    /// Activity ids are item ids: inherited items keep their own ids and the
    /// domain records no source thread to scope them with.
    #[test]
    fn preserves_authoritative_order_instead_of_sorting_reconstructed_collections() {
        let rows = feed(vec![
            user_message_at("2026-06-20T00:00:03.000Z"),
            command_at("2026-06-20T00:00:01.000Z"),
            assistant_at("2026-06-20T00:00:02.000Z"),
        ]);
        assert_eq!(kinds(&rows), ["message", "activity-group", "message"]);
        assert_eq!(
            ids(&rows),
            ["message-user", "item-command", "message-assistant"]
        );
        let activity = rows[1].activities()[0].clone();
        assert_eq!(activity.item.id.as_str(), "item-command");
        assert!(
            activity
                .full_detail
                .unwrap()
                .contains("\"command\": \"vp check\"")
        );
    }

    #[test]
    fn keeps_adjacent_work_from_different_attempts_in_separate_groups() {
        let attempt = |id: &str, ordinal| Attempt {
            id: RunAttemptId::new(id).unwrap(),
            run: run_id(RUN),
            ordinal,
            status: AttemptStatus::Completed,
            native_thread: None,
            native_turn: None,
            native_head: None,
            accepted: true,
            usage: None,
            context_usage: None,
            turn_usage: None,
            usage_accumulator: None,
            usage_observed: false,
            rejected_limits: Default::default(),
            started_at: ts("2026-06-20T00:00:01.000Z"),
            completed_at: None,
        };
        let first = Item {
            attempt: Some(RunAttemptId::new("attempt-1").unwrap()),
            ..command_item()
        };
        let second = Item {
            attempt: Some(RunAttemptId::new("attempt-2").unwrap()),
            ..with_id(command_at("2026-06-20T00:00:03.000Z"), "item-command-retry").ordinal(2)
        };
        let state = State {
            attempts: vec![attempt("attempt-1", 1), attempt("attempt-2", 2)],
            ..thread(vec![first, second])
        };
        let rows = build_thread_feed(&state);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .map(|row| row.activities()[0]
                    .attempt
                    .as_ref()
                    .map(RunAttemptId::as_str))
                .collect::<Vec<_>>(),
            [Some("attempt-1"), Some("attempt-2")]
        );
    }

    /// The domain has inherited and local items only; a fork card is local.
    #[test]
    fn retains_inherited_and_synthetic_rows_with_their_original_projected_identity() {
        let inherited = command_item().ordinal(0);
        let fork = base(
            fork("item-fork", "thread-source", 1),
            "2026-06-20T00:00:03.000Z",
            1,
        );
        let state = State {
            inherited_items: vec![inherited],
            ..thread(vec![fork])
        };
        let rows = build_thread_feed(&state);
        let activities = activities(&rows);
        assert_eq!(
            activities
                .iter()
                .map(|activity| (activity.item.id.as_str(), activity.visibility))
                .collect::<Vec<_>>(),
            [
                ("item-command", FeedVisibility::Inherited),
                ("item-fork", FeedVisibility::Local)
            ]
        );
        assert!(activities[1].prominent);
    }

    #[test]
    fn keeps_orchestration_relationship_cards_visible_when_a_completed_run_is_folded() {
        let rows = feed(vec![
            user(),
            command_item(),
            base(
                fork("item-fork", "thread-source", 1),
                "2026-06-20T00:00:02.500Z",
                2,
            ),
            assistant().ordinal(3),
        ]);
        let collapsed = present(&rows, completed_run(), BTreeSet::new());
        let has = |kind: ItemType| {
            activities(&collapsed)
                .iter()
                .any(|activity| ItemType::of(&activity.item.kind) == kind)
        };
        assert!(has(ItemType::Fork));
        assert!(!has(ItemType::CommandExecution));
    }

    #[test]
    fn keeps_opening_and_final_assistant_messages_around_the_first_hidden_work() {
        let opening = assistant_with(
            "item-opening",
            "message-opening",
            "I will check the deployment configuration.",
            "2026-06-20T00:00:01.500Z",
            1,
        );
        let middle = assistant_with(
            "item-middle",
            "message-middle",
            "The configuration is valid; checking the build next.",
            "2026-06-20T00:00:02.500Z",
            3,
        );
        let rows = feed(vec![
            user(),
            opening,
            command_item().ordinal(2),
            middle,
            assistant().ordinal(4),
        ]);
        let collapsed = present(&rows, completed_run(), BTreeSet::new());
        assert_eq!(
            ids(&collapsed),
            [
                "message-user",
                "message-opening",
                "run-fold:run-1",
                "message-assistant"
            ]
        );
        assert_eq!(
            message(&collapsed[1]).text,
            "I will check the deployment configuration."
        );
        let FeedRow::RunFold {
            created_at, label, ..
        } = &collapsed[2]
        else {
            panic!("expected a run fold");
        };
        assert_eq!(created_at, &ts("2026-06-20T00:00:02.000Z"));
        assert_eq!(label, "Worked for 2.0s");

        let expanded = present(&rows, completed_run(), runs(&[RUN]));
        assert_eq!(
            kinds(&expanded),
            [
                "message",
                "message",
                "run-fold",
                "work-toggle",
                "message",
                "message"
            ]
        );
        assert_eq!(message(&expanded[4]).id.as_str(), "message-middle");
        assert_eq!(
            message(&expanded[4]).text,
            "The configuration is valid; checking the build next."
        );
    }

    #[test]
    fn does_not_fold_a_response_that_only_has_opening_and_final_messages() {
        let rows = feed(vec![
            user(),
            assistant_with(
                "item-opening",
                "message-opening",
                "The result is ready.",
                "2026-06-20T00:00:02.000Z",
                1,
            ),
            assistant(),
        ]);
        assert_eq!(
            ids(&present(&rows, None, BTreeSet::new())),
            ["message-user", "message-opening", "message-assistant"]
        );
    }

    fn app_owned(task: Task) -> Task {
        Task {
            original_message: Some(MessageId::new(format!("message-{}", task.id)).unwrap()),
            ..task
        }
    }

    #[test]
    fn folds_subagents_while_keeping_created_thread_and_fork_cards_visible() {
        let mut child = app_owned(task(
            "child-agent",
            "thread-source",
            "Inspect the deployment configuration",
        ));
        child.result = Some("Configuration is valid".into());
        child.status = ItemStatus::Completed;
        let resources = [
            base(
                subagent("item-subagent", "child-agent"),
                "2026-06-20T00:00:01.500Z",
                1,
            ),
            base(
                fork("item-fork", "thread", 1),
                "2026-06-20T00:00:02.000Z",
                2,
            ),
            base(
                thread_created("item-created-thread", "thread-source", ""),
                "2026-06-20T00:00:04.000Z",
                4,
            ),
        ];
        let state = State {
            tasks: vec![child],
            ..thread(vec![
                user(),
                resources[0].clone(),
                resources[1].clone(),
                command_at("2026-06-20T00:00:03.000Z").ordinal(3),
                resources[2].clone(),
                assistant_at("2026-06-20T00:00:05.000Z").ordinal(5),
            ])
        };
        let rows = build_thread_feed(&state);
        let collapsed = present(&rows, None, BTreeSet::new());
        assert_eq!(
            kinds(&collapsed),
            [
                "message",
                "run-fold",
                "activity-group",
                "activity-group",
                "message"
            ]
        );
        assert_eq!(collapsed[1].created_at(), &ts("2026-06-20T00:00:01.500Z"));
        assert_eq!(
            activities(&collapsed)
                .iter()
                .map(|activity| activity.item.id.as_str())
                .collect::<Vec<_>>(),
            ["item-fork", "item-created-thread"]
        );
        let expanded = present(&rows, None, runs(&[RUN]));
        assert!(
            activities(&expanded)
                .iter()
                .any(|activity| matches!(activity.item.kind, ItemKind::Subagent { .. }))
        );
    }

    #[test]
    fn folds_settled_run_work_while_keeping_the_terminal_assistant_message_visible() {
        let rows = feed(vec![user(), command_item(), assistant()]);
        assert_eq!(
            kinds(&present(&rows, completed_run(), BTreeSet::new())),
            ["message", "run-fold", "message"]
        );
        assert_eq!(
            kinds(&present(&rows, completed_run(), runs(&[RUN]))),
            ["message", "run-fold", "work-toggle", "message"]
        );
    }

    #[test]
    fn keeps_an_active_run_expanded_and_detects_failures_from_completed_command_output() {
        let failed = command_item().text("sh: missing-command: command not found");
        let rows = feed(vec![user(), failed]);
        let presented = present(
            &rows,
            running_run(Some("2026-06-20T00:00:01.000Z")),
            BTreeSet::new(),
        );
        assert!(!kinds(&presented).contains(&"run-fold"));
        let toggle = toggle(&presented);
        assert_eq!(toggle.summary, "vp check");
        assert_eq!(toggle.hidden_count, 1);
        assert!(toggle.has_failure);
        assert!(!toggle.live);
    }

    fn shape(rows: &[FeedRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                FeedRow::RunFold { label, .. } => format!("fold:{label}"),
                FeedRow::Message { message, .. } => format!(
                    "{}:{}",
                    if message.role == Role::User {
                        "user"
                    } else {
                        "assistant"
                    },
                    message.id
                ),
                row => kind(row).into(),
            })
            .collect()
    }

    #[test]
    fn folds_each_run_of_a_provider_native_subagent_thread_like_a_normal_turn() {
        // A Claude subagent's child thread: no runs, and a user prompt for the
        // launch and for a resume.
        let prompt = |id: &str, at: &str| runless(user_with(id, id, at, 0));
        let answer = |id: &str, at: &str| runless(assistant_with(id, id, "Done", at, 0));
        let feed_for = |resume_running: bool| {
            let mut items = vec![
                prompt("launch", "2026-06-20T00:00:00.000Z"),
                runless(with_id(command_at("2026-06-20T00:00:04.000Z"), "launch-ls")),
                answer("launch-answer", "2026-06-20T00:00:08.000Z"),
                prompt("resume", "2026-06-20T00:01:12.000Z"),
            ];
            if resume_running {
                items.push(running(runless(
                    with_id(command_at("2026-06-20T00:01:17.000Z"), "resume-ls")
                        .text("")
                        .exit_code(None),
                )));
            } else {
                items.push(runless(with_id(
                    command_at("2026-06-20T00:01:17.000Z"),
                    "resume-ls",
                )));
                items.push(answer("resume-answer", "2026-06-20T00:01:20.000Z"));
            }
            feed(positioned(items))
        };
        let settled = present(&feed_for(false), None, BTreeSet::new());
        assert_eq!(
            shape(&settled),
            [
                "user:launch",
                "fold:Worked for 8.0s",
                "assistant:launch-answer",
                "user:resume",
                "fold:Worked for 8.0s",
                "assistant:resume-answer",
            ]
        );
        let Some(FeedRow::RunFold { run, .. }) = settled
            .iter()
            .find(|row| matches!(row, FeedRow::RunFold { .. }))
        else {
            panic!("expected the launch fold");
        };
        assert_eq!(
            shape(&present(
                &feed_for(false),
                None,
                BTreeSet::from([run.clone()])
            )),
            [
                "user:launch",
                "fold:Worked for 8.0s",
                "work-toggle",
                "assistant:launch-answer",
                "user:resume",
                "fold:Worked for 8.0s",
                "assistant:resume-answer",
            ]
        );
        // While the resume runs, only the settled launch folds.
        assert_eq!(
            shape(&present_working(
                &feed_for(true),
                None,
                BTreeSet::new(),
                BTreeSet::new(),
                Some("2026-06-20T00:01:12.000Z"),
                true,
            )),
            [
                "user:launch",
                "fold:Worked for 8.0s",
                "assistant:launch-answer",
                "user:resume",
                "work-toggle",
            ]
        );
    }

    /// Imported native sessions keep their runless turns.
    #[test]
    fn keeps_imported_turns_folded_once_the_threads_first_run_starts() {
        let imported = |item: Item, id: &str| runless(with_id(item, id));
        let presented = |start: Item| {
            let rows = feed(positioned(vec![
                imported(
                    user_message_at("2026-06-20T00:00:00.000Z"),
                    "imported-prompt",
                ),
                imported(
                    assistant_with("x", "update", "Done", "2026-06-20T00:00:02.000Z", 0),
                    "imported-update",
                ),
                imported(command_at("2026-06-20T00:00:04.000Z"), "imported-ls"),
                imported(
                    assistant_with("x", "answer", "Done", "2026-06-20T00:00:08.000Z", 0),
                    "imported-answer",
                ),
                start,
            ]));
            let presented = present_working(
                &rows,
                Some(latest(
                    RunStatus::Running,
                    Some("2026-06-20T00:01:00.000Z"),
                    None,
                )),
                BTreeSet::new(),
                BTreeSet::new(),
                Some("2026-06-20T00:01:00.000Z"),
                false,
            );
            presented[..4]
                .iter()
                .map(|row| match row {
                    FeedRow::Message { message, .. } if message.role == Role::User => "user",
                    FeedRow::Message { .. } => "assistant",
                    row => kind(row),
                })
                .collect::<Vec<_>>()
        };
        // A sent prompt and an automatic wake both start new work below the import.
        assert_eq!(
            presented(user_with(
                "new-prompt",
                "new-prompt",
                "2026-06-20T00:01:00.000Z",
                0
            )),
            ["user", "assistant", "run-fold", "assistant"]
        );
        let mut wake = base(
            notification("wake", "Background task finished"),
            "2026-06-20T00:01:00.000Z",
            4,
        );
        if let ItemKind::Notification { notification } = &mut wake.kind {
            notification.source = NotificationSource::Native(BackgroundKind::BackgroundTask);
        }
        assert_eq!(
            presented(wake),
            ["user", "assistant", "run-fold", "assistant"]
        );
    }

    #[test]
    fn keeps_a_provider_native_subagents_runless_tool_call_live_while_it_works() {
        let started_at = "2026-06-20T00:00:01.000Z";
        let running_command = running(runless(command_item().text("").exit_code(None)));
        let rows = feed(vec![runless(user()), running_command]);
        let presented = present_working(
            &rows,
            None,
            BTreeSet::new(),
            BTreeSet::new(),
            Some(started_at),
            true,
        );
        let toggle = toggle(&presented);
        assert_eq!(toggle.summary, "Running vp");
        assert!(toggle.live);
        assert!(toggle.shimmer);
        assert!(!kinds(&presented).contains(&"thinking"));
    }

    #[test]
    fn keeps_a_runless_tail_folded_while_a_normal_thread_waits_for_its_sent_run() {
        // Right after a send the local clock runs before the Host creates the
        // run, and the latest run may still be queued: neither is runless
        // work, so the settled tail must not reopen and shift the feed.
        let started_at = "2026-06-20T00:00:05.000Z";
        let rows = feed(vec![runless(user()), runless(command_item())]);
        for latest_run in [None, Some(latest(RunStatus::Queued, None, None))] {
            let presented = present_working(
                &rows,
                latest_run,
                BTreeSet::new(),
                BTreeSet::new(),
                Some(started_at),
                false,
            );
            assert_eq!(kinds(&presented), ["message", "run-fold", "thinking"]);
        }
    }

    #[test]
    fn waits_for_workspace_preparation_before_showing_provider_activity() {
        let started_at = "2026-04-01T00:00:01.000Z";
        let run = latest(RunStatus::Preparing, None, None);
        assert_eq!(
            present_working(
                &[],
                Some(run.clone()),
                BTreeSet::new(),
                BTreeSet::new(),
                Some(started_at),
                false
            ),
            []
        );
        assert_eq!(
            present_working(
                &[],
                Some(FeedLatestRun {
                    status: RunStatus::Running,
                    started_at: Some(ts(started_at)),
                    ..run
                }),
                BTreeSet::new(),
                BTreeSet::new(),
                Some(started_at),
                false
            ),
            [FeedRow::Thinking {
                id: "live-activity-row".into(),
                created_at: ts(started_at),
                run: Some(run_id(RUN)),
                continues_work_log: false,
            }]
        );
    }

    #[test]
    fn uses_a_stable_thinking_row_while_work_has_started_without_a_projected_item() {
        let started_at = "2026-04-01T00:00:01.000Z";
        let presented = || {
            present_working(
                &[],
                None,
                BTreeSet::new(),
                BTreeSet::new(),
                Some(started_at),
                false,
            )
        };
        assert_eq!(
            presented(),
            [FeedRow::Thinking {
                id: "live-activity-row".into(),
                created_at: ts(started_at),
                run: None,
                continues_work_log: false,
            }]
        );
        assert_eq!(presented(), presented());
    }

    fn constructed_activity(id: &str, created_at: &str, status: FeedStatus) -> FeedActivity {
        let lifecycle = if status == FeedStatus::Neutral {
            ToolLifecycleStatus::InProgress
        } else {
            ToolLifecycleStatus::Completed
        };
        let item = Arc::new(command_at(created_at));
        FeedActivity {
            id: id.into(),
            created_at: ts(created_at),
            run: None,
            attempt: None,
            summary: format!("Tool {id}"),
            detail: None,
            can_expand: false,
            fetches_detail: false,
            full_detail: None,
            copy_text: id.into(),
            icon: WorkIcon::Command,
            logo: None,
            tool_like: true,
            prominent: false,
            status: Some(status),
            lifecycle_status: lifecycle,
            work_entry: WorkLogEntry {
                command: Some("vp check".into()),
                item_type: Some(ItemType::CommandExecution),
                tool_lifecycle_status: Some(lifecycle),
                ..WorkLogEntry::new(id, ts(created_at), format!("Tool {id}"), WorkTone::Tool)
            },
            grouped_tool_detail: false,
            live: false,
            visibility: FeedVisibility::Local,
            item,
        }
    }

    #[test]
    fn keeps_expanded_work_in_one_group_with_stable_row_identities() {
        let rows = vec![FeedRow::ActivityGroup(ActivityGroup {
            id: "work-group-1".into(),
            created_at: ts("2026-04-01T00:00:01.000Z"),
            run: None,
            activities: vec![
                constructed_activity(
                    "activity-neutral",
                    "2026-04-01T00:00:01.000Z",
                    FeedStatus::Neutral,
                ),
                constructed_activity(
                    "activity-1",
                    "2026-04-01T00:00:02.000Z",
                    FeedStatus::Success,
                ),
                constructed_activity(
                    "activity-2",
                    "2026-04-01T00:00:03.000Z",
                    FeedStatus::Success,
                ),
                constructed_activity(
                    "activity-3",
                    "2026-04-01T00:00:04.000Z",
                    FeedStatus::Success,
                ),
            ],
            continues_work_log: false,
        })];
        let collapsed = present(&rows, None, BTreeSet::new());
        assert_eq!(ids(&collapsed), ["work-toggle:work-group:activity-neutral"]);
        let header = toggle(&collapsed);
        assert_eq!(header.group_id, "work-group:activity-neutral");
        assert_eq!(header.hidden_count, 3);
        assert!(!header.expanded);
        assert_eq!(header.summary, "Ran 3 commands");

        let expanded = present_working(
            &rows,
            None,
            BTreeSet::new(),
            groups(&["work-group:activity-neutral"]),
            None,
            false,
        );
        assert_eq!(
            ids(&expanded),
            [
                "work-toggle:work-group:activity-neutral",
                "work-details:work-group:activity-neutral",
            ]
        );
        assert!(toggle(&expanded).expanded);
        assert_eq!(
            expanded[1]
                .activities()
                .iter()
                .map(|activity| (
                    activity.id.as_str(),
                    activity.grouped_tool_detail,
                    activity.live
                ))
                .collect::<Vec<_>>(),
            [
                ("activity-1", true, false),
                ("activity-2", true, false),
                ("activity-3", true, false)
            ]
        );
    }

    #[test]
    fn retains_claude_read_image_previews_without_tool_output() {
        let item = base(
            dynamic_tool(
                "image-read",
                "Read",
                json!({ "file_path": "/workspace/reference.png" }),
                None,
            ),
            "2026-06-20T00:00:04.000Z",
            3,
        );
        let rows = feed(vec![item]);
        assert_eq!(
            rows[0].activities()[0]
                .work_entry
                .viewed_image_path
                .as_deref(),
            Some("/workspace/reference.png")
        );
    }

    #[test]
    fn pretty_prints_mcp_dynamic_tool_activities_and_attaches_the_product_logo() {
        let item = base(
            dynamic_tool(
                "item-orchestration-tool",
                "mcp__orchestration__thread_read",
                json!({ "threadId": "thread-child" }),
                Some(json!({ "messages": [] })),
            ),
            "2026-06-20T00:00:04.000Z",
            3,
        );
        let rows = feed(vec![item]);
        let activity = &rows[0].activities()[0];
        assert_eq!(activity.summary, "Read a thread");
        assert_eq!(activity.logo, Some(ToolLogo::App));
        assert_eq!(activity.copy_text.split('\n').next(), Some("Read a thread"));
    }

    #[test]
    fn uses_the_cua_action_title_in_the_mobile_feed() {
        let item = base(
            dynamic_tool(
                "cua",
                "cua_repl.js",
                json!({
                    "code": "await game.getAXStateAndScreenshot();",
                    "title": "Inspect Saga music screen",
                }),
                None,
            ),
            "2026-09-23T20:20:00.000Z",
            1,
        );
        let rows = feed(vec![item]);
        assert_eq!(rows[0].activities()[0].summary, "Inspect Saga music screen");
    }

    #[test]
    fn uses_canonical_orchestration_summaries_in_compact_work_groups() {
        let mut items = vec![command_at("2026-06-20T00:00:01.000Z").ordinal(0)];
        for (index, name) in [
            "mcp__orchestration__thread_send",
            "orchestration.thread_send",
            "thread_send",
        ]
        .into_iter()
        .enumerate()
        {
            items.push(base(
                dynamic_tool(
                    &format!("item-send-{index}"),
                    name,
                    json!({ "threadId": format!("thread-{index}"), "message": "Continue" }),
                    Some(json!({
                        "threadId": format!("thread-{index}"),
                        "messageId": format!("message-{index}"),
                    })),
                ),
                &format!("2026-06-20T00:00:0{}.000Z", index + 2),
                index as u64 + 1,
            ));
        }
        items.push(with_id(command_at("2026-06-20T00:00:06.000Z"), "item-command-2").ordinal(4));
        let presented = present(&feed(items), running_run(None), BTreeSet::new());
        assert_eq!(presented.len(), 1);
        let toggle = toggle(&presented);
        assert_eq!(
            toggle.summary,
            "Ran 2 commands and sent messages to 3 threads"
        );
        assert_eq!(toggle.hidden_count, 5);
        assert!(!toggle.has_failure);
    }
}

mod retained_feed_presentation {
    use super::*;

    #[test]
    fn retains_unchanged_rows_while_the_assistant_streams() {
        let latest_run = latest(RunStatus::Running, Some("2026-06-20T00:00:01.000Z"), None);
        let streaming = |text: &str, updated: &str| agent_domain::Message {
            streaming: true,
            updated_at: ts(updated),
            ..stored_message("message-assistant", Role::Assistant, text)
        };
        let rows_for = |message: agent_domain::Message, assistant: Item| {
            build_thread_feed(&State {
                messages: vec![message],
                ..thread(vec![user(), command_item(), assistant])
            })
        };
        let before = rows_for(streaming("Done", "2026-06-20T00:00:03.000Z"), assistant());
        let after = rows_for(
            streaming("Still working", "2026-06-20T00:00:04.000Z"),
            assistant_at("2026-06-20T00:00:04.000Z"),
        );
        let present = |rows: &[FeedRow]| {
            present_working(
                rows,
                Some(latest_run.clone()),
                BTreeSet::new(),
                BTreeSet::new(),
                Some("2026-06-20T00:00:01.000Z"),
                false,
            )
        };
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1], before[1]);
        assert_ne!(after[2], before[2]);
        let (before, after) = (present(&before), present(&after));
        assert_eq!(after[0], before[0]);
        assert_eq!(after[1], before[1]);
    }

    #[rstest]
    #[case::running(ItemStatus::Running, "Compacting context")]
    #[case::completed(ItemStatus::Completed, "Context compacted 899K → 19K tokens")]
    #[case::interrupted(ItemStatus::Interrupted, "Context compacted")]
    fn uses_the_compaction_row_as_the_live_activity_only_while(
        #[case] status: ItemStatus,
        #[case] summary: &str,
    ) {
        let after = (status == ItemStatus::Completed).then_some(19_000);
        let compact = base(
            compaction("compacted", Some(899_000), after),
            "2026-06-20T00:00:02.000Z",
            1,
        )
        .status(status);
        let rows = present_working(
            &feed(vec![user(), compact]),
            running_run(Some("2026-06-20T00:00:01.000Z")),
            BTreeSet::new(),
            BTreeSet::new(),
            Some("2026-06-20T00:00:01.000Z"),
            false,
        );
        assert_eq!(
            kinds(&rows).contains(&"thinking"),
            status != ItemStatus::Running
        );
        assert_eq!(activities(&rows)[0].summary, summary);
    }

    /// Handoffs are dividers placed before the run that received them.
    fn handoff_state(items: Vec<Item>) -> State {
        let previous = run("run-0", 1, RunStatus::Completed);
        let received = Run {
            selection: selection("claude"),
            ..run(RUN, 2, RunStatus::Completed)
        };
        let transfer = Transfer {
            native_source: None,
            instance: Some("claude".into()),
            target_run: None,
            delivery: Some(ContextDelivery {
                attempt: RunAttemptId::new("attempt").unwrap(),
                run: run_id(RUN),
                native_thread: None,
                status: ContextDeliveryStatus::Injected,
                item_ids: vec![],
                omitted_item_ids: vec![],
            }),
            id: ContextTransferId::new("handoff").unwrap(),
            kind: TransferKind::ProviderHandoff,
            source: thread_id(),
            target: thread_id(),
            boundary: 0,
            history: HistoricalContext {
                messages: vec![HistoricalMessage {
                    role: Role::User,
                    text: "Earlier".into(),
                    thread: "thread".into(),
                    run: Some("run-0".into()),
                    item: "earlier".into(),
                    provider_thread: None,
                    status: "completed".into(),
                    kind: "user_message".into(),
                    run_status: None,
                }],
                context: String::new(),
                omitted_items: 0,
                omitted_item_ids: vec![],
            },
            superseded: false,
        };
        State {
            runs: vec![previous, received],
            transfers: vec![transfer],
            ..thread(items)
        }
    }

    #[test]
    fn keeps_a_handoff_separate_from_commands_and_visible_through_folds() {
        let rows = build_thread_feed(&handoff_state(vec![
            user(),
            command_at("2026-06-20T00:00:03.000Z").ordinal(2),
            assistant_at("2026-06-20T00:00:04.000Z").ordinal(3),
        ]));
        for expanded in [BTreeSet::new(), runs(&[RUN])] {
            let presented = present(&rows, None, expanded);
            assert_eq!(
                presented
                    .iter()
                    .filter(|row| matches!(row, FeedRow::Handoff { divider, .. } if divider.run == run_id(RUN)))
                    .count(),
                1
            );
        }
        let alone = present(
            &build_thread_feed(&handoff_state(vec![user()])),
            None,
            BTreeSet::new(),
        );
        assert_eq!(kinds(&alone), ["handoff", "message"]);
    }

    #[test]
    fn keeps_a_standalone_compaction_visible_and_folds_it_with_other_completed_work() {
        let compact = base(
            compaction("compacted", None, None),
            "2026-06-20T00:00:02.000Z",
            1,
        )
        .text("Shorter context");
        let latest_run = Some(latest(
            RunStatus::Completed,
            Some("2026-06-20T00:00:01.000Z"),
            Some("2026-06-20T00:00:04.000Z"),
        ));
        let only = present(
            &feed(vec![user(), compact.clone()]),
            latest_run.clone(),
            BTreeSet::new(),
        );
        assert_eq!(kinds(&only), ["message", "activity-group"]);
        let rows = feed(vec![
            user(),
            compact,
            command_at("2026-06-20T00:00:03.000Z").ordinal(2),
            assistant_at("2026-06-20T00:00:04.000Z").ordinal(3),
        ]);
        assert_eq!(
            kinds(&present(&rows, latest_run.clone(), BTreeSet::new())),
            ["message", "run-fold", "message"]
        );
        let expanded = present(&rows, latest_run, runs(&[RUN]));
        let compaction = activities(&expanded)
            .into_iter()
            .find(|activity| matches!(activity.item.kind, ItemKind::Compaction { .. }))
            .expect("the compaction row");
        assert_eq!(compaction.summary, "Context compacted");
    }

    #[test]
    fn retains_assistant_image_attachments_from_the_wire() {
        let image = Attachment {
            kind: AttachmentKind::Image,
            source: None,
            id: "assistant-image".into(),
            name: "result.png".into(),
            mime_type: "image/png".into(),
            path: "result.png".into(),
            size: 100,
        };
        let state = State {
            messages: vec![agent_domain::Message {
                attachments: vec![image.clone()],
                ..stored_message("message-assistant", Role::Assistant, "")
            }],
            ..thread(vec![assistant().text("")])
        };
        let rows = build_thread_feed(&state);
        assert_eq!(rows.len(), 1);
        assert_eq!(message(&rows[0]).role, Role::Assistant);
        assert_eq!(message(&rows[0]).attachments, [image]);
    }

    #[test]
    fn keeps_native_application_icons_and_source_identity_in_collapsed_and_expanded_work() {
        let icon = json!({ "_tag": "native-app", "app": { "_tag": "app-id", "appId": "com.example.Editor" } });
        let source = json!({
            "key": "native-app:com.example.editor",
            "name": "Editor",
            "kind": "computer",
            "icon": icon,
        });
        let items = (0..2)
            .map(|index| {
                let mut item = base(
                    dynamic_tool(
                        &format!("native-{index}"),
                        "computer.click",
                        json!({ "x": index, "y": 1 }),
                        None,
                    ),
                    &format!("2026-06-20T00:00:0{}.000Z", index + 2),
                    index + 1,
                );
                if let ItemKind::DynamicTool { presentation, .. } = &mut item.kind {
                    presentation.surface = Some("computer".into());
                    presentation.icon = Some(Json(icon.clone()));
                    presentation.source = Some(Json(source.clone()));
                }
                item
            })
            .collect();
        let rows = feed(items);
        let collapsed = present(&rows, running_run(None), BTreeSet::new());
        let group = toggle(&collapsed).group_id.clone();
        let presented = present_working(
            &rows,
            running_run(None),
            BTreeSet::new(),
            groups(&[&group]),
            None,
            false,
        );
        let native_icon = ToolIcon::NativeApp(NativeApp::AppId("com.example.Editor".into()));
        let header = toggle(&presented);
        assert_eq!(header.summary, "Used Editor");
        assert_eq!(header.tool_surface, Some(ToolSurface::Computer));
        assert_eq!(header.tool_icon, Some(native_icon.clone()));
        let details = presented[1].activities();
        assert_eq!(details.len(), 2);
        for activity in details {
            assert_eq!(activity.icon, WorkIcon::Computer);
            assert_eq!(activity.work_entry.tool_icon, Some(native_icon.clone()));
            let source = activity.work_entry.tool_source.as_ref().unwrap();
            assert_eq!(
                (source.key.as_str(), source.name.as_str(), source.kind),
                (
                    "native-app:com.example.editor",
                    "Editor",
                    ToolSourceKind::Computer
                )
            );
            assert_eq!(source.icon, Some(native_icon.clone()));
        }
    }

    /// The reference uses the preview browser's click tool, which this product
    /// does not serve; an orchestration tool keeps the same lifecycle labels.
    #[rstest]
    #[case::failed(ItemStatus::Failed, "Failed to read a thread", true)]
    #[case::cancelled(ItemStatus::Cancelled, "Stopped reading a thread", false)]
    fn keeps_calls_terminal_while_the_parent_run_remains_live(
        #[case] status: ItemStatus,
        #[case] summary: &str,
        #[case] has_failure: bool,
    ) {
        let item = base(
            dynamic_tool(
                "thread-read",
                "mcp__orchestration__thread_read",
                json!({ "threadId": "thread-child" }),
                None,
            ),
            "2026-06-20T00:00:02.000Z",
            1,
        )
        .status(status)
        .completed(Some("2026-06-20T00:00:02.000Z"));
        let rows = present_working(
            &feed(vec![item]),
            running_run(Some("2026-06-20T00:00:01.000Z")),
            BTreeSet::new(),
            BTreeSet::new(),
            Some("2026-06-20T00:00:01.000Z"),
            false,
        );
        let FeedRow::WorkToggle(toggle) = &rows[0] else {
            panic!("expected a work toggle: {rows:?}");
        };
        assert_eq!(toggle.summary, summary);
        assert_eq!(toggle.has_failure, has_failure);
        assert!(!toggle.shimmer);
    }

    #[rstest]
    #[case::direct(json!({ "taskId": "a" }))]
    #[case::structured(json!({ "structuredContent": { "taskId": "a" } }))]
    #[case::text(json!({ "content": [{ "type": "text", "text": "{\"taskId\":\"a\"}" }] }))]
    fn folds_matched_delegations_without_hiding_pending_failed_or_unmatched_calls(
        #[case] output: serde_json::Value,
    ) {
        let agent =
            |id: &str, index: u64| base(subagent(id, id), "2026-06-20T00:00:01.000Z", index);
        let delegation = |id: &str, index: u64, output: Option<serde_json::Value>| {
            base(
                dynamic_tool(
                    id,
                    "orchestration.delegate_task",
                    json!({ "task": "Identical task" }),
                    output,
                ),
                "2026-06-20T00:00:02.000Z",
                index,
            )
        };
        let child = |id: &str, app: bool| {
            let mut task = task(id, &format!("child-{id}"), "Identical task");
            task.run = Some(run_id(RUN));
            task.result = Some("Done".into());
            task.status = ItemStatus::Completed;
            if app {
                task.original_message = Some(MessageId::new(format!("message-{id}")).unwrap());
            }
            task
        };
        let items = vec![
            agent("a", 1),
            delegation("matched", 2, Some(output.clone())),
            agent("b", 3),
            running(delegation("pending", 4, None)),
            delegation("unmatched", 5, Some(json!({ "taskId": "missing" }))),
            delegation("failed", 6, Some(output.clone())).status(ItemStatus::Failed),
            delegation(
                "error-output",
                7,
                Some(json!({ "taskId": "a", "isError": true })),
            ),
            delegation("other-run", 8, Some(output)).run("other-run"),
            agent("native", 9),
            delegation("native-delegation", 10, Some(json!({ "taskId": "native" }))),
        ];
        let state = State {
            tasks: vec![child("a", true), child("b", true), child("native", false)],
            ..thread(items)
        };
        let rows = build_thread_feed(&state);
        let groups = group_item_ids(&rows);
        assert_eq!(groups[0], ["a", "b"]);
        assert_eq!(
            groups.concat(),
            [
                "a",
                "b",
                "pending",
                "unmatched",
                "failed",
                "error-output",
                "other-run",
                "native",
                "native-delegation",
            ]
        );
        let presented = present(&rows, None, runs(&[RUN, "other-run"]));
        let card = presented
            .iter()
            .find(|row| {
                row.activities()
                    .first()
                    .is_some_and(|a| a.item.id.as_str() == "a")
            })
            .expect("the subagent card");
        assert!(!card.continues_work_log());
    }

    #[test]
    fn keeps_subagents_from_different_provider_turns_in_separate_cards() {
        let agent = |id: &str, index: u64| Item {
            attempt: Some(RunAttemptId::new(id).unwrap()),
            ..base(subagent(id, id), "2026-06-20T00:00:01.000Z", index)
        };
        assert_eq!(
            group_item_ids(&feed(vec![agent("a", 1), agent("b", 2)])),
            [vec!["a"], vec!["b"]]
        );
    }

    #[test]
    fn groups_only_adjacent_subagents_in_the_same_run_keeping_their_child_links() {
        let agent = |id: &str, index: u64, run: &str| {
            base(
                subagent(id, id),
                &format!("2026-06-20T00:00:0{index}.000Z"),
                index,
            )
            .run(run)
        };
        let child = |id: &str| {
            let mut task = task(id, &format!("child-{id}"), "Solve the puzzle");
            task.original_message = Some(MessageId::new(format!("message-{id}")).unwrap());
            task.result = Some("Done".into());
            task
        };
        let state = State {
            tasks: ["a", "b", "c", "d"].map(child).to_vec(),
            ..thread(vec![
                agent("a", 1, RUN),
                agent("b", 2, RUN),
                command_at("2026-06-20T00:00:03.000Z").ordinal(3),
                agent("c", 4, RUN),
                agent("d", 5, "other-run"),
            ])
        };
        let rows = build_thread_feed(&state);
        assert_eq!(
            group_item_ids(&rows),
            [vec!["a", "b"], vec!["item-command"], vec!["c"], vec!["d"]]
        );
        let presented = present(&rows, None, runs(&[RUN, "other-run"]));
        let cards: Vec<&FeedRow> = presented
            .iter()
            .filter(|row| {
                row.activities()
                    .first()
                    .is_some_and(|activity| matches!(activity.item.kind, ItemKind::Subagent { .. }))
            })
            .collect();
        assert_eq!(
            cards
                .iter()
                .map(|row| row.activities().len())
                .collect::<Vec<_>>(),
            [2, 1, 1]
        );
        let children: Vec<ThreadId> = cards[0]
            .activities()
            .iter()
            .map(|activity| {
                subagent_task(&state, &activity.item)
                    .unwrap()
                    .child_thread
                    .clone()
            })
            .collect();
        assert_eq!(
            children,
            [
                ThreadId::new("child-a").unwrap(),
                ThreadId::new("child-b").unwrap()
            ]
        );
    }
}

/// The answer lives on the request, so the attachment shows through the
/// question answer rather than the item JSON.
#[test]
fn makes_attachment_only_question_answers_expandable_in_the_mobile_feed() {
    let item = base(
        user_input_item("answer-history", "question-request"),
        "2026-09-08T00:00:00.000Z",
        0,
    );
    let file = Attachment {
        kind: AttachmentKind::File,
        source: None,
        id: "question-file".into(),
        name: "spec.txt".into(),
        mime_type: "text/plain".into(),
        path: "spec.txt".into(),
        size: 4,
    };
    let request = Request {
        answers: Some(BTreeMap::from([(
            "q".to_owned(),
            Answer::Text(String::new()),
        )])),
        attachments: BTreeMap::from([("q".to_owned(), vec![file.clone()])]),
        ..questions(
            "question-request",
            vec![Question {
                required: true,
                id: "q".into(),
                header: "Spec".into(),
                question: "Attach the specification".into(),
                multiple: false,
                options: vec![],
            }],
        )
    };
    let state = State {
        requests: vec![request],
        ..thread(vec![item])
    };
    let rows = build_thread_feed(&state);
    let [FeedRow::ActivityGroup(group)] = rows.as_slice() else {
        panic!("expected one activity group: {rows:?}");
    };
    let activity = &group.activities[0];
    assert!(activity.can_expand);
    let answer = activity.work_entry.question_answer.as_ref().unwrap();
    assert_eq!(answer.request.as_str(), "question-request");
    assert_eq!(answer.attachments, [("q".to_owned(), vec![file])]);
}

#[test]
fn renders_automatic_completion_as_a_neutral_activity_while_retaining_its_details() {
    let mut item = base(
        notification("notification", "Monitor reported an update"),
        "2026-06-20T00:00:01.000Z",
        0,
    );
    if let ItemKind::Notification { notification } = &mut item.kind {
        notification.source = NotificationSource::Native(BackgroundKind::Monitor);
        notification.outcome = NotificationOutcome::Updated;
        notification.detail = Some("Build checks changed".into());
    }
    let rows = feed(vec![item, command_item(), assistant()]);
    assert_eq!(kind(&rows[0]), "activity-group");
    let activity = &rows[0].activities()[0];
    assert_eq!(activity.summary, "Monitor reported an update");
    assert_eq!(activity.detail, None);
    assert_eq!(activity.status, None);
    assert!(
        activity
            .full_detail
            .as_deref()
            .unwrap()
            .contains("Build checks changed")
    );
    let presented = present(&rows, completed_run(), BTreeSet::new());
    assert!(
        activities(&presented)
            .iter()
            .any(|activity| activity.summary == "Monitor reported an update")
    );
    assert_eq!(kind(&feed(vec![user()])[0]), "message");
}

#[test]
fn uses_a_compact_reasoning_preview_and_a_short_expanded_heading() {
    let entry = WorkLogEntry {
        item_type: Some(ItemType::Reasoning),
        detail: Some("Check **ordering**.\nThen run the test.".into()),
        tool_lifecycle_status: Some(ToolLifecycleStatus::InProgress),
        ..WorkLogEntry::new(
            "thought",
            ts("2026-09-17T12:00:00Z"),
            "Thinking",
            WorkTone::Thinking,
        )
    };
    assert_eq!(
        work_entry_row_label(&entry, false),
        "Check **ordering**. Then run the test."
    );
    assert_eq!(work_entry_row_label(&entry, true), "Thinking");
    assert_eq!(
        work_entry_row_label(
            &WorkLogEntry {
                tool_lifecycle_status: Some(ToolLifecycleStatus::Completed),
                ..entry
            },
            true
        ),
        "Thought"
    );
}

#[test]
fn keeps_search_output_in_expanded_details_rather_than_the_compact_label() {
    let entry = WorkLogEntry {
        tool_title: Some("Grep".into()),
        item_type: Some(ItemType::DynamicTool),
        detail: Some("---\nfile body".into()),
        ..WorkLogEntry::new("search", ts("2026-09-17T12:00:00Z"), "Grep", WorkTone::Tool)
    };
    assert_eq!(work_entry_row_label(&entry, false), "Grep");
    assert_eq!(work_entry_row_label(&entry, true), "---\nfile body");
}

#[rstest]
#[case::paragraphs("First paragraph.\n\nSecond paragraph.")]
#[case::empty("")]
fn previews_live_reasoning_text(#[case] text: &str) {
    let thought = running(base(
        reasoning("live-thought", text),
        "2026-06-20T00:00:02.000Z",
        1,
    ));
    let rows = present_working(
        &feed(vec![user(), thought]),
        running_run(Some("2026-06-20T00:00:01.000Z")),
        BTreeSet::new(),
        BTreeSet::new(),
        Some("2026-06-20T00:00:01.000Z"),
        false,
    );
    if text.is_empty() {
        assert!(rows.iter().any(|row| match row {
            FeedRow::Thinking { .. } => true,
            FeedRow::WorkToggle(toggle) => toggle.summary == "Thinking",
            _ => false,
        }));
    } else {
        let toggle = toggle(&rows);
        assert_eq!(toggle.summary, "First paragraph. Second paragraph.");
        assert!(toggle.live);
    }
}

#[test]
fn stops_stranded_thinking_after_a_steer_and_follows_the_next_thought_or_tool() {
    let at = "2026-06-20T00:00:02.000Z";
    let thought = |id: &str| running(base(reasoning(id, id), at, 1));
    let first = thought("first-thought");
    let next = thought("next-thought");
    let steer = user_message_at(at);
    let tool = running(command_at(at).exit_code(None));
    let rows = |items: Vec<Item>, expanded: BTreeSet<String>| {
        present_working(
            &feed(positioned(items)),
            running_run(Some(at)),
            BTreeSet::new(),
            expanded,
            Some(at),
            false,
        )
    };
    let alone = rows(vec![first.clone()], BTreeSet::new());
    let header = toggle(&alone);
    assert_eq!(header.summary, "first-thought");
    assert!(header.live);
    assert!(header.shimmer);

    let after_steer = rows(vec![first.clone(), steer.clone()], BTreeSet::new());
    let header = toggle(&after_steer);
    assert!(!header.live);
    assert!(!header.shimmer);
    assert_eq!(kind(after_steer.last().unwrap()), "thinking");
    let expanded = rows(
        vec![first.clone(), steer.clone()],
        groups(&[&header.group_id]),
    );
    assert_eq!(toggle(&expanded).summary, "Thought");
    let detail = activities(&expanded)[0];
    assert_eq!(detail.lifecycle_status, ToolLifecycleStatus::Completed);
    assert_eq!(
        detail.work_entry.tool_lifecycle_status,
        Some(ToolLifecycleStatus::Completed)
    );
    for items in [
        vec![first.clone(), steer.clone(), next.clone()],
        vec![first.clone(), next.clone()],
        vec![first.clone(), steer, next, tool],
    ] {
        let expected = if matches!(items.last().unwrap().kind, ItemKind::Reasoning) {
            "next-thought"
        } else {
            "Running vp"
        };
        let presented = rows(items, BTreeSet::new());
        let live: Vec<&WorkToggle> = presented
            .iter()
            .filter_map(|row| match row {
                FeedRow::WorkToggle(toggle) if toggle.shimmer => Some(toggle),
                _ => None,
            })
            .collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].summary, expected);
    }
    assert_eq!(first.status, ItemStatus::Running);
}

#[test]
fn previews_a_settled_thought_in_its_collapsed_header_and_labels_its_expanded_header() {
    let text = "First paragraph.\n\nSecond paragraph.";
    let thought = base(
        reasoning("thought-preview", text),
        "2026-06-20T00:00:02.000Z",
        1,
    );
    let rows = feed(vec![user(), thought, assistant()]);
    let collapsed = present(&rows, completed_run(), runs(&[RUN]));
    let header = toggle(&collapsed);
    assert_eq!(header.summary, "First paragraph. Second paragraph.");
    let expanded = present_working(
        &rows,
        completed_run(),
        runs(&[RUN]),
        groups(&[&header.group_id]),
        None,
        false,
    );
    let header = toggle(&expanded);
    assert_eq!(header.summary, "Thought");
    assert!(header.continues_work_log);
    let detail = expanded
        .iter()
        .find(|row| matches!(row, FeedRow::ActivityGroup(_)))
        .unwrap();
    assert!(!detail.continues_work_log());
    assert_eq!(detail.activities()[0].detail.as_deref(), Some(text));
}

#[rstest]
#[case::provider_error("provider_error")]
#[case::usage_limit("usage_limit")]
fn keeps_a_historical_failure_and_preceding_work_visible_without_disclosures(#[case] class: &str) {
    let at = "2026-06-20T00:00:03.000Z";
    let message = "The provider stopped this turn.\nRetry later.";
    let mut error = base(provider_error("failure", class, message), at, 2)
        .status(ItemStatus::Failed)
        .completed(Some(at));
    if let ItemKind::Error { retryable, .. } = &mut error.kind {
        *retryable = Some(true);
    }
    let command = base(command("command", "pwd"), "2026-06-20T00:00:02.000Z", 1);
    let source = feed(vec![user(), command, error]);
    let newer = |status, completed: Option<&str>| FeedLatestRun {
        run: run_id("newer-run"),
        status,
        started_at: Some(ts(at)),
        completed_at: completed.map(ts),
    };
    let settled = present(
        &source,
        Some(newer(RunStatus::Completed, Some(at))),
        BTreeSet::new(),
    );
    let while_working = present(
        &source,
        Some(newer(RunStatus::Running, None)),
        BTreeSet::new(),
    );
    for row in &settled {
        assert_eq!(
            while_working
                .iter()
                .find(|candidate| candidate.id() == row.id()),
            Some(row)
        );
    }
    assert!(
        !kinds(&settled)
            .iter()
            .any(|kind| *kind == "run-fold" || *kind == "work-toggle")
    );
    let activities = activities(&settled);
    assert_eq!(
        activities
            .iter()
            .map(|activity| activity.item.id.as_str())
            .collect::<Vec<_>>(),
        ["command", "failure"]
    );
    let failure = activities.last().unwrap();
    assert_eq!(failure.detail.as_deref(), Some(message));
    assert_eq!(failure.created_at, ts(at));
    assert!(!failure.can_expand);
    assert!(failure.prominent);
}
