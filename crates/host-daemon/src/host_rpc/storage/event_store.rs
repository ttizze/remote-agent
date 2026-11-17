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
    if !matches!(
        event.change,
        model::Change::ThreadCreated(_) | model::Change::Thread(_)
    ) {
        transaction.prepare_cached("UPDATE orchestration_v2_projection_threads
            SET updated_at=?1,payload_json=json_set(payload_json,'$.updatedAt',?1) WHERE thread_id=?2")
            .map_err(failure)?.execute(params![event.occurred_at,event.thread_id]).map_err(failure)?;
    }
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
    transaction.prepare_cached("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,command_id,causation_event_id,correlation_id,actor_kind,payload_json,metadata_json,application_event_version)
        VALUES (?1,'thread',?2,coalesce((SELECT max(stream_version)+1 FROM orchestration_events WHERE aggregate_kind='thread' AND stream_id=?2),0),?3,?4,?5,NULL,?5,?6,?7,?8,2)")
        .map_err(failure)?.execute(params![event.id,event.thread_id,event_type,event.occurred_at,command_id,
            if event.raw_event_id.is_some() {"provider"} else {"server"},
            serde_json::to_string(&payload).map_err(failure)?,serde_json::to_string(fields).map_err(failure)?]).map_err(failure)?;
    let sequence: u64 = transaction
        .last_insert_rowid()
        .try_into()
        .map_err(failure)?;
    transaction.prepare_cached("INSERT INTO orchestration_v2_projection_metadata(projection_name,schema_version,last_sequence,updated_at)
        VALUES ('thread-projections',2,?1,?2) ON CONFLICT(projection_name) DO UPDATE SET
        last_sequence=excluded.last_sequence,updated_at=excluded.updated_at")
        .map_err(failure)?.execute(params![integer(sequence)?,event.occurred_at]).map_err(failure)?;
    Ok(StoredEvent {
        sequence,
        command_id: command_id.map(str::to_owned),
        event,
    })
}

/// Project metadata shares the application log and the transaction that owns it.
pub(super) fn append_metadata(
    transaction: &Transaction<'_>,
    aggregate: &str,
    id: &str,
    kind: &str,
    now: &str,
    payload: &Value,
) -> Result<(), String> {
    transaction.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,command_id,causation_event_id,correlation_id,actor_kind,payload_json,metadata_json,application_event_version)
        VALUES (?1,?6,?2,coalesce((SELECT max(stream_version)+1 FROM orchestration_events WHERE aggregate_kind=?6 AND stream_id=?2),0),?3,?4,NULL,NULL,NULL,'server',?5,'{}',1)",
        params![Uuid::new_v4().to_string(),id,kind,now,serde_json::to_string(payload).map_err(failure)?,aggregate]).map_err(failure)?;
    Ok(())
}

/// T3's persisted item positions are allocated once.
/// A queued body replaces T3's separate
/// message row, so it receives a transcript position only when activated.
fn normalize_item(
    transaction: &Transaction<'_>,
    item: &mut model::TurnItem,
    run_ordinal: Option<u64>,
) -> Result<(), String> {
    let placement = if matches!(
        item.status,
        model::TurnItemStatus::Pending | model::TurnItemStatus::Cancelled
    ) && matches!(&item.body,model::TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
    {
        ItemPlacement::Deferred
    } else {
        ItemPlacement::Append
    };
    item.ordinal = item_position(
        transaction,
        &item.thread_id,
        &item.id,
        item.run_id.as_deref(),
        run_ordinal,
        placement,
    )?;
    Ok(())
}

pub(super) enum ItemPlacement {
    Deferred,
    Append,
    RunStart,
}

/// Both explicit recovery reservations and ordinary event writes use the same
/// allocation rule. Existing positions survive retries and queue cancellation.
pub(super) fn item_position(
    transaction: &Transaction<'_>,
    thread_id: &str,
    item_id: &str,
    run_id: Option<&str>,
    run_ordinal: Option<u64>,
    placement: ItemPlacement,
) -> Result<u64, String> {
    let existing:Option<i64>=transaction.prepare_cached(
        "SELECT ordinal FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND turn_item_id=?2")
        .map_err(failure)?.query_row(params![thread_id,item_id],|row|row.get(0)).optional().map_err(failure)?;
    if let Some(ordinal) = existing {
        return ordinal.try_into().map_err(failure);
    }
    if matches!(placement, ItemPlacement::Deferred) {
        return Ok(0);
    }
    if run_id.is_some() != run_ordinal.is_some() {
        return Err("item position requires its run ordinal".into());
    }
    // Run-less native work (imported history, or a turn the provider started
    // itself) follows everything the thread already shows.
    if run_id.is_none() {
        let last: i64 = transaction
            .prepare_cached(
                "SELECT coalesce(max(ordinal),0) FROM orchestration_v2_turn_item_positions WHERE thread_id=?1")
            .map_err(failure)?.query_row(
                [thread_id],
                |row| row.get(0),
            )
            .map_err(failure)?;
        let ordinal = last.checked_add(1).ok_or("item position exhausted")?;
        transaction.prepare_cached("INSERT INTO orchestration_v2_turn_item_positions(thread_id,turn_item_id,ordinal) VALUES (?1,?2,?3)")
                .map_err(failure)?.execute(params![thread_id,item_id,ordinal]).map_err(failure)?;
        return ordinal.try_into().map_err(failure);
    }
    let lower = integer(run_ordinal.unwrap_or(0))?
        .checked_mul(1_000_000)
        .ok_or("item position exhausted")?;
    let upper = lower
        .checked_add(999_999)
        .ok_or("item position exhausted")?;
    let (first, last): (i64, i64) = transaction
        .prepare_cached(
            "SELECT coalesce(min(ordinal),?3),coalesce(max(ordinal),?2) FROM orchestration_v2_turn_item_positions
                WHERE thread_id=?1 AND ordinal BETWEEN ?2 AND ?3")
        .map_err(failure)?.query_row(
            params![thread_id, lower, upper],
            |row| Ok((row.get(0)?,row.get(1)?)),
        )
        .map_err(failure)?;
    let ordinal = if matches!(placement, ItemPlacement::RunStart) && first > lower {
        lower
    } else {
        last.checked_add(1)
            .filter(|ordinal| *ordinal <= upper)
            .ok_or("item run position exhausted")?
    };
    transaction.prepare_cached("INSERT INTO orchestration_v2_turn_item_positions(thread_id,turn_item_id,ordinal) VALUES (?1,?2,?3)")
                .map_err(failure)?.execute(params![thread_id,item_id,ordinal]).map_err(failure)?;
    ordinal.try_into().map_err(failure)
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
mod tests;
