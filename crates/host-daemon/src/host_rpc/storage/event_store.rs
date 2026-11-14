//! T3's common event log and projections share one transaction. Replay reads
//! these committed domain events; it never reconstructs changes from latest rows.
use super::*;
use agent_protocol::orchestration::{self as model, Event, StoredEvent};
use serde_json::Value;

pub(super) fn head(connection: &Connection, thread_id: Option<&str>) -> Result<u64, String> {
    connection.query_row("SELECT coalesce(max(sequence),0) FROM orchestration_events WHERE aggregate_kind='thread' AND application_event_version=2 AND (?1 IS NULL OR stream_id=?1)",
        [thread_id], |row|row.get::<_,i64>(0)).map_err(failure)?.try_into().map_err(failure)
}

pub(super) fn append(
    transaction: &Transaction<'_>,
    event: &Event,
    command_id: Option<&str>,
    run_ordinal: Option<u64>,
) -> Result<StoredEvent, String> {
    let mut event = event.clone();
    if let model::Change::TurnItem(item) = &mut event.change {
        normalize_item(transaction, item, run_ordinal)?;
    }
    projections::apply(transaction, &event.thread_id, &event.change)?;
    let mut encoded = serde_json::to_value(&event).map_err(failure)?;
    let fields = encoded.as_object_mut().ok_or("event is not an object")?;
    let payload = fields.remove("payload").ok_or("event payload is missing")?;
    let event_type = fields
        .remove("type")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or("event type is missing")?;
    for key in ["id", "threadId", "occurredAt"] {
        fields.remove(key);
    }
    transaction.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,command_id,causation_event_id,correlation_id,actor_kind,payload_json,metadata_json,application_event_version)
        VALUES (?1,'thread',?2,coalesce((SELECT max(stream_version)+1 FROM orchestration_events WHERE aggregate_kind='thread' AND stream_id=?2),0),?3,?4,?5,NULL,?5,?6,?7,?8,2)",
        params![event.id,event.thread_id,event_type,event.occurred_at,command_id,
            if event.raw_event_id.is_some() {"provider"} else {"server"},
            serde_json::to_string(&payload).map_err(failure)?,serde_json::to_string(fields).map_err(failure)?]).map_err(failure)?;
    let sequence: u64 = transaction
        .last_insert_rowid()
        .try_into()
        .map_err(failure)?;
    transaction.execute("INSERT INTO orchestration_v2_projection_metadata(projection_name,schema_version,last_sequence,updated_at)
        VALUES ('thread-projections',2,?1,?2) ON CONFLICT(projection_name) DO UPDATE SET
        last_sequence=excluded.last_sequence,updated_at=excluded.updated_at",
        params![integer(sequence)?,event.occurred_at]).map_err(failure)?;
    Ok(StoredEvent {
        sequence,
        command_id: command_id.map(str::to_owned),
        event,
    })
}

/// T3's persisted item positions are allocated once.
/// A queued body replaces T3's separate
/// message row, so it receives a transcript position only when activated.
fn normalize_item(
    transaction: &Transaction<'_>,
    item: &mut model::TurnItem,
    run_ordinal: Option<u64>,
) -> Result<(), String> {
    let existing:Option<i64>=transaction.query_row(
        "SELECT ordinal FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND turn_item_id=?2",
        params![item.thread_id,item.id],|row|row.get(0)).optional().map_err(failure)?;
    if let Some(ordinal) = existing {
        item.ordinal = ordinal.try_into().map_err(failure)?;
        return Ok(());
    }
    if matches!(
        item.status,
        model::TurnItemStatus::Pending | model::TurnItemStatus::Cancelled
    ) && matches!(&item.body,model::TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
    {
        item.ordinal = 0;
        return Ok(());
    }
    if item.run_id.is_some() != run_ordinal.is_some() {
        return Err("item position requires its run ordinal".into());
    }
    let lower = integer(run_ordinal.unwrap_or(0))?
        .checked_mul(1_000_000)
        .ok_or("item position exhausted")?;
    let upper = lower
        .checked_add(999_999)
        .ok_or("item position exhausted")?;
    let last: i64 = transaction
        .query_row(
            "SELECT coalesce(max(ordinal),?2) FROM orchestration_v2_turn_item_positions
                WHERE thread_id=?1 AND ordinal BETWEEN ?2 AND ?3",
            params![item.thread_id, lower, upper],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let ordinal = last
        .checked_add(1)
        .filter(|ordinal| *ordinal <= upper)
        .ok_or("item run position exhausted")?;
    transaction.execute("INSERT INTO orchestration_v2_turn_item_positions(thread_id,turn_item_id,ordinal) VALUES (?1,?2,?3)",
                params![item.thread_id,item.id,ordinal]).map_err(failure)?;
    item.ordinal = ordinal.try_into().map_err(failure)?;
    Ok(())
}

pub(super) fn read(
    connection: &Connection,
    thread_id: Option<&str>,
    after: u64,
    through: u64,
    limit: usize,
) -> Result<Vec<StoredEvent>, String> {
    let mut query=connection.prepare("SELECT sequence,command_id,event_id,stream_id,event_type,occurred_at,payload_json,metadata_json FROM orchestration_events WHERE aggregate_kind='thread' AND application_event_version=2 AND (?1 IS NULL OR stream_id=?1) AND sequence>?2 AND sequence<=?3 ORDER BY sequence LIMIT ?4").map_err(failure)?;
    let mut rows = query
        .query(params![
            thread_id,
            integer(after)?,
            integer(through)?,
            i64::try_from(limit).map_err(failure)?
        ])
        .map_err(failure)?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let payload: Value =
            serde_json::from_str(&row.get::<_, String>(6).map_err(failure)?).map_err(failure)?;
        let metadata: Value =
            serde_json::from_str(&row.get::<_, String>(7).map_err(failure)?).map_err(failure)?;
        let mut event = metadata
            .as_object()
            .cloned()
            .ok_or("event metadata is not an object")?;
        for (field, column) in [("id", 2), ("threadId", 3), ("type", 4), ("occurredAt", 5)] {
            event.insert(
                field.into(),
                row.get::<_, String>(column).map_err(failure)?.into(),
            );
        }
        event.insert("payload".into(), payload);
        events.push(StoredEvent {
            sequence: row
                .get::<_, i64>(0)
                .map_err(failure)?
                .try_into()
                .map_err(failure)?,
            command_id: row.get(1).map_err(failure)?,
            event: serde_json::from_value(event.into()).map_err(failure)?,
        });
    }
    Ok(events)
}

pub(super) fn save_effect(
    transaction: &Transaction<'_>,
    effect: &model::Effect,
) -> Result<(), String> {
    let status = serde_json::to_value(effect.status).map_err(failure)?;
    let request = serde_json::to_value(&effect.request).map_err(failure)?;
    let kind = request
        .get("type")
        .and_then(Value::as_str)
        .ok_or("effect type is missing")?;
    if effect.status == model::EffectStatus::Pending && effect.attempt_count == 0 {
        transaction.execute("INSERT INTO orchestration_v2_effect_outbox(effect_id,command_id,thread_id,effect_type,payload_json,status,attempt_count,available_at,lease_owner,lease_expires_at,created_at,updated_at,completed_at,last_error)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
        ON CONFLICT(effect_id) DO NOTHING",
        params![effect.id,effect.command_id,effect.thread_id,kind,serde_json::to_string(&request).map_err(failure)?,
            status.as_str().ok_or("effect status is not a string")?,integer(effect.attempt_count)?,effect.available_at,effect.lease_owner,
            effect.lease_expires_at,effect.created_at,effect.updated_at,effect.completed_at,effect.last_error]).map_err(failure)?;
        return Ok(());
    }
    transaction.execute("UPDATE orchestration_v2_effect_outbox SET status=?6,attempt_count=?7,available_at=?8,lease_owner=?9,lease_expires_at=?10,updated_at=?11,completed_at=?12,last_error=?13
        WHERE effect_id=?1 AND command_id=?2 AND thread_id=?3 AND effect_type=?4 AND payload_json=?5",
        params![effect.id,effect.command_id,effect.thread_id,kind,serde_json::to_string(&request).map_err(failure)?,status.as_str().ok_or("effect status is not a string")?,integer(effect.attempt_count)?,effect.available_at,effect.lease_owner,effect.lease_expires_at,effect.updated_at,effect.completed_at,effect.last_error])
        .map_err(failure).and_then(|count|if count==1 {Ok(())} else {Err("effect does not belong to this decision".into())})
}

pub(super) fn effects(
    connection: &Connection,
    thread_id: &str,
) -> Result<std::collections::BTreeMap<String, model::Effect>, String> {
    let mut query=connection.prepare(&format!("SELECT {EFFECT_COLUMNS} FROM orchestration_v2_effect_outbox WHERE thread_id=?1 AND status IN ('pending','running')")).map_err(failure)?;
    let mut rows = query.query([thread_id]).map_err(failure)?;
    let mut effects = std::collections::BTreeMap::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let value = effect(row)?;
        effects.insert(value.id.clone(), value);
    }
    Ok(effects)
}

pub(super) const EFFECT_COLUMNS: &str = "effect_id,command_id,thread_id,payload_json,status,attempt_count,available_at,lease_owner,lease_expires_at,created_at,updated_at,completed_at,last_error";

pub(super) fn effect(row: &rusqlite::Row<'_>) -> Result<model::Effect, String> {
    Ok(model::Effect {
        id: row.get(0).map_err(failure)?,
        command_id: row.get(1).map_err(failure)?,
        thread_id: row.get(2).map_err(failure)?,
        request: serde_json::from_str(&row.get::<_, String>(3).map_err(failure)?)
            .map_err(failure)?,
        status: serde_json::from_value(Value::String(row.get(4).map_err(failure)?))
            .map_err(failure)?,
        attempt_count: row
            .get::<_, i64>(5)
            .map_err(failure)?
            .try_into()
            .map_err(failure)?,
        available_at: row.get(6).map_err(failure)?,
        lease_owner: row.get(7).map_err(failure)?,
        lease_expires_at: row.get(8).map_err(failure)?,
        created_at: row.get(9).map_err(failure)?,
        updated_at: row.get(10).map_err(failure)?,
        completed_at: row.get(11).map_err(failure)?,
        last_error: row.get(12).map_err(failure)?,
    })
}

#[cfg(test)]
mod tests {
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
        })).unwrap()
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
        let requeued = append(&tx, &event("requeued", queued), None, Some(4)).unwrap();
        tx.commit().unwrap();
        assert!(
            matches!(active.event.change,model::Change::TurnItem(value) if value.ordinal==4_000_001)
        );
        assert!(
            matches!(requeued.event.change,model::Change::TurnItem(value) if value.ordinal==4_000_001)
        );
    }
}
