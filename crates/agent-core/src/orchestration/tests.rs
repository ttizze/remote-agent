use super::*;
use model::*;
use serde_json::json;

const NOW: &str = "2026-11-14T00:00:00.000Z";

fn projection() -> ThreadProjection {
    ThreadProjection {
        thread:serde_json::from_value(json!({
            "id":"thread","createdBy":"user","creationSource":"mobile","projectId":null,
            "title":"Example","providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"example"},
            "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,
            "activeProviderThreadId":null,"lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"thread"},
            "forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,"archivedAt":null,"settledOverride":null,"settledAt":null,"lastVisitedAt":null,"deletedAt":null
        })).unwrap(),
        runs:vec![],attempts:vec![],nodes:vec![],subagents:vec![],provider_sessions:vec![],provider_threads:vec![],
        provider_turns:vec![],runtime_requests:vec![],plans:vec![],turn_items:vec![],checkpoint_scopes:vec![],
        checkpoints:vec![],context_handoffs:vec![],context_transfers:vec![],visible_turn_items:vec![],updated_at:NOW.into(),
    }
}

fn run(status: &str) -> Run {
    serde_json::from_value(json!({
        "id":"run","threadId":"thread","ordinal":1,"providerInstanceId":"codex",
        "modelSelection":{"instanceId":"codex","model":"example"},"providerThreadId":null,
        "userMessageId":"input","rootNodeId":null,"activeAttemptId":null,"status":status,
        "queuePosition":null,"queueHeld":false,"requestedAt":NOW,"startedAt":null,"completedAt":null,
        "checkpointId":null,"contextHandoffId":null
    })).unwrap()
}

fn item(id: &str, ordinal: u64) -> TurnItem {
    serde_json::from_value(json!({
        "id":id,"threadId":"thread","runId":"run","nodeId":null,"providerThreadId":null,
        "providerTurnId":null,"nativeItemRef":null,"parentItemId":null,"ordinal":ordinal,
        "status":"completed","title":null,"startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,
        "type":"assistant_message","messageId":id,"text":id,"attachments":[],"streaming":false
    }))
    .unwrap()
}

fn event(change: Change) -> Event {
    Event {
        id: "event".into(),
        thread_id: "thread".into(),
        run_id: None,
        node_id: None,
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: NOW.into(),
        change,
    }
}

fn apply(projection: &ThreadProjection, change: Change) -> ThreadProjection {
    apply_event(projection, &event(change), ApplyOptions::default())
}

fn row(item: TurnItem, visibility: Visibility, position: u64) -> ProjectedTurnItem {
    ProjectedTurnItem {
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        position,
        visibility,
        item: Arc::new(item),
    }
}

#[test]
fn canonical_subagents_keep_public_child_links_models_and_status_without_opening_the_child() {
    use agent_protocol::{execution, items, session};
    let owner = session::SessionRef::new(None, "thread".into()).unwrap();
    let child =
        session::SessionRef::new(None, "105a6224-6aa3-4618-9fdc-d916232bfce5".into()).unwrap();
    let mut snapshot = snapshot(1);
    snapshot.has_more_history = false;
    snapshot.history_cursor = None;
    let task:Subagent=serde_json::from_value(json!({
        "id":"real-task","threadId":"thread","runId":null,"parentNodeId":"real-native-root",
        "origin":"provider_native","createdBy":"agent","driver":"codex","providerInstanceId":"codex",
        "providerThreadId":"child-native-provider","childThreadId":child.application_id(),
        "nativeTaskRef":{"driver":"codex","nativeId":"real-spawn:private-native-child","strength":"strong"},
        "title":"Review","model":"requested-child-model","status":"completed","startedAt":NOW,"completedAt":NOW,"updatedAt":NOW
    })).unwrap();
    let mut value = item("real-task-item", 1);
    value.run_id = None;
    value.node_id = Some(task.id.clone());
    value.body = TurnItemBody::Subagent {
        subagent_id: task.id.clone(),
        origin: task.origin.clone(),
        driver: task.driver.clone(),
        provider_instance_id: task.provider_instance_id.clone(),
        child_thread_id: Some(child.application_id()),
        prompt: "Review the requested behavior".into(),
        result: Some("review completed".into()),
        progress: None,
    };
    snapshot.projection.subagents.push(task);
    snapshot.projection.turn_items.push(Arc::new(value.clone()));
    snapshot
        .projection
        .visible_turn_items
        .push(row(value, Visibility::Local, 0));
    let shown = project(
        &snapshot,
        &owner,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    let item = &shown.thread.turns.as_ref().unwrap()[0]
        .items
        .as_ref()
        .unwrap()[0];
    let items::ItemBody::Subagent {
        receivers,
        states,
        model,
        agent_id,
        ..
    } = item.body()
    else {
        panic!("canonical task presentation");
    };
    assert_eq!(receivers, &[child.clone()]);
    assert_eq!(states[0].session, child);
    assert_eq!(states[0].status, execution::TurnStatus::Completed);
    assert_eq!(states[0].message.as_deref(), Some("review completed"));
    assert_eq!(model.as_deref(), Some("requested-child-model"));
    assert!(
        agent_id.is_none(),
        "an app conversation ID is not a native agent ID"
    );
    let mut state = crate::state::Snapshot::default();
    Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
        Some(owner.clone());
    Arc::make_mut(&mut state.sessions.conversations).insert(owner.clone(), Arc::new(shown.thread));
    Arc::make_mut(&mut state.sessions.projections)
        .insert(owner.clone(), Arc::new(snapshot.clone()));
    let agents = state.agent_panel(state.panes.primary());
    assert_eq!(agents.len(), 1);
    assert!(!agents[0].active);
    assert_eq!(agents[0].model.as_deref(), Some("requested-child-model"));
    assert_eq!(agents[0].detail.as_deref(), Some("review completed"));
    // Task metadata is authoritative even when the disposable conversation DTO
    // hasn't changed and the child itself has never been loaded.
    snapshot.projection.subagents[0].model = Some("newly-selected-child-model".into());
    Arc::make_mut(&mut state.sessions.projections).insert(owner.clone(), Arc::new(snapshot));
    assert_eq!(
        state.agent_panel(state.panes.primary())[0].model.as_deref(),
        Some("newly-selected-child-model")
    );
    assert!(!state.sessions.conversations.contains_key(&child));
    Arc::make_mut(&mut state.sessions.conversations).insert(
        child.clone(),
        Arc::new(agent_protocol::models::Thread {
            id: Some(child),
            parent_id: Some(owner),
            name: Some("User's reviewer".into()),
            ..Default::default()
        }),
    );
    assert_eq!(
        state.agent_panel(state.panes.primary())[0].title,
        "User's reviewer"
    );
}

#[test]
fn native_display_preserves_inherited_groups_without_creating_application_runs() {
    use agent_protocol::{execution, items, session};
    let identity =
        session::SessionRef::new(session::ProviderKind::Codex, "native-thread".into()).unwrap();
    let mut projection = projection();
    projection.thread.id = identity.application_id();
    projection.runs.push(run("running"));
    let mut inherited = item("inherited", 9_000_001);
    inherited.thread_id = "parent".into();
    inherited.run_id = Some("parent-run".into());
    let mut local = item("local", 1_000_001);
    local.body = TurnItemBody::AssistantMessage {
        message_id: "local".into(),
        text: "answer".into(),
        phase: Some("final_answer".into()),
        attachments: vec![],
        streaming: true,
    };
    projection.visible_turn_items = vec![
        row(inherited, Visibility::Inherited, 0),
        row(local, Visibility::Local, 1),
    ];
    let snapshot = ThreadSnapshot {
        snapshot_sequence: 3,
        projection,
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: Some(1_000_001),
    };
    let displayed = project(
        &snapshot,
        &identity,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        Some("/workspace"),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(displayed.thread.id, Some(identity));
    assert_eq!(displayed.thread.cwd.as_deref(), Some("/workspace"));
    let turns = displayed.thread.turns.unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].id.as_str(), "parent-run");
    assert_eq!(turns[0].status, execution::TurnStatus::Completed);
    assert_eq!(turns[1].id.as_str(), "run");
    assert_eq!(turns[1].status, execution::TurnStatus::Running);
    assert!(matches!(
        turns[1].items.as_ref().unwrap()[0].body(),
        items::ItemBody::AssistantText {
            phase: items::AssistantPhase::Final,
            ..
        }
    ));
    assert_eq!(snapshot.projection.runs.len(), 1);
}

#[test]
fn fork_actions_use_the_visible_rows_real_owner_and_explicit_boundary() {
    use agent_protocol::{conversation::records::ForkPoint, session::SessionRef};
    let owner = SessionRef::new(None, "d0ef657a-9331-425b-902d-fcc00a413a2c".into()).unwrap();
    for visibility in [Visibility::Local, Visibility::Inherited] {
        let mut projection = projection();
        let mut value = item("answer", 1);
        value.thread_id = owner.application_id();
        projection.visible_turn_items = vec![row(value.clone(), visibility, 0)];
        assert_eq!(
            fork_point(&projection, &"run".into()),
            Some((
                owner.clone(),
                ForkPoint::Run {
                    run_id: "run".into()
                }
            ))
        );
        value.run_id = None;
        value.provider_turn_id = Some("real-provider-turn".into());
        projection.visible_turn_items = vec![row(value.clone(), visibility, 0)];
        assert_eq!(
            fork_point(&projection, &"real-provider-turn".into()),
            Some((
                owner.clone(),
                ForkPoint::ProviderTurn {
                    provider_turn_id: "real-provider-turn".into()
                }
            ))
        );
        value.provider_turn_id = None;
        projection.thread.history_origin = Some(ThreadHistoryOrigin::V1Import);
        let mut last = value.clone();
        last.id = "last-reasoning".into();
        last.ordinal = 2;
        projection.visible_turn_items = vec![row(value, visibility, 0), row(last, visibility, 1)];
        assert_eq!(
            fork_point(
                &projection,
                &format!("history:{}:answer", owner.application_id()).into()
            ),
            Some((
                owner.clone(),
                ForkPoint::TurnItem {
                    turn_item_id: "last-reasoning".into()
                }
            ))
        );
        assert!(fork_point(&projection, &"missing".into()).is_none());
        projection.visible_turn_items[0].visibility = Visibility::Synthetic;
        assert!(
            fork_point(
                &projection,
                &format!("history:{}:answer", owner.application_id()).into()
            )
            .is_none()
        );
    }
}

#[test]
fn inherited_success_waits_for_the_root_checkpoint_and_ignores_later_background_results() {
    let mut snapshot = snapshot(1);
    let mut answer = item("answer", 1);
    answer.thread_id = "parent".into();
    answer.provider_turn_id = Some("native-turn".into());
    let mut checkpoint = answer.clone();
    checkpoint.id = "root-checkpoint".into();
    checkpoint.ordinal = 2;
    checkpoint.completed_at = Some("1970-01-01T00:00:03.000Z".into());
    checkpoint.body = TurnItemBody::Checkpoint {
        checkpoint_id: "actual-checkpoint".into(),
        scope_id: "root-scope".into(),
        files: vec![],
        files_total: None,
    };
    let mut late = answer.clone();
    late.id = "late-tool".into();
    late.ordinal = 3;
    late.completed_at = Some("1970-01-01T00:00:09.000Z".into());
    snapshot.projection.provider_turns.push(
        serde_json::from_value(json!({
            "id":"native-turn","providerThreadId":"source-native","nodeId":"source-root",
            "runAttemptId":"actual-source-attempt","nativeThreadRef":null,
            "nativeTurnRef":{"driver":"codex","nativeId":"raw-turn","strength":"strong"},
            "ordinal":1,"status":"completed","startedAt":"1970-01-01T00:00:01.000Z",
            "completedAt":"1970-01-01T00:00:02.000Z"
        }))
        .unwrap(),
    );
    snapshot.projection.visible_turn_items = vec![
        row(answer, Visibility::Inherited, 0),
        row(checkpoint, Visibility::Inherited, 1),
        row(late, Visibility::Inherited, 2),
    ];
    let view = project(
        &snapshot,
        &agent_protocol::session::SessionRef::new(None, "thread".into()).unwrap(),
        agent_protocol::session::ProviderKind::Codex,
        Default::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    let turns = view.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].started_at_ms, Some(1000));
    assert_eq!(turns[0].completed_at_ms, Some(3000));
    assert!(snapshot.projection.runs.is_empty());
    assert!(snapshot.projection.attempts.is_empty());
}

#[test]
fn a_queued_input_is_displayed_once_in_the_queue_until_it_starts() {
    use agent_protocol::{items, session};
    let identity = session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let mut projection = projection();
    projection.thread.id = identity.application_id();
    projection.runs.push(run("queued"));
    let mut user = item("user", 0);
    user.status = TurnItemStatus::Pending;
    user.body = TurnItemBody::UserMessage {
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        message_id: "input".into(),
        input_intent: "queued_turn".into(),
        text: "next".into(),
        attachments: vec![],
        context: None,
    };
    projection.turn_items.push(Arc::new(user));
    let snapshot = ThreadSnapshot {
        snapshot_sequence: 1,
        projection,
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: None,
    };
    let view = project(
        &snapshot,
        &identity,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(view.thread.turns.unwrap().is_empty());
    assert_eq!(view.thread.queue.inputs.len(), 1);
    let activated = receive(
        &snapshot.projection.thread.id,
        Some(&snapshot),
        ThreadStreamItem::Event {
            sequence: 2,
            event: Event {
                thread_id: identity.application_id(),
                ..event(Change::Run(run("running")))
            },
        },
    )
    .unwrap()
    .unwrap();
    let view = project(
        &activated,
        &identity,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    assert!(view.thread.queue.inputs.is_empty());
    let turns = view.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].queued);
    assert!(
        matches!(turns[0].items.as_ref().unwrap()[0].body(),items::ItemBody::UserMessage {text:Some(text),..} if text=="next")
    );
}

#[test]
fn an_uncertain_steer_is_answerable_as_its_own_input_without_replacing_the_running_turn() {
    use agent_protocol::session;
    let identity = session::SessionRef::new(None, "thread".into()).unwrap();
    let mut projection = projection();
    projection.runs.push(run("running"));
    let mut user = item("steer-item", 1_000_002);
    user.body = TurnItemBody::UserMessage {
        message_id: "steer-input".into(),
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        input_intent: "steer".into(),
        text: "extra".into(),
        attachments: vec![],
        context: None,
    };
    let mut error = item("delivery-error", 1_000_003);
    error.parent_item_id = Some(user.id.clone());
    error.status = TurnItemStatus::Failed;
    error.body = TurnItemBody::Error {
        failure: ProviderFailure {
            class: "delivery_uncertain".into(),
            message: "ack lost".into(),
            code: None,
            retryable: Some(false),
            reset_at: None,
        },
        retry: None,
    };
    projection.visible_turn_items = vec![
        row(user.clone(), Visibility::Local, 0),
        row(error.clone(), Visibility::Local, 1),
    ];
    projection.turn_items = vec![Arc::new(user), Arc::new(error)];
    let mut snapshot = ThreadSnapshot {
        snapshot_sequence: 1,
        projection,
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: None,
    };
    let display = |snapshot: &ThreadSnapshot| {
        project(
            snapshot,
            &identity,
            session::ProviderKind::Codex,
            session::Capabilities::default(),
            None,
            &BTreeMap::new(),
        )
        .unwrap()
    };
    let view = display(&snapshot);
    assert_eq!(view.thread.preparations.len(), 1);
    assert_eq!(
        view.thread.preparations[0]
            .submission
            .client_user_message_id
            .as_str(),
        "steer-input"
    );
    assert_eq!(
        view.thread.preparations[0].status,
        session::PreparationStatus::Unknown
    );
    assert_eq!(
        view.thread.submissions["steer-input"],
        session::SubmissionDelivery::Unknown
    );
    assert_eq!(snapshot.projection.runs[0].status, RunStatus::Running);
    assert!(
        view.thread.turns.as_ref().unwrap()[0].error.is_none(),
        "unknown input has its own recovery control; the running turn has not failed"
    );
    let mut cancelled = snapshot.projection.turn_items[1].as_ref().clone();
    cancelled.status = TurnItemStatus::Cancelled;
    snapshot.projection = apply(&snapshot.projection, Change::TurnItem(cancelled));
    let view = display(&snapshot);
    assert!(view.thread.preparations.is_empty());
    assert!(!view.thread.submissions.contains_key("steer-input"));
    assert!(view.thread.turns.as_ref().unwrap()[0].error.is_none());
}

#[test]
fn imported_messages_group_at_user_inputs_without_inventing_application_runs() {
    use agent_protocol::session;
    let identity = session::SessionRef::new(None, "thread".into()).unwrap();
    let mut projection = projection();
    for index in 0..4 {
        let mut item = item(&format!("history-{index}"), index + 1);
        item.run_id = None;
        if index % 2 == 0 {
            item.body = TurnItemBody::UserMessage {
                message_id: format!("user-{index}"),
                created_by: Actor::User,
                creation_source: CreationSource::Server,
                input_intent: "turn_start".into(),
                text: format!("question {index}"),
                attachments: vec![],
                context: None,
            };
        }
        projection
            .visible_turn_items
            .push(row(item.clone(), Visibility::Local, index));
        projection.turn_items.push(Arc::new(item));
    }
    let snapshot = ThreadSnapshot {
        snapshot_sequence: 1,
        projection,
        history_cursor: None,
        has_more_history: false,
        latest_local_turn_ordinal: None,
    };
    let view = project(
        &snapshot,
        &identity,
        session::ProviderKind::Codex,
        session::Capabilities::default(),
        None,
        &BTreeMap::new(),
    )
    .unwrap();
    let turns = view.thread.turns.unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].items.as_ref().unwrap().len(), 2);
    assert_eq!(turns[1].items.as_ref().unwrap().len(), 2);
    assert!(snapshot.projection.runs.is_empty());
    assert!(snapshot.projection.attempts.is_empty());
}

#[test]
fn fork_history_keeps_source_identity_and_orders_local_items_by_their_own_ordinal() {
    let mut original = projection();
    let mut inherited = item("parent-item", 9_000_001);
    inherited.thread_id = "parent".into();
    original
        .visible_turn_items
        .push(row(inherited, Visibility::Inherited, 0));
    let next = apply(&original, Change::TurnItem(item("later", 1_000_002)));
    let next = apply(&next, Change::TurnItem(item("earlier", 1_000_001)));
    assert_eq!(
        next.visible_turn_items
            .iter()
            .map(|row| (
                row.source_thread_id.as_str(),
                row.source_item_id.as_str(),
                row.position
            ))
            .collect::<Vec<_>>(),
        [
            ("parent", "parent-item", 0),
            ("thread", "earlier", 1),
            ("thread", "later", 2)
        ]
    );
    let mut parent_event = event(Change::TurnItem(item("parent-item", 9_000_001)));
    parent_event.thread_id = "parent".into();
    assert_eq!(
        apply_event(&next, &parent_event, ApplyOptions::default()),
        next
    );
    assert_eq!(original.visible_turn_items.len(), 1);
}

#[test]
fn queued_input_is_hidden_until_activation_regardless_of_event_order_and_stays_hidden_if_cancelled()
{
    let mut initial = projection();
    initial.runs.push(run("queued"));
    let mut queued = item("input", 1_000_001);
    queued.body = TurnItemBody::UserMessage {
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        message_id: "input".into(),
        input_intent: "queued_turn".into(),
        text: "hello".into(),
        attachments: vec![],
        context: None,
    };
    let queued_first = apply(&initial, Change::TurnItem(queued.clone()));
    assert!(queued_first.visible_turn_items.is_empty());
    let activated = apply(&queued_first, Change::Run(run("starting")));
    assert_eq!(activated.visible_turn_items.len(), 1);
    let run_first = apply(&initial, Change::Run(run("starting")));
    let item_second = apply(&run_first, Change::TurnItem(queued));
    assert_eq!(item_second, activated);
    assert!(
        apply(&queued_first, Change::Run(run("cancelled")))
            .visible_turn_items
            .is_empty()
    );
}

#[test]
fn a_partial_timeline_drops_unloaded_old_updates_but_accepts_queued_runs_below_the_watermark() {
    let mut initial = projection();
    initial.runs.push(run("completed"));
    let initial = apply(&initial, Change::TurnItem(item("newest", 2_000_001)));
    let options = ApplyOptions {
        partial_timeline: true,
        latest_local_turn_ordinal: Some(2_000_001),
    };
    assert_eq!(
        apply_event(
            &initial,
            &event(Change::TurnItem(item("old", 1_000_001))),
            options
        ),
        initial
    );
    let queued = apply(&initial, Change::Run(run("queued")));
    let incoming = apply_event(
        &queued,
        &event(Change::TurnItem(item("queued-low", 1_000_001))),
        options,
    );
    assert_eq!(incoming.turn_items.len(), 2);
    assert_eq!(incoming.visible_turn_items.len(), 2);
    let mut native = item("native-old", 1);
    native.run_id = None;
    assert_eq!(
        apply_event(&initial, &event(Change::TurnItem(native)), options),
        initial
    );
}

#[test]
fn delayed_history_preserves_live_bodies_and_does_not_revive_rolled_back_items() {
    let initial = apply(&projection(), Change::TurnItem(item("live", 2)));
    let mut stale = item("live", 2);
    stale.body = TurnItemBody::SystemNotice {
        message: "stale".into(),
    };
    let older = [
        row(item("old", 1), Visibility::Local, 0),
        row(stale, Visibility::Local, 1),
    ];
    let merged = merge_older_history(&initial, &older);
    assert_eq!(merged.visible_turn_items.len(), 2);
    assert_eq!(
        merged.visible_turn_items[1].item,
        initial.visible_turn_items[0].item
    );
    let rolled_back = apply(&initial, Change::Run(run("rolled_back")));
    let merged = merge_older_history(&rolled_back, &older[1..]);
    assert!(merged.visible_turn_items.is_empty());
    assert_eq!(merged.turn_items.len(), 1);
}

#[test]
fn an_older_page_deduplicates_by_both_source_thread_and_item() {
    let mut parent = item("same", 1);
    parent.thread_id = "parent".into();
    let rows = [
        row(parent, Visibility::Inherited, 0),
        row(item("same", 1), Visibility::Local, 1),
    ];
    let merged = merge_older_history(&projection(), &rows);
    assert_eq!(merged.visible_turn_items.len(), 2);
    assert_eq!(merged.turn_items.len(), 1);
    assert_eq!(merge_older_history(&merged, &rows), merged);
}

#[test]
fn superseded_interrupt_results_require_a_matching_stop_request() {
    let mut initial = projection();
    initial.attempts.push(serde_json::from_value(json!({
        "id":"attempt","runId":"run","attemptOrdinal":1,"reason":"steering_restart","rootNodeId":"root",
        "providerInstanceId":"codex","providerThreadId":"native","providerTurnId":null,"status":"superseded",
        "startedAt":NOW,"completedAt":NOW,"error":null
    })).unwrap());
    let mut result = item("result", 2);
    result.node_id = Some("root".into());
    result.body = TurnItemBody::RunInterruptResult {
        message: "stopped".into(),
    };
    let hidden = apply(&initial, Change::TurnItem(result));
    assert!(hidden.visible_turn_items.is_empty());
    let mut request = item("request", 1);
    request.body = TurnItemBody::RunInterruptRequest {
        message: "stop".into(),
    };
    // Like T3, request updates recheck existing rows; the result's next event restores it.
    let with_request = apply(&hidden, Change::TurnItem(request));
    let restored = apply(
        &with_request,
        Change::TurnItem(hidden.turn_items[0].as_ref().clone()),
    );
    assert_eq!(restored.visible_turn_items.len(), 2);
}

fn snapshot(sequence: u64) -> ThreadSnapshot {
    ThreadSnapshot {
        snapshot_sequence: sequence,
        projection: projection(),
        history_cursor: Some("cursor".into()),
        has_more_history: true,
        latest_local_turn_ordinal: None,
    }
}

#[test]
fn the_client_resumes_own_events_without_reapplying_overlap_and_accepts_a_replacement_snapshot() {
    let original = snapshot(3);
    let updated = receive(
        "thread",
        Some(&original),
        ThreadStreamItem::Event {
            sequence: 9,
            event: event(Change::TurnItem(item("new", 100))),
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(updated.snapshot_sequence, 9);
    assert_eq!(updated.latest_local_turn_ordinal, Some(100));
    assert_eq!(
        receive(
            "thread",
            Some(&updated),
            ThreadStreamItem::Event {
                sequence: 8,
                event: event(Change::TurnItem(item("old", 99)))
            }
        )
        .unwrap()
        .unwrap(),
        updated
    );
    assert_eq!(
        receive("thread", Some(&updated), ThreadStreamItem::Synchronized)
            .unwrap()
            .unwrap(),
        updated
    );
    let reset = snapshot(0);
    assert_eq!(
        receive(
            "thread",
            Some(&updated),
            ThreadStreamItem::Snapshot(reset.clone())
        )
        .unwrap(),
        Some(reset)
    );
}

#[test]
fn a_missing_snapshot_or_wrong_thread_cannot_poison_the_resume_cursor() {
    let frame = ThreadStreamItem::Event {
        sequence: 4,
        event: event(Change::Thread(projection().thread)),
    };
    assert!(receive("thread", None, frame.clone()).is_err());
    assert!(receive("another", Some(&snapshot(3)), frame).is_err());
    assert!(receive("another", None, ThreadStreamItem::Snapshot(snapshot(3))).is_err());
}

#[test]
fn history_responses_are_cursor_fenced_and_do_not_acknowledge_live_events() {
    let mut current = snapshot(3);
    current.latest_local_turn_ordinal = Some(100);
    let page = ThreadHistoryPage {
        snapshot_sequence: 20,
        items: vec![row(item("old", 1), Visibility::Local, 0)],
        next_cursor: None,
        has_more_history: false,
    };
    assert!(receive_history(&current, "obsolete-cursor", page.clone()).is_none());
    let merged = receive_history(&current, "cursor", page).unwrap();
    assert_eq!(merged.snapshot_sequence, 3);
    assert_eq!(merged.latest_local_turn_ordinal, Some(100));
    assert_eq!(merged.projection.visible_turn_items.len(), 1);
    assert_eq!(merged.history_cursor, None);
    assert!(!merged.has_more_history);
}

#[test]
fn snapshot_frames_keep_t3s_flat_wire_contract() {
    let frame = ThreadStreamItem::Snapshot(snapshot(3));
    let json = serde_json::to_value(&frame).unwrap();
    assert_eq!(json["kind"], "snapshot");
    assert_eq!(json["snapshotSequence"], 3);
    assert_eq!(json["projection"]["thread"]["id"], "thread");
    assert_eq!(
        serde_json::from_value::<ThreadStreamItem>(json).unwrap(),
        frame
    );
}

#[test]
fn content_events_update_the_thread_clock_without_rewriting_explicit_metadata_or_foreign_threads() {
    let before = projection();
    let later = "2026-11-14T01:00:00.000Z";
    let mut observation = event(Change::RunCreated(run("running")));
    observation.occurred_at = later.into();
    let current = apply_event(&before, &observation, ApplyOptions::default());
    assert_eq!(current.updated_at, later);
    assert_eq!(current.thread.updated_at, later);
    let mut explicit = current.thread.clone();
    explicit.updated_at = NOW.into();
    let metadata = Event {
        id: "metadata-clock".into(),
        change: Change::Thread(explicit),
        ..observation.clone()
    };
    assert_eq!(
        apply_event(&current, &metadata, ApplyOptions::default())
            .thread
            .updated_at,
        NOW
    );
    observation.thread_id = "foreign".into();
    assert_eq!(
        apply_event(&current, &observation, ApplyOptions::default()),
        current
    );
}

#[rstest::rstest]
#[case("eligible", true)]
#[case("paged", false)]
#[case("no-checkpoint", false)]
#[case("no-session", false)]
#[case("wrong-scope", false)]
#[case("inherited", false)]
#[case("imported", false)]
#[case("stale-checkpoint", false)]
#[case("retained-native", true)]
#[case("retained-rebound-physical", false)]
#[case("retained-latest-foreign", false)]
#[test]
fn canonical_rewind_actions_require_a_real_local_run_and_restorable_boundary(
    #[case] condition: &str,
    #[case] expected: bool,
) {
    use agent_protocol::session::{Capabilities, ProviderKind, SessionRef};
    let mut snapshot = snapshot(3);
    snapshot.has_more_history = condition == "paged";
    snapshot.history_cursor = condition.eq("paged").then(|| "older".into());
    let projection = &mut snapshot.projection;
    projection.thread.active_provider_thread_id = Some("native".into());
    let mut run = run("completed");
    run.root_node_id = Some("root".into());
    run.provider_thread_id = Some("native".into());
    projection.runs.push(run);
    projection.nodes.push(serde_json::from_value(json!({"id":"root","threadId":"thread","rootNodeId":"root","runId":"run","kind":"root_turn","status":"completed","countsForRun":true,"checkpointScopeId":"scope"})).unwrap());
    projection.provider_threads.push(serde_json::from_value(json!({"id":"native","driver":"codex","providerInstanceId":"codex","providerSessionId":if condition=="no-session" {None} else {Some("sdk")},"appThreadId":"thread","nativeThreadRef":{"driver":"codex","nativeId":"physical","strength":"strong"},"status":"idle","handoffIds":[],"createdAt":NOW,"updatedAt":NOW})).unwrap());
    let mut caps = ProviderCapabilities::default();
    caps.threads.can_rollback_thread = true;
    caps.checkpointing.provider_can_rollback_conversation = true;
    caps.checkpointing.provider_rollback_returns_snapshot = true;
    projection.provider_sessions.push(ProviderSession {
        id: "sdk".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        status: ProviderSessionStatus::Ready,
        cwd: "/fixture".into(),
        model: None,
        capabilities: caps,
        created_at: NOW.into(),
        updated_at: NOW.into(),
        last_error: None,
    });
    projection.checkpoint_scopes.push(serde_json::from_value(json!({"id":"scope","threadId":"thread","nodeId":"root","kind":"root_turn","ordinalWithinParent":0,"advancesAppRunCount":true,"cwd":"/fixture","createdAt":NOW})).unwrap());
    if condition != "no-checkpoint" {
        projection.checkpoints.push(serde_json::from_value(json!({"id":"checkpoint","threadId":"thread","scopeId":if condition=="wrong-scope" {"foreign-scope"} else {"scope"},"nodeId":"root","ordinalWithinScope":0,"appRunOrdinal":0,"ref":"conversation-only","status":if condition=="stale-checkpoint" {"stale"} else {"missing"},"files":[],"capturedAt":NOW})).unwrap());
    }
    let mut input = item("input", 1);
    input.body = TurnItemBody::UserMessage {
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        message_id: "input".into(),
        input_intent: "submit".into(),
        text: "original".into(),
        attachments: vec![],
        context: None,
    };
    if condition == "imported" {
        input.run_id = None;
    }
    projection.turn_items.push(Arc::new(input.clone()));
    projection.visible_turn_items.push(row(
        input,
        if condition == "inherited" {
            Visibility::Inherited
        } else {
            Visibility::Local
        },
        1,
    ));
    if condition.starts_with("retained-") {
        let mut next = projection.runs[0].clone();
        next.id = "next-run".into();
        next.ordinal = 2;
        next.active_attempt_id = Some("next-attempt".into());
        projection.runs[0].active_attempt_id = Some("first-attempt".into());
        projection.runs.push(next);
        projection.checkpoints[0].app_run_ordinal = Some(1);
        let visible = &mut projection.visible_turn_items[0];
        Arc::make_mut(&mut visible.item).run_id = Some("next-run".into());
        Arc::make_mut(&mut projection.turn_items[0]).run_id = Some("next-run".into());
        let native_ref = json!({"driver":"codex","nativeId":"physical","strength":"strong"});
        for (attempt, run) in [("first-attempt", "run"), ("next-attempt", "next-run")] {
            projection.attempts.push(serde_json::from_value(json!({"id":attempt,"runId":run,"attemptOrdinal":1,"rootNodeId":"root","providerInstanceId":"codex","providerThreadId":"native","reason":"initial","status":"completed","nativeThreadRef":native_ref})).unwrap());
        }
        let turn:ProviderTurn=serde_json::from_value(json!({"id":"first-native-turn","providerThreadId":"native","nodeId":"root","runAttemptId":"first-attempt","nativeThreadRef":native_ref,"nativeTurnRef":{"driver":"codex","nativeId":"first-native-handle","strength":"strong"},"ordinal":1,"status":"completed"})).unwrap();
        projection.provider_turns.push(turn.clone());
        if condition == "retained-rebound-physical" {
            projection.provider_turns[0].native_thread_ref = Some(ProviderRef {
                driver: "codex".into(),
                native_id: Some("previous-physical".into()),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            });
        } else if condition == "retained-latest-foreign" {
            let mut latest = turn;
            latest.id = "later-native-turn".into();
            latest.ordinal = 2;
            latest.native_thread_ref = Some(ProviderRef {
                driver: "codex".into(),
                native_id: Some("previous-physical".into()),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            });
            projection.provider_turns.push(latest);
        }
    }
    let view = project(
        &snapshot,
        &SessionRef::new(None, "thread".into()).unwrap(),
        ProviderKind::Codex,
        Capabilities {
            rollback: true,
            fork: true,
            ..Default::default()
        },
        None,
        &Default::default(),
    )
    .unwrap();
    let rendered = crate::presentation::conversation::project_conversation(
        Default::default(),
        &Default::default(),
        &Default::default(),
        Arc::new(view.thread),
        &None,
    );
    assert_eq!(
        rendered.turns[0]
            .rows
            .iter()
            .any(|row| row.edit_turn_id.is_some()),
        expected
    );
}
