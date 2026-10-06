//! Ported from T3 `threadHistoryPaging.test.ts`; expectations are unchanged. T3
//! projected rows become visible items of a folded `State`.
use super::*;
use crate::store::tests::selection;
use agent_domain::{
    ApprovalOption, Fact, FactBody, InteractionMode, ItemStatus, PlanId, PlanKind, RequestBody,
    ResponseCapability, Role, RuntimeMode, Timestamp, TransferKind, apply,
};

pub(crate) fn at() -> Timestamp {
    Timestamp::parse("2026-06-20T00:00:00Z").unwrap()
}
pub(crate) fn thread_id() -> ThreadId {
    ThreadId::new("thread-1").unwrap()
}
pub(crate) fn fold_into(state: &mut State, body: FactBody) {
    apply(state, &Fact { at: at(), body }).unwrap();
}
pub(crate) fn created() -> State {
    let mut state = State::default();
    fold_into(
        &mut state,
        FactBody::ThreadCreated {
            id: thread_id(),
            project: "project-1".into(),
            title: "Thread".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            created_by: agent_domain::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    );
    state
}
pub(crate) fn item(id: &str, ordinal: u64, kind: ItemKind, text: String) -> Item {
    Item {
        id: TurnItemId::new(id).unwrap(),
        run: None,
        attempt: None,
        native_key: String::new(),
        ordinal,
        kind,
        status: ItemStatus::Completed,
        text,
        started_at: at(),
        completed_at: Some(at()),
    }
}
pub(crate) fn command_row(index: usize, output: String) -> Item {
    item(
        &format!("item-{index}"),
        index as u64 + 1,
        ItemKind::CommandExecution {
            command: format!("cmd-{index}"),
            cwd: None,
            exit_code: Some(0),
            title: None,
        },
        output,
    )
}
pub(crate) fn command_rows(count: usize) -> State {
    let mut state = created();
    state.items = (0..count)
        .map(|index| command_row(index, format!("output-{index}")))
        .collect();
    state
}
fn message(id: &str, run: Option<&str>, role: Role, author: MessageAuthor, text: &str) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: run.map(|run| RunId::new(run).unwrap()),
        role,
        text: text.into(),
        attachments: vec![],
        intent: InputIntent::TurnStart,
        streaming: false,
        created_by: author,
        creation_source: "provider".into(),
        created_at: at(),
        updated_at: at(),
    }
}
fn prompt(state: &mut State, index: usize, id: &str, author: MessageAuthor, text: &str) {
    let message = message(id, None, Role::User, author, text);
    state.items.push(item(
        &format!("item-{index}"),
        index as u64 + 1,
        ItemKind::UserMessage {
            message: message.id.clone(),
        },
        text.into(),
    ));
    state.messages.push(message);
}
fn interrupt(id: &str, ordinal: u64, result: bool) -> Item {
    let kind = if result {
        ItemKind::RunInterruptResult {
            request: TurnItemId::new("interrupt-req").unwrap(),
        }
    } else {
        ItemKind::RunInterruptRequest
    };
    let text = if result { "Stopped" } else { "Stopping" };
    item(id, ordinal, kind, text.into())
}
fn ids(page: &HistoryPage) -> Vec<String> {
    page.rows
        .iter()
        .map(|row| row.item.id.to_string())
        .collect()
}
fn visible_ids(state: &State) -> Vec<String> {
    state
        .visible_items()
        .iter()
        .map(|item| item.id.to_string())
        .collect()
}
fn sorted_item_ids(state: &State) -> Vec<String> {
    let mut ids: Vec<_> = state.items.iter().map(|item| item.id.to_string()).collect();
    ids.sort();
    ids
}
fn with_run(state: &mut State, id: &str, status: RunStatus) {
    fold_into(
        state,
        FactBody::RunRequested {
            id: RunId::new(id).unwrap(),
            message: MessageId::new(format!("message-{id}")).unwrap(),
            ordinal: state.runs.len() as u64 + 1,
            selection: selection(),
            status,
            queue_position: None,
            held: false,
            source_plan: None,
        },
    );
}

#[test]
fn builds_a_resumable_bounded_socket_snapshot_frame() {
    let state = std::sync::Arc::new(command_rows(90));
    let head = crate::ThreadHead {
        thread_seq: 90,
        input_seq: 90,
        global_seq: 23,
    };
    let snapshot = crate::ThreadSnapshot::build(&state, head, true);
    let window = snapshot.window.unwrap();
    assert_eq!(snapshot.snapshot_seq, 23);
    assert_eq!(snapshot.state.visible_items().len(), HISTORY_MAX_ITEMS);
    assert!(window.history_cursor.is_some());
    assert!(window.has_more_history);
    assert_eq!(window.latest_local_ordinal, Some(90));
    assert!(!window.payload_budget_exceeded);
}

#[test]
fn keeps_background_turns_with_user_turns_with_a_150_turn_fan_out_ceiling() {
    let mut state = created();
    for turn in 0..161 {
        let author = if turn == 0 {
            MessageAuthor::User
        } else {
            MessageAuthor::Agent
        };
        prompt(
            &mut state,
            turn * 2,
            &format!("prompt-{turn}"),
            author,
            &format!("Prompt {turn}"),
        );
        state.items.push(command_row(
            turn * 2 + 1,
            format!("output-{}", turn * 2 + 1),
        ));
    }
    let first = recent_history(&state, 1, PagePolicy::RECENT);
    assert_eq!(first.rows.len(), 300);
    assert_eq!(first.rows[0].item.id.as_str(), "item-22");
    let older = history_before(&state, first.next_cursor.as_ref().unwrap(), 1, None).unwrap();
    assert_eq!(older.rows.len(), 22);
    assert!(!older.has_more);
    assert_eq!([ids(&older), ids(&first)].concat(), visible_ids(&state),);
}

#[test]
fn pages_agent_only_child_transcripts_instead_of_dropping_their_earlier_activity() {
    let mut state = created();
    prompt(
        &mut state,
        0,
        "child-prompt",
        MessageAuthor::Agent,
        "Inspect this project",
    );
    state
        .items
        .extend((1..=90).map(|index| command_row(index, format!("output-{index}"))));
    let recent = recent_history(&state, 1, PagePolicy::RECENT);
    assert_eq!(recent.rows.len(), HISTORY_MAX_ITEMS);
    assert!(recent.has_more);
    let older = history_before(&state, recent.next_cursor.as_ref().unwrap(), 1, None).unwrap();
    assert!(matches!(
        older.rows[0].item.kind,
        ItemKind::UserMessage { .. }
    ));
    assert_eq!(older.rows.len() + recent.rows.len(), state.items.len());
}

#[test]
fn encodes_opaque_cursors_with_stable_source_identity() {
    let cursor =
        HistoryCursor::new(9, &thread_id(), &TurnItemId::new("item-3").unwrap(), 3).encode();
    assert!(!cursor.contains('{'));
    assert_eq!(
        HistoryCursor::decode(&cursor).unwrap(),
        HistoryCursor {
            v: 1,
            snapshot_seq: 9,
            source: "thread-1".into(),
            item: "item-3".into(),
            position: 3,
        }
    );
}

#[test]
fn rejects_malformed_cursors() {
    assert_eq!(HistoryCursor::decode("not-valid"), Err(InvalidCursor));
    assert_eq!(
        HistoryCursor::decode(&URL_SAFE_NO_PAD.encode("{}")),
        Err(InvalidCursor)
    );
}

#[test]
fn rejects_cursors_longer_than_the_maximum_length_before_decoding() {
    let oversized = "A".repeat(HISTORY_CURSOR_MAX_LEN + 1);
    assert_eq!(HistoryCursor::decode(&oversized), Err(InvalidCursor));
}

#[test]
fn selects_a_recent_window_by_item_count() {
    let state = command_rows(120);
    let page = recent_history(&state, 4, PagePolicy::budget(10, 10_000_000));
    assert_eq!(page.rows.len(), 10);
    assert_eq!(page.rows[0].item.id.as_str(), "item-110");
    assert_eq!(page.rows[9].item.id.as_str(), "item-119");
    assert!(page.has_more);
    let cursor = HistoryCursor::decode(page.next_cursor.as_ref().unwrap()).unwrap();
    assert_eq!(cursor.item, "item-110");
    assert_eq!(cursor.snapshot_seq, 4);
}

#[test]
fn always_includes_at_least_one_pathological_oversized_item() {
    let mut state = created();
    state.items = vec![
        command_row(0, "x".repeat(2_000_000)),
        command_row(1, "x".repeat(10)),
    ];
    let policy = PagePolicy::budget(50, 1_024);
    let page = recent_history(&state, 1, policy);
    assert_eq!(ids(&page), ["item-1"]);
    assert!(page.has_more);

    let older =
        history_before(&state, page.next_cursor.as_ref().unwrap(), 1, Some(policy)).unwrap();
    assert_eq!(ids(&older), ["item-0"]);
    assert!(!older.has_more);
    assert_eq!(older.next_cursor, None);
}

#[test]
fn recovers_identity_miss_cursors_when_position_is_past_a_shrunken_timeline() {
    let state = command_rows(5);
    let cursor = HistoryCursor::new(
        3,
        &thread_id(),
        &TurnItemId::new("item-deleted-long-ago").unwrap(),
        40,
    )
    .encode();
    let page = history_before(&state, &cursor, 3, Some(PagePolicy::budget(2, 10_000_000))).unwrap();
    assert_eq!(ids(&page), ["item-3", "item-4"]);
    assert!(page.has_more);
    assert!(page.next_cursor.is_some());
}

#[test]
fn keeps_cursor_resolution_stable_when_newer_items_append() {
    let policy = PagePolicy::budget(5, 10_000_000);
    let initial = command_rows(20);
    let recent = recent_history(&initial, 2, policy);
    let grown = command_rows(22);
    let older = history_before(
        &grown,
        recent.next_cursor.as_ref().unwrap(),
        5,
        Some(policy),
    )
    .unwrap();
    assert_eq!(
        ids(&older),
        ["item-10", "item-11", "item-12", "item-13", "item-14"]
    );
    assert!(older.has_more);
}

fn with_control_state(mut state: State) -> State {
    with_run(&mut state, "run-linked", RunStatus::Completed);
    let run = RunId::new("run-linked").unwrap();
    let attempt = RunAttemptId::new("attempt-linked").unwrap();
    fold_into(
        &mut state,
        FactBody::AttemptStarted {
            id: attempt.clone(),
            run: run.clone(),
            ordinal: 1,
        },
    );
    fold_into(
        &mut state,
        FactBody::RequestOpened {
            owner_path: vec![],
            id: agent_domain::RuntimeRequestId::new("req-1").unwrap(),
            attempt: attempt.clone(),
            native_key: "req-1".into(),
            body: RequestBody::Approval {
                kind: "command".into(),
                title: "Run".into(),
                detail: None,
                options: vec![ApprovalOption {
                    label: "Allow".into(),
                    decision: agent_domain::ApprovalDecision::Accept,
                }],
                input: agent_domain::Json(serde_json::json!({})),
            },
            capability: ResponseCapability::Live,
        },
    );
    fold_into(
        &mut state,
        FactBody::TaskStarted {
            original_message: None,
            background: false,
            id: agent_domain::NodeId::new("node-linked").unwrap(),
            native_key: "task".into(),
            run: Some(run.clone()),
            attempt: attempt.clone(),
            child: ThreadId::new("child-linked").unwrap(),
            parent: None,
            prompt: "Inspect".into(),
            model: None,
            wake: agent_domain::CompletionWake::Always,
        },
    );
    fold_into(
        &mut state,
        FactBody::CheckpointCaptured {
            status: agent_domain::CheckpointStatus::Ready,
            scope: None,
            id: agent_domain::CheckpointId::new("checkpoint-linked").unwrap(),
            run: Some(run),
            run_ordinal: 1,
            native_heads: Default::default(),
            file_ref: "refs/checkpoint".into(),
        },
    );
    state
}

#[test]
fn builds_a_bounded_projection_that_preserves_non_timeline_control_state() {
    let full = with_control_state(command_rows(40));
    let bounded = bounded_state(&full, 7, PagePolicy::budget(5, 10_000_000));
    assert_eq!(bounded.state.runs, full.runs);
    assert_eq!(bounded.state.requests, full.requests);
    assert_eq!(bounded.state.attempts, full.attempts);
    assert_eq!(bounded.state.visible_items().len(), 5);
    assert_eq!(bounded.state.items.len(), 5);
    assert!(bounded.has_more_history);
    assert!(bounded.history_cursor.is_some());
    assert_eq!(
        visible_ids(&bounded.state),
        ["item-35", "item-36", "item-37", "item-38", "item-39"]
    );
    // The watermark comes from the full projection, not the trimmed window.
    assert_eq!(bounded.latest_local_ordinal, Some(40));
}

#[test]
fn preserves_linked_control_rows_as_one_coherent_graph() {
    let linked = with_control_state(command_rows(40));
    let bounded = bounded_state(&linked, 7, PagePolicy::budget(5, 10_000_000));
    assert_eq!(bounded.state.runs, linked.runs);
    assert_eq!(bounded.state.attempts, linked.attempts);
    assert_eq!(bounded.state.tasks, linked.tasks);
    assert_eq!(bounded.state.checkpoints, linked.checkpoints);
    assert_eq!(bounded.state.thread, linked.thread);
}

#[test]
fn carries_a_full_projection_watermark_when_the_bounded_window_is_inherited_only() {
    let mut full = command_rows(2);
    full.thread.as_mut().unwrap().parent = Some(ThreadId::new("parent-thread").unwrap());
    full.inherited_items = vec![command_row(2, "inherited".into())];
    let bounded = bounded_state(&full, 2, PagePolicy::budget(1, 10_000_000));
    assert_eq!(visible_ids(&bounded.state), ["item-2"]);
    assert_eq!(bounded.state.inherited_items.len(), 1);
    assert!(bounded.state.items.is_empty());
    assert_eq!(bounded.latest_local_ordinal, Some(2));
    assert_eq!(
        bounded_state(&created(), 0, PagePolicy::RECENT).latest_local_ordinal,
        None
    );
}

/// A forked thread whose inherited history is one turn steered `steers` times.
fn forked_steered_turn(steers: usize) -> State {
    let mut source = created();
    prompt(
        &mut source,
        0,
        "prompt",
        MessageAuthor::User,
        "Original prompt",
    );
    for index in 1..=steers {
        prompt(
            &mut source,
            index,
            &format!("steer-{index}"),
            MessageAuthor::User,
            &format!("Steer {index}"),
        );
        source.messages.last_mut().unwrap().intent = InputIntent::Steer;
    }
    let mut fork = created();
    fold_into(
        &mut fork,
        FactBody::ForkAccepted {
            parent: ThreadId::new("parent-thread").unwrap(),
            boundary: 1,
            history: source.items,
            messages: source.messages,
        },
    );
    fork
}

#[test]
fn keeps_an_inherited_steered_turn_whole_using_the_inherited_intents() {
    let fork = forked_steered_turn(10);
    let page = recent_history(&fork, 1, PagePolicy::RECENT);
    assert_eq!(page.rows.len(), 11);
    assert!(!page.has_more);
    assert_eq!(page.rows[0].item.id.as_str(), "item-0");
    assert!(page.rows.iter().all(|row| row.inherited));
    assert_eq!(
        page.rows[1].message.as_ref().map(|m| m.intent),
        Some(InputIntent::Steer)
    );

    let one_turn = PagePolicy {
        max_user_turns: Some(1),
        ..PagePolicy::RECENT
    };
    assert_eq!(recent_history(&fork, 1, one_turn).rows.len(), 11);
}

#[test]
fn bounds_inherited_messages_to_the_inherited_rows_in_the_window() {
    let mut fork = created();
    let mut source = created();
    for turn in 0..15 {
        prompt(
            &mut source,
            turn,
            &format!("prompt-{turn}"),
            MessageAuthor::User,
            &format!("Prompt {turn}"),
        );
    }
    fold_into(
        &mut fork,
        FactBody::ForkAccepted {
            parent: ThreadId::new("parent-thread").unwrap(),
            boundary: 15,
            history: source.items,
            messages: source.messages,
        },
    );
    let bounded = bounded_state(&fork, 1, PagePolicy::RECENT);
    assert_eq!(bounded.state.inherited_items.len(), HISTORY_MAX_USER_TURNS);
    let kept: Vec<_> = bounded
        .state
        .inherited_messages
        .iter()
        .map(|m| m.id.to_string())
        .collect();
    assert_eq!(
        kept,
        (5..15)
            .map(|turn| format!("prompt-{turn}"))
            .collect::<Vec<_>>()
    );
    assert!(bounded.has_more_history);
}

#[test]
fn retains_an_out_of_window_interrupt_request_needed_by_a_visible_result() {
    let mut full = created();
    full.items = vec![
        interrupt("interrupt-req", 1, false),
        command_row(1, "filler".into()),
        interrupt("interrupt-res", 3, true),
    ];
    let bounded = bounded_state(&full, 3, PagePolicy::budget(1, 10_000_000));
    assert_eq!(
        sorted_item_ids(&bounded.state),
        ["interrupt-req", "interrupt-res"]
    );
    assert!(bounded.has_more_history);
    let cursor = HistoryCursor::decode(bounded.history_cursor.as_ref().unwrap()).unwrap();
    assert_eq!(cursor.item, "interrupt-res");
}

#[test]
fn retains_all_interrupt_requests_even_when_no_result_is_in_the_initial_window() {
    let mut full = created();
    full.items = vec![
        interrupt("interrupt-req", 1, false),
        interrupt("interrupt-res", 2, true),
        command_row(2, "recent".into()),
    ];
    let bounded = bounded_state(&full, 4, PagePolicy::budget(1, 10_000_000));
    assert_eq!(sorted_item_ids(&bounded.state), ["interrupt-req", "item-2"]);
    assert!(
        !bounded
            .state
            .items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::RunInterruptResult { .. }))
    );
}

/// T3 charges a local row twice because its item is duplicated into `turnItems`.
/// Here an item appears once, so the snapshot charges each row with its message and
/// the control state, which history pages do not carry.
#[test]
fn charges_control_state_and_messages_when_measuring_bounded_timeline_bytes() {
    let mut full = created();
    full.items = (0..8)
        .map(|index| command_row(index, "x".repeat(2_000)))
        .collect();
    let max_encoded_bytes = 12_000;
    let policy = PagePolicy::budget(50, max_encoded_bytes);
    let bounded = bounded_state(&full, 1, policy);
    let contribution: u64 = bounded.state.items.iter().map(json_len).sum();
    assert!(contribution <= max_encoded_bytes);
    assert!(!bounded.state.items.is_empty());
    assert!(bounded.state.items.len() < full.items.len());
    assert!(json_len(&bounded.state) <= max_encoded_bytes);

    let page = recent_history(&full, 1, policy);
    let page_bytes: u64 = page.rows.iter().map(json_len).sum();
    assert!(page_bytes <= max_encoded_bytes);
    assert!(page.rows.len() > bounded.state.items.len());

    let mut prompted = created();
    prompt(&mut prompted, 0, "prompt", MessageAuthor::User, "hello");
    let rows = timeline(&prompted);
    assert!(snapshot_row_bytes(&rows[0], 0) > json_len(rows[0].item));
}

#[test]
fn allows_a_single_pathological_bounded_row_to_exceed_the_configured_cap() {
    let mut full = created();
    full.items = vec![command_row(0, "x".repeat(50_000))];
    let max_encoded_bytes = 1_000;
    let bounded = bounded_state(&full, 1, PagePolicy::budget(10, max_encoded_bytes));
    assert_eq!(bounded.state.visible_items().len(), 1);
    assert!(json_len(&bounded.state.items[0]) > max_encoded_bytes);
    assert!(bounded.payload_budget_exceeded);
}

#[test]
fn exposes_conservative_default_page_budgets() {
    const { assert!(PagePolicy::RECENT.max_items >= 50) };
    const { assert!(PagePolicy::RECENT.max_items <= 100) };
    assert_eq!(PagePolicy::RECENT.max_encoded_bytes, 1_048_576);
}

#[test]
fn does_not_carry_the_full_duplicated_message_table_into_a_bounded_snapshot() {
    let mut full = command_rows(120);
    full.messages = (0..2_000)
        .map(|index| {
            message(
                &format!("message-{index}"),
                Some(&format!("old-run-{index}")),
                Role::Assistant,
                MessageAuthor::Agent,
                &"x".repeat(1_000),
            )
        })
        .collect();
    let bounded = bounded_state(&full, 9, PagePolicy::RECENT);
    assert!(bounded.state.messages.is_empty());
    assert!(json_len(&bounded.state) < HISTORY_MAX_ENCODED_BYTES + 100_000);
}

/// T3 uses 2 MiB per plan and handoff; 20 KiB still exceeds the budget many times over.
#[test]
fn omits_historical_control_details_that_remain_available_from_history_items() {
    let mut populated = command_rows(200);
    let body = 20_480;
    populated.plans = (0..120)
        .map(|index| Plan {
            kind: PlanKind::Proposed,
            id: PlanId::new(format!("plan-{index}")).unwrap(),
            run: RunId::new("run-1").unwrap(),
            native_key: format!("plan-{index}"),
            markdown: "p".repeat(body),
            steps: vec![],
            implemented_by: None,
        })
        .collect();
    populated.transfers = (0..120)
        .map(|index| agent_domain::Transfer {
            native_source: None,
            instance: Some("codex".into()),
            target_run: None,
            delivery: None,
            id: agent_domain::ContextTransferId::new(format!("handoff-{index}")).unwrap(),
            kind: TransferKind::ProviderHandoff,
            source: thread_id(),
            target: thread_id(),
            boundary: 1,
            history: agent_domain::HistoricalContext {
                messages: vec![],
                context: "h".repeat(body),
                omitted_items: 0,
                omitted_item_ids: vec![],
            },
            superseded: true,
        })
        .collect();
    assert!(json_len(&populated) > 4 * HISTORY_MAX_ENCODED_BYTES);

    let bounded = bounded_state(&populated, 9, PagePolicy::RECENT);
    assert!(json_len(&bounded.state) <= HISTORY_MAX_ENCODED_BYTES);
    assert!(!bounded.payload_budget_exceeded);
    assert!(
        bounded
            .state
            .plans
            .iter()
            .all(|plan| plan.markdown.is_empty())
    );
    assert!(
        bounded
            .state
            .transfers
            .iter()
            .all(|transfer| transfer.history.context.is_empty())
    );
    assert_eq!(populated.plans.len(), 120);
    assert_eq!(populated.plans[0].markdown.len(), body);
}

#[test]
fn preserves_oversized_actionable_state_and_reports_the_budget_exception() {
    let mut actionable = created();
    with_run(&mut actionable, "run-active", RunStatus::Running);
    actionable.plans = vec![Plan {
        kind: PlanKind::Proposed,
        id: PlanId::new("plan-actionable").unwrap(),
        run: RunId::new("run-active").unwrap(),
        native_key: "plan".into(),
        markdown: "a".repeat(2_097_152),
        steps: vec![],
        implemented_by: None,
    }];
    let bounded = bounded_state(&actionable, 9, PagePolicy::RECENT);
    assert!(bounded.payload_budget_exceeded);
    assert_eq!(bounded.state.plans[0], actionable.plans[0]);
}

#[test]
fn keeps_paged_historical_plan_detail_in_the_turn_item_and_only_status_in_its_artifact() {
    let detail = "Implement the historical plan exactly.\n".repeat(1_000);
    let mut full = created();
    let plan = PlanId::new("plan-historical-visible").unwrap();
    full.items = vec![item(
        "item-0",
        1,
        ItemKind::ProposedPlan { plan: plan.clone() },
        detail.clone(),
    )];
    full.plans = vec![Plan {
        kind: PlanKind::Proposed,
        id: plan,
        run: RunId::new("run-done").unwrap(),
        native_key: "plan".into(),
        markdown: detail.clone(),
        steps: vec![],
        implemented_by: None,
    }];
    let bounded = bounded_state(&full, 9, PagePolicy::RECENT);
    assert_eq!(bounded.state.visible_items()[0].text, detail);
    assert_eq!(bounded.state.plans[0].markdown, "");
    assert_eq!(bounded.state.plans[0].id, full.plans[0].id);
    assert!(!bounded.payload_budget_exceeded);
}

mod fold_after_bounded_snapshot;
