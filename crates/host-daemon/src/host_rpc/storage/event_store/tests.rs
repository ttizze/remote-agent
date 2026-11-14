use super::*;
use serde_json::json;

const NOW: &str = "2026-11-14T00:00:00.000Z";

fn database() -> Connection {
    let mut db = super::super::test_support::database();
    let thread = super::super::test_support::thread("thread");
    let tx = db.transaction().unwrap();
    projections::save(&tx, None, &thread).unwrap();
    for (id, ordinal, status) in [("first", 1, "running"), ("second", 2, "queued")] {
        let run: model::Run = serde_json::from_value(json!({
            "id":id,"threadId":"thread","ordinal":ordinal,"providerInstanceId":"codex",
            "modelSelection":{"instanceId":"codex","model":"example"},"providerThreadId":null,
            "userMessageId":id,"rootNodeId":null,"activeAttemptId":null,"status":status,
            "queuePosition":null,"queueHeld":false,"requestedAt":NOW,"startedAt":null,"completedAt":null,
            "checkpointId":null,"contextHandoffId":null
        })).unwrap();
        projections::save(&tx, Some("thread"), &run).unwrap();
    }
    tx.commit().unwrap();
    db
}

fn item(id: &str, run: &str) -> model::TurnItem {
    serde_json::from_value(json!({
        "id":id,"threadId":"thread","runId":run,"nodeId":null,"providerThreadId":null,
        "providerTurnId":null,"nativeItemRef":null,"parentItemId":null,"ordinal":79,
        "status":"completed","title":null,"startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,
        "type":"assistant_message","messageId":id,"text":"body","attachments":[],"streaming":false
    }))
    .unwrap()
}

fn event(id: &str, item: model::TurnItem) -> Event {
    Event {
        id: id.into(),
        thread_id: "thread".into(),
        run_id: item.run_id.clone(),
        node_id: None,
        driver: None,
        provider_instance_id: None,
        raw_event_id: None,
        occurred_at: NOW.into(),
        change: model::Change::TurnItem(item),
    }
}

#[test]
fn item_positions_are_stable_and_identical_in_projection_and_replayed_events() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let first = append(&tx, &event("one", item("one", "first")), None, Some(1)).unwrap();
    let second = append(&tx, &event("two", item("two", "first")), None, Some(1)).unwrap();
    let mut changed = item("one", "first");
    changed.ordinal = 999;
    changed.body = model::TurnItemBody::SystemNotice {
        message: "updated".into(),
    };
    let updated = append(&tx, &event("updated", changed), None, Some(1)).unwrap();
    tx.commit().unwrap();
    let saved = projections::read::<model::TurnItem>(&db, "one")
        .unwrap()
        .unwrap();
    assert_eq!(saved.ordinal, 1_000_001);
    let positions: Vec<i64> = db
        .prepare("SELECT ordinal FROM orchestration_v2_turn_item_positions ORDER BY ordinal")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(positions, [1_000_001, 1_000_002]);
    assert_eq!(
        read(&db, None, 0, updated.sequence, 10).unwrap(),
        [first, second, updated.clone()]
    );
    let head:i64=db.query_row("SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='thread-projections'",[],|row|row.get(0)).unwrap();
    assert_eq!(head, updated.sequence as i64);
    assert!(matches!(&updated.event.change,model::Change::TurnItem(value) if value==&saved));
}

#[test]
fn a_queued_body_gets_its_position_at_activation_after_ordinal_reassignment() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let mut queued = item("queued-body", "second");
    queued.body = model::TurnItemBody::UserMessage {
        created_by: model::Actor::User,
        creation_source: model::CreationSource::Mobile,
        message_id: "queued-body".into(),
        input_intent: "queued_turn".into(),
        text: "hello".into(),
        attachments: vec![],
        context: None,
    };
    queued.status = model::TurnItemStatus::Pending;
    append(&tx, &event("queued", queued.clone()), None, Some(2)).unwrap();
    assert_eq!(
        tx.query_row(
            "SELECT count(*) FROM orchestration_v2_turn_item_positions",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let mut run = projections::read::<model::Run>(&tx, "second")
        .unwrap()
        .unwrap();
    queued.status = model::TurnItemStatus::Completed;
    let active = append(&tx, &event("active", queued.clone()), None, Some(4)).unwrap();
    run.status = model::RunStatus::Starting;
    run.ordinal = 4;
    projections::save(&tx, Some("thread"), &run).unwrap();
    run.status = model::RunStatus::Queued;
    projections::save(&tx, Some("thread"), &run).unwrap();
    let requeued = append(&tx, &event("requeued", queued.clone()), None, Some(4)).unwrap();
    queued.status = model::TurnItemStatus::Cancelled;
    let cancelled = append(&tx, &event("cancelled", queued), None, Some(4)).unwrap();
    tx.commit().unwrap();
    assert!(
        matches!(active.event.change,model::Change::TurnItem(value) if value.ordinal==4_000_001)
    );
    assert!(
        matches!(requeued.event.change,model::Change::TurnItem(value) if value.ordinal==4_000_001)
    );
    assert!(
        matches!(cancelled.event.change,model::Change::TurnItem(value) if value.ordinal==4_000_001)
    );
}

#[test]
fn run_start_is_reserved_before_items_and_appends_after_the_slot_is_taken() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let message = append(
        &tx,
        &event("message", item("message", "second")),
        None,
        Some(2),
    )
    .unwrap();
    let recovery = item_position(
        &tx,
        "thread",
        "recovery",
        Some("second"),
        Some(2),
        ItemPlacement::RunStart,
    )
    .unwrap();
    assert_eq!(recovery, 2_000_000);
    let saved = append(
        &tx,
        &event("recovery", item("recovery", "second")),
        None,
        Some(2),
    )
    .unwrap();
    assert!(
        matches!(&saved.event.change, model::Change::TurnItem(value) if value.ordinal == recovery)
    );
    tx.execute_batch("CREATE TEMP TRIGGER reject_redundant_position_insert BEFORE INSERT ON orchestration_v2_turn_item_positions WHEN EXISTS(SELECT 1 FROM orchestration_v2_turn_item_positions WHERE thread_id=NEW.thread_id AND turn_item_id=NEW.turn_item_id) BEGIN SELECT RAISE(ABORT,'Existing item positions must be reused'); END;").unwrap();
    assert_eq!(
        item_position(
            &tx,
            "thread",
            "recovery",
            Some("moved"),
            Some(3),
            ItemPlacement::RunStart
        )
        .unwrap(),
        recovery
    );
    assert_eq!(
        item_position(
            &tx,
            "thread",
            "retry-recovery",
            Some("second"),
            Some(2),
            ItemPlacement::RunStart
        )
        .unwrap(),
        2_000_002
    );
    assert_eq!(
        item_position(
            &tx,
            "thread",
            "reply",
            Some("second"),
            Some(2),
            ItemPlacement::Append
        )
        .unwrap(),
        2_000_003
    );
    append(
        &tx,
        &event("retry-recovery", item("retry-recovery", "second")),
        None,
        Some(2),
    )
    .unwrap();
    append(&tx, &event("reply", item("reply", "second")), None, Some(2)).unwrap();
    tx.commit().unwrap();
    assert!(
        matches!(&message.event.change,model::Change::TurnItem(value) if value.ordinal == 2_000_001)
    );
    let events = read(&db, Some("thread"), 0, saved.sequence, 10).unwrap();
    assert_eq!(events, [message, saved]);
}
