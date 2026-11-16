//! Full T3-shaped JSON is the stored record. Indexed columns are written from
//! that very value, rather than split out and reinserted during deserialization.
use super::*;

/// Keep unavailable storage separate from invalid or missing source records.
/// A queued input may fail validation without losing its predecessor's result.
/// SQL column/type corruption also aborts the transaction as a storage error.
#[derive(Debug)]
pub(super) enum ReadError {
    Storage(rusqlite::Error),
    Data(String),
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(f),
            Self::Data(error) => error.fmt(f),
        }
    }
}
impl From<rusqlite::Error> for ReadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}
impl From<serde_json::Error> for ReadError {
    fn from(error: serde_json::Error) -> Self {
        Self::Data(error.to_string())
    }
}
impl From<String> for ReadError {
    fn from(error: String) -> Self {
        Self::Data(error)
    }
}
impl From<&str> for ReadError {
    fn from(error: &str) -> Self {
        Self::Data(error.into())
    }
}
impl From<ReadError> for String {
    fn from(error: ReadError) -> Self {
        error.to_string()
    }
}
use agent_protocol::orchestration as model;
use rusqlite::types::Value as SqlValue;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

pub(super) trait Projection: Serialize + DeserializeOwned {
    const TABLE: &'static str;
    const KEY: &'static str;
    const OWNER: Option<&'static str>;
    const COLUMNS: &'static [(&'static str, &'static str)];
}

macro_rules! projection {
    ($model:ty, $table:literal, $key:literal, $owner:expr, [$($column:literal => $field:literal),* $(,)?]) => {
        impl Projection for $model {
            const TABLE: &'static str = $table;
            const KEY: &'static str = $key;
            const OWNER: Option<&'static str> = $owner;
            const COLUMNS: &'static [(&'static str, &'static str)] = &[$(($column, $field)),*];
        }
    };
}

projection!(model::Thread, "orchestration_v2_projection_threads", "thread_id", Some("thread_id"), [
    "thread_id" => "id", "project_id" => "projectId", "title" => "title",
    "default_provider" => "providerInstanceId", "provider_instance_id" => "providerInstanceId",
    "runtime_mode" => "runtimeMode", "interaction_mode" => "interactionMode",
    "active_provider_thread_id" => "activeProviderThreadId", "created_at" => "createdAt",
    "updated_at" => "updatedAt", "archived_at" => "archivedAt", "deleted_at" => "deletedAt",
]);
projection!(model::Run, "orchestration_v2_projection_runs", "run_id", Some("thread_id"), [
    "run_id" => "id", "thread_id" => "threadId", "ordinal" => "ordinal",
    "provider" => "providerInstanceId", "provider_instance_id" => "providerInstanceId",
    "provider_thread_id" => "providerThreadId", "status" => "status",
    "requested_at" => "requestedAt", "completed_at" => "completedAt",
]);
projection!(model::RunAttempt, "orchestration_v2_projection_run_attempts", "attempt_id", Some("thread_id"), [
    "attempt_id" => "id", "thread_id" => "$thread", "run_id" => "runId",
    "attempt_ordinal" => "attemptOrdinal", "root_node_id" => "rootNodeId",
    "provider" => "providerInstanceId", "provider_instance_id" => "providerInstanceId",
    "provider_thread_id" => "providerThreadId", "provider_turn_id" => "providerTurnId", "status" => "status",
]);
projection!(model::ExecutionNode,"orchestration_v2_projection_nodes","node_id",Some("thread_id"),[
    "node_id"=>"id","thread_id"=>"threadId","run_id"=>"runId","parent_node_id"=>"parentNodeId","root_node_id"=>"rootNodeId",
    "kind"=>"kind","status"=>"status","provider_thread_id"=>"providerThreadId","provider_turn_id"=>"providerTurnId",
    "runtime_request_id"=>"runtimeRequestId","checkpoint_scope_id"=>"checkpointScopeId","started_at"=>"startedAt","completed_at"=>"completedAt",
]);
projection!(model::ProviderSession, "orchestration_v2_projection_provider_sessions", "provider_session_id", None, [
    "provider_session_id" => "id", "thread_id"=>"$thread", "provider" => "providerInstanceId",
    "driver" => "driver", "provider_instance_id" => "providerInstanceId",
    "status" => "status", "model" => "model", "updated_at" => "updatedAt",
]);
projection!(model::ProviderThread, "orchestration_v2_projection_provider_threads", "provider_thread_id", Some("thread_id"), [
    "provider_thread_id" => "id", "thread_id" => "appThreadId", "owner_node_id" => "ownerNodeId",
    "provider" => "providerInstanceId", "driver" => "driver", "provider_instance_id" => "providerInstanceId",
    "provider_session_id" => "providerSessionId", "status" => "status",
    "first_run_ordinal" => "firstRunOrdinal", "last_run_ordinal" => "lastRunOrdinal", "updated_at" => "updatedAt",
]);
projection!(model::ProviderTurn, "orchestration_v2_projection_provider_turns", "provider_turn_id", Some("thread_id"), [
    "provider_turn_id" => "id", "thread_id" => "$thread", "provider_thread_id" => "providerThreadId",
    "node_id" => "nodeId", "run_attempt_id" => "runAttemptId", "ordinal" => "ordinal",
    "status" => "status", "started_at" => "startedAt", "completed_at" => "completedAt",
]);
projection!(model::TurnItem, "orchestration_v2_projection_turn_items", "turn_item_id", Some("thread_id"), [
    "turn_item_id" => "id", "thread_id" => "threadId", "run_id" => "runId", "node_id" => "nodeId",
    "provider_thread_id" => "providerThreadId", "provider_turn_id" => "providerTurnId",
    "parent_item_id" => "parentItemId", "ordinal" => "ordinal", "type" => "type",
    "input_intent" => "inputIntent", "created_by" => "createdBy",
    "status" => "status", "updated_at" => "updatedAt",
]);
projection!(model::RuntimeRequest, "orchestration_v2_projection_runtime_requests", "runtime_request_id", Some("thread_id"), [
    "runtime_request_id" => "id", "thread_id" => "$thread", "node_id" => "nodeId",
    "provider_turn_id" => "providerTurnId", "kind" => "kind", "status" => "status",
    "created_at" => "createdAt", "resolved_at" => "resolvedAt",
]);
projection!(model::Plan, "orchestration_v2_projection_plans", "plan_id", Some("thread_id"), [
    "plan_id" => "id", "thread_id" => "threadId", "run_id" => "runId", "node_id" => "nodeId",
    "kind" => "kind", "status" => "status",
]);
projection!(model::Subagent, "orchestration_v2_projection_subagents", "subagent_id", Some("thread_id"), [
    "subagent_id" => "id", "thread_id" => "threadId", "run_id" => "runId", "parent_node_id" => "parentNodeId",
    "provider" => "providerInstanceId", "driver" => "driver", "provider_instance_id" => "providerInstanceId",
    "provider_thread_id" => "providerThreadId", "child_thread_id" => "childThreadId", "origin" => "origin",
    "status" => "status", "started_at" => "startedAt", "completed_at" => "completedAt", "updated_at" => "updatedAt",
]);
projection!(model::CheckpointScope, "orchestration_v2_projection_checkpoint_scopes", "scope_id", Some("thread_id"), [
    "scope_id" => "id", "thread_id" => "threadId", "run_id" => "runId", "node_id" => "nodeId",
    "parent_scope_id" => "parentScopeId", "provider_thread_id" => "providerThreadId", "kind" => "kind",
    "ordinal_within_parent" => "ordinalWithinParent", "advances_app_run_count" => "advancesAppRunCount", "created_at" => "createdAt",
]);
projection!(model::Checkpoint, "orchestration_v2_projection_checkpoints", "checkpoint_id", Some("thread_id"), [
    "checkpoint_id" => "id", "thread_id" => "threadId", "scope_id" => "scopeId", "run_id" => "runId",
    "node_id" => "nodeId", "parent_checkpoint_id" => "parentCheckpointId", "ordinal_within_scope" => "ordinalWithinScope",
    "app_run_ordinal" => "appRunOrdinal", "status" => "status", "captured_at" => "capturedAt",
]);
projection!(model::ContextHandoff, "orchestration_v2_projection_context_handoffs", "context_handoff_id", Some("thread_id"), [
    "context_handoff_id" => "id", "thread_id" => "threadId", "target_run_id" => "targetRunId",
    "to_provider_thread_id" => "toProviderThreadId", "strategy" => "strategy", "status" => "status", "updated_at" => "updatedAt",
]);
projection!(model::ContextTransfer, "orchestration_v2_projection_context_transfers", "context_transfer_id", Some("source_thread_id"), [
    "context_transfer_id" => "id", "source_thread_id" => "sourceThreadId", "target_thread_id" => "targetThreadId",
    "target_run_id" => "targetRunId", "type" => "type", "status" => "status",
    "source_provider" => "sourceProviderInstanceId", "target_provider" => "targetProviderInstanceId",
    "source_provider_instance_id" => "sourceProviderInstanceId", "target_provider_instance_id" => "targetProviderInstanceId", "updated_at" => "updatedAt",
]);

fn scalar(value: &serde_json::Value) -> Result<SqlValue, String> {
    match value {
        serde_json::Value::Null => Ok(SqlValue::Null),
        serde_json::Value::Bool(value) => Ok(SqlValue::Integer(i64::from(*value))),
        serde_json::Value::String(value) => Ok(SqlValue::Text(value.clone())),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .ok_or_else(|| "projection value exceeds SQLite integer range".into()),
        _ => Err("indexed projection value is not a scalar".into()),
    }
}

pub(super) fn save<T: Projection>(
    transaction: &Transaction<'_>,
    thread_id: Option<&str>,
    record: &T,
) -> Result<(), String> {
    let value = serde_json::to_value(record).map_err(failure)?;
    let mut columns = Vec::with_capacity(T::COLUMNS.len() + 1);
    let mut values = Vec::with_capacity(T::COLUMNS.len() + 1);
    for (column, field) in T::COLUMNS {
        columns.push(*column);
        values.push(if *field == "$thread" {
            SqlValue::Text(
                thread_id
                    .ok_or("projection requires a conversation")?
                    .into(),
            )
        } else {
            scalar(value.get(*field).unwrap_or(&serde_json::Value::Null))?
        });
    }
    if let (Some(scope), Some(owner)) = (thread_id, T::OWNER) {
        let index = columns
            .iter()
            .position(|column| *column == owner)
            .ok_or("projection owner column is missing")?;
        if values[index] != SqlValue::Null && values[index] != SqlValue::Text(scope.into()) {
            return Err("event and projection belong to different conversations".into());
        }
    }
    columns.push("payload_json");
    values.push(SqlValue::Text(
        serde_json::to_string(&value).map_err(failure)?,
    ));
    let assignments = columns
        .iter()
        .map(|column| format!("{column}=excluded.{column}"))
        .collect::<Vec<_>>()
        .join(",");
    let ownership = T::OWNER
        .map(|owner| {
            format!(
                " WHERE ({}.{owner} IS excluded.{owner} OR {}.{owner} IS NULL)",
                T::TABLE,
                T::TABLE
            )
        })
        .unwrap_or_default();
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO UPDATE SET {}{}",
        T::TABLE,
        columns.join(","),
        vec!["?"; columns.len()].join(","),
        T::KEY,
        assignments,
        ownership
    );
    let changed = transaction
        .execute(&sql, rusqlite::params_from_iter(values))
        .map_err(failure)?;
    if changed != 1 {
        return Err("projection belongs to another conversation".into());
    }
    Ok(())
}

pub(super) fn read<T: Projection>(
    connection: &Connection,
    id: &str,
) -> Result<Option<T>, ReadError> {
    let body = connection
        .query_row(
            &format!("SELECT payload_json FROM {} WHERE {}=?1", T::TABLE, T::KEY),
            [id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(ReadError::from)?;
    body.map(|body| serde_json::from_str(&body).map_err(ReadError::from))
        .transpose()
}

pub(super) fn apply(
    transaction: &Transaction<'_>,
    thread_id: &str,
    change: &model::Change,
) -> Result<(), String> {
    use model::Change;
    match change {
        Change::ThreadCreated(value) | Change::Thread(value) => {
            save(transaction, Some(thread_id), value)
        }
        Change::RunCreated(value) | Change::Run(value) => save(transaction, Some(thread_id), value),
        Change::RunAttemptCreated(value) | Change::RunAttempt(value) => {
            save(transaction, Some(thread_id), value)
        }
        Change::Node(value) => save(transaction, Some(thread_id), value),
        Change::ProviderSession(value) => save(transaction, Some(thread_id), value),
        Change::ProviderSessionAttached(value) => {
            save(transaction, Some(thread_id), value)?;
            transaction.execute("INSERT INTO orchestration_v2_projection_provider_session_bindings(provider_session_id,thread_id) VALUES (?1,?2) ON CONFLICT DO NOTHING",
                params![value.id,thread_id]).map_err(failure)?;
            Ok(())
        }
        Change::ProviderSessionDetached(value) => {
            transaction.execute("DELETE FROM orchestration_v2_projection_provider_session_bindings WHERE provider_session_id=?1 AND thread_id=?2",
                params![value.provider_session_id,thread_id]).map_err(failure)?;
            Ok(())
        }
        Change::ProviderThread(value) => {
            if let Some(native) = value
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_ref())
            {
                let provider = ProviderKind::from_instance_id(&value.provider_instance_id)?;
                if value.driver != provider.driver()
                    || value
                        .native_thread_ref
                        .as_ref()
                        .is_none_or(|reference| reference.driver != value.driver)
                {
                    return Err("native binding uses another provider".into());
                }
                let source = SessionRef::new(provider, native.clone()).map_err(str::to_owned)?;
                let occupied = native_bindings::owner(transaction, &source)?
                    .is_some_and(|owner| Some(owner.application_id()) != value.app_thread_id);
                if occupied {
                    return Err("native conversation belongs to another app thread".into());
                }
            }
            save(transaction, Some(thread_id), value)
        }
        Change::ProviderTurn(value) => save(transaction, Some(thread_id), value),
        Change::TurnItem(value) => {
            if let Some(previous) = read::<model::TurnItem>(transaction, &value.id)?
                && previous.run_id != value.run_id
                && !promoted_message(transaction, &previous, value)?
            {
                return Err("item belongs to another run".into());
            }
            save(transaction, Some(thread_id), value)
        }
        Change::RuntimeRequest(value) => save(transaction, Some(thread_id), value),
        Change::Plan(value) => save(transaction, Some(thread_id), value),
        Change::Subagent(value) => save(transaction, Some(thread_id), value),
        Change::CheckpointScope(value) => save(transaction, Some(thread_id), value),
        Change::Checkpoint(value) => save(transaction, Some(thread_id), value),
        Change::ContextHandoff(value) => save(transaction, Some(thread_id), value),
        Change::ContextTransferCreated(value) | Change::ContextTransfer(value) => {
            if thread_id != value.source_thread_id && thread_id != value.target_thread_id {
                return Err("transfer does not involve the event's conversation".into());
            }
            save(transaction, None, value)
        }
    }
}

fn promoted_message(
    connection: &Connection,
    old: &model::TurnItem,
    new: &model::TurnItem,
) -> Result<bool, String> {
    use model::TurnItemBody::UserMessage;
    let matching = matches!((&old.body,&new.body),(
        UserMessage {message_id:left,text:left_text,attachments:left_attachments,context:left_context,input_intent:old_intent,..},
        UserMessage {message_id:right,text:right_text,attachments:right_attachments,context:right_context,input_intent,..})
        if left==right && left_text==right_text && left_attachments==right_attachments && left_context==right_context
            && old_intent=="queued_turn" && input_intent=="promoted_queued_to_steer");
    if !matching || old.thread_id != new.thread_id {
        return Ok(false);
    }
    let (Some(old_run), Some(new_run)) = (&old.run_id, &new.run_id) else {
        return Ok(false);
    };
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_runs old
        JOIN orchestration_v2_projection_runs new ON new.thread_id=old.thread_id
        WHERE old.run_id=?1 AND new.run_id=?2 AND old.status='cancelled'
        AND new.status IN ('starting','running','waiting'))",
            params![old_run, new_run],
            |row| row.get(0),
        )
        .map_err(failure)
}

fn records<T: Projection>(
    connection: &Connection,
    thread_id: &str,
) -> Result<BTreeMap<String, T>, ReadError> {
    let mut query = connection
        .prepare(&format!(
            "SELECT {},payload_json FROM {} WHERE thread_id=?1",
            T::KEY,
            T::TABLE
        ))
        .map_err(ReadError::from)?;
    let rows = query
        .query_map([thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ReadError::from)?;
    rows.map(|row| {
        let (id, body) = row.map_err(ReadError::from)?;
        Ok((id, serde_json::from_str(&body).map_err(ReadError::from)?))
    })
    .collect()
}

/// Keep bodies for current work and explicitly addressed items. History paging
/// reads bodies separately; ordinary commands do not deserialize a transcript.
fn working_items(
    connection: &Connection,
    thread_id: &str,
    item_ids: &[String],
) -> Result<BTreeMap<String, model::TurnItem>, String> {
    let mut query=connection.prepare("SELECT i.turn_item_id,i.payload_json FROM orchestration_v2_projection_turn_items i
        WHERE i.thread_id=?1 AND (
            i.turn_item_id IN (SELECT value FROM json_each(?2))
            OR (i.run_id IN (SELECT run_id FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND status IN ('preparing','queued','starting','running','waiting'))
                AND (i.status IN ('pending','running','waiting') OR i.type='user_message'))
            OR (i.status IN ('pending','running','waiting') AND i.type IN ('command_execution','dynamic_tool','subagent')
                AND i.run_id IN (SELECT run_id FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND status!='rolled_back'))
            OR (i.type='error' AND i.status='failed' AND json_extract(i.payload_json,'$.failure.class')='delivery_uncertain')
            OR i.turn_item_id IN (SELECT parent_item_id FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND type='error' AND status='failed' AND json_extract(payload_json,'$.failure.class')='delivery_uncertain')
            OR EXISTS(SELECT 1 FROM orchestration_v2_projection_runtime_requests r WHERE r.thread_id=?1 AND r.status='pending' AND r.node_id=i.node_id)
            OR EXISTS(SELECT 1 FROM orchestration_v2_projection_subagents s WHERE s.thread_id=?1 AND s.subagent_id=i.node_id AND s.status IN ('pending','running','waiting'))
        )").map_err(failure)?;
    let rows = query
        .query_map(
            params![thread_id, serde_json::to_string(item_ids).map_err(failure)?],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(failure)?;
    rows.map(|row| {
        let (id, body) = row.map_err(failure)?;
        Ok((id, serde_json::from_str(&body).map_err(failure)?))
    })
    .collect()
}

/// Decisions consume the saved T3 records, never a reconstructed private
/// input/turn/binding model. Shared sessions are joined through attachments.
pub(super) fn load(
    connection: &Connection,
    thread_id: &str,
    item_ids: &[String],
) -> Result<model::State, String> {
    let mut state = metadata(connection, thread_id)?;
    state.effects = event_store::effects(connection, thread_id)?;
    // Rollback distinguishes a compaction from an ordinary retained prompt.
    // Load that canonical input in both the native-effect read and its fenced
    // publication transaction; ordinary commands still read no past bodies.
    let mut addressed = item_ids.to_vec();
    addressed.extend(state.effects.values().filter_map(|effect| {
        let model::EffectRequest::ProviderThreadRollback { checkpoint_id, .. } = &effect.request
        else {
            return None;
        };
        let ordinal = state
            .checkpoints
            .get(checkpoint_id)?
            .app_run_ordinal
            .filter(|ordinal| *ordinal > 0)?;
        let run = state.runs.values().find(|run| run.ordinal == ordinal)?;
        Some(model::user_item_id(thread_id, &run.user_message_id))
    }));
    state.turn_items = working_items(connection, thread_id, &addressed)?;
    Ok(state)
}

pub(super) fn metadata(
    connection: &Connection,
    thread_id: &str,
) -> Result<model::State, ReadError> {
    let thread =
        read::<model::Thread>(connection, thread_id)?.ok_or("conversation does not exist")?;
    let mut state = model::State::new(thread);
    state.runs = records(connection, thread_id)?;
    state.run_attempts = records(connection, thread_id)?;
    state.nodes = records(connection, thread_id)?;
    let mut query=connection.prepare("SELECT provider_thread_id,payload_json FROM orchestration_v2_projection_provider_threads p
        WHERE thread_id=?1 OR EXISTS(SELECT 1 FROM orchestration_v2_projection_subagents s
        WHERE s.thread_id=?1 AND s.provider_thread_id=p.provider_thread_id)").map_err(ReadError::from)?;
    let rows = query
        .query_map([thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ReadError::from)?;
    for row in rows {
        let (id, body) = row.map_err(ReadError::from)?;
        state
            .provider_threads
            .insert(id, serde_json::from_str(&body).map_err(ReadError::from)?);
    }
    state.provider_turns = records(connection, thread_id)?;
    state.runtime_requests = records(connection, thread_id)?;
    state.plans = records(connection, thread_id)?;
    state.subagents = records(connection, thread_id)?;
    state.checkpoint_scopes = records(connection, thread_id)?;
    state.checkpoints = records(connection, thread_id)?;
    state.context_handoffs = records(connection, thread_id)?;
    let mut query=connection.prepare("SELECT context_transfer_id,payload_json FROM orchestration_v2_projection_context_transfers WHERE source_thread_id=?1 OR target_thread_id=?1").map_err(ReadError::from)?;
    let rows = query
        .query_map([thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(ReadError::from)?;
    for row in rows {
        let (id, body) = row.map_err(ReadError::from)?;
        state
            .context_transfers
            .insert(id, serde_json::from_str(&body).map_err(ReadError::from)?);
    }
    let mut query=connection.prepare("SELECT s.provider_session_id,s.payload_json,
        EXISTS(SELECT 1 FROM orchestration_v2_projection_provider_session_bindings b WHERE b.provider_session_id=s.provider_session_id AND b.thread_id=?1)
        FROM orchestration_v2_projection_provider_sessions s WHERE EXISTS(
            SELECT 1 FROM orchestration_v2_projection_provider_session_bindings b WHERE b.provider_session_id=s.provider_session_id AND b.thread_id=?1)
        OR EXISTS(SELECT 1 FROM orchestration_v2_projection_provider_threads p WHERE p.provider_session_id=s.provider_session_id
            AND (p.thread_id=?1 OR EXISTS(SELECT 1 FROM orchestration_v2_projection_subagents a WHERE a.thread_id=?1 AND a.provider_thread_id=p.provider_thread_id)))").map_err(ReadError::from)?;
    let rows = query
        .query_map([thread_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .map_err(ReadError::from)?;
    for row in rows {
        let (id, body, attached) = row.map_err(ReadError::from)?;
        if attached {
            state.attached_provider_sessions.insert(id.clone());
        }
        state
            .provider_sessions
            .insert(id, serde_json::from_str(&body).map_err(ReadError::from)?);
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::{database, thread};

    #[test]
    fn t3_run_json_is_kept_whole_and_its_query_columns_match() {
        let mut db = database();
        let thread = thread("thread");
        let run: model::Run = serde_json::from_value(serde_json::json!({
            "id":"run","threadId":"thread","ordinal":1,"providerInstanceId":"codex",
            "modelSelection":{"instanceId":"codex","model":"example"},"providerThreadId":null,
            "userMessageId":"message","rootNodeId":null,"activeAttemptId":null,"status":"queued",
            "queuePosition":2,"queueHeld":true,"steerWhenReadyRunId":null,
            "requestedAt":"2026-11-14T00:00:00.000Z","startedAt":null,"completedAt":null,
            "checkpointId":null,"contextHandoffId":null,"restartContinuationOfRunId":null,
            "usageLimitContinuationOfRunId":null,"workStartedAt":null,"workspacePreparation":null
        }))
        .unwrap();
        let tx = db.transaction().unwrap();
        save(&tx, None, &thread).unwrap();
        save(&tx, Some("thread"), &run).unwrap();
        tx.commit().unwrap();
        assert_eq!(read::<model::Run>(&db, "run").unwrap(), Some(run));
        let stored:(String,String,i64,String)=db.query_row(
            "SELECT status,thread_id,ordinal,payload_json FROM orchestration_v2_projection_runs WHERE run_id='run'",[],
            |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
        assert_eq!((&*stored.0, &*stored.1, stored.2), ("queued", "thread", 1));
        let payload: serde_json::Value = serde_json::from_str(&stored.3).unwrap();
        assert_eq!(payload["status"], "queued");
        assert_eq!(payload["queuePosition"], 2);
        assert_eq!(payload["queueHeld"], true);
        assert_eq!(payload["userMessageId"], "message");
    }

    #[test]
    fn an_orphan_run_aborts_the_transaction_and_cannot_leave_a_projection() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("orchestration.sql")).unwrap();
        db.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO orchestration_v2_projection_runs(run_id,thread_id,ordinal,provider,status,requested_at,payload_json) VALUES ('run','missing',1,'codex','queued','2026-11-14T00:00:00.000Z','{}')",[]).unwrap();
        assert!(tx.commit().is_err());
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM orchestration_v2_projection_runs",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(db.execute("INSERT INTO orchestration_v2_projection_provider_sessions(provider_session_id,provider,status,updated_at,payload_json) VALUES ('session','codex','invalid','2026-11-14T00:00:00.000Z','{}')",[]).is_err());
    }

    #[test]
    fn committed_events_replay_the_historical_value_after_the_projection_changes() {
        let mut db = database();
        let initial = thread("thread");
        let event = |id: &str, change| model::Event {
            id: id.into(),
            thread_id: "thread".into(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: "2026-11-14T00:00:00.000Z".into(),
            change,
        };
        let tx = db.transaction().unwrap();
        let first = event_store::append(
            &tx,
            &event("created", model::Change::ThreadCreated(initial.clone())),
            Some("create"),
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        let mut changed = initial.clone();
        changed.title = "Renamed".into();
        let tx = db.transaction().unwrap();
        let second = event_store::append(
            &tx,
            &event("renamed", model::Change::Thread(changed.clone())),
            Some("rename"),
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        assert_eq!(
            read::<model::Thread>(&db, "thread").unwrap(),
            Some(changed.clone())
        );
        assert_eq!(
            event_store::read(&db, Some("thread"), 0, second.sequence, 10).unwrap(),
            [first.clone(), second.clone()]
        );
        assert_eq!(
            event_store::read(&db, Some("thread"), first.sequence, second.sequence, 10).unwrap(),
            [second]
        );
        assert_eq!(load(&db, "thread", &[]).unwrap().thread, changed);
        let original_payload: String = db
            .query_row(
                "SELECT payload_json FROM orchestration_events WHERE event_id='created'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&original_payload).unwrap()["title"],
            "Example"
        );
    }

    #[test]
    fn projection_and_event_rollback_together_when_a_scoped_reference_is_invalid() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        save(&tx, None, &thread("thread")).unwrap();
        save(&tx, None, &thread("other")).unwrap();
        tx.commit().unwrap();
        let run:model::Run=serde_json::from_value(serde_json::json!({
            "id":"run","threadId":"thread","ordinal":1,"providerInstanceId":"codex","modelSelection":{"instanceId":"codex","model":"example"},
            "providerThreadId":null,"userMessageId":"message","rootNodeId":null,"activeAttemptId":null,"status":"queued",
            "queuePosition":1,"queueHeld":false,"requestedAt":"2026-11-14T00:00:00.000Z","startedAt":null,"completedAt":null,
            "checkpointId":null,"contextHandoffId":null
        })).unwrap();
        let tx = db.transaction().unwrap();
        save(&tx, Some("thread"), &run).unwrap();
        tx.commit().unwrap();
        let item = model::TurnItem {
            detail_omitted: false,
            id: "foreign".into(),
            thread_id: "other".into(),
            run_id: Some("run".into()),
            node_id: None,
            provider_thread_id: None,
            provider_turn_id: None,
            native_item_ref: None,
            parent_item_id: None,
            ordinal: 1,
            status: model::TurnItemStatus::Completed,
            title: None,
            started_at: None,
            completed_at: None,
            updated_at: "2026-11-14T00:00:00.000Z".into(),
            body: model::TurnItemBody::SystemNotice {
                message: "wrong owner".into(),
            },
        };
        let event = model::Event {
            id: "bad".into(),
            thread_id: "other".into(),
            run_id: None,
            node_id: None,
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: "2026-11-14T00:00:00.000Z".into(),
            change: model::Change::TurnItem(item),
        };
        let tx = db.transaction().unwrap();
        event_store::append(&tx, &event, Some("command"), Some(1)).unwrap();
        assert!(tx.commit().is_err());
        assert!(read::<model::TurnItem>(&db, "foreign").unwrap().is_none());
        assert!(event_store::read(&db, None, 0, 100, 10).unwrap().is_empty());
        let tx = db.transaction().unwrap();
        let mut stolen = run.clone();
        stolen.thread_id = "other".into();
        assert!(save(&tx, Some("other"), &stolen).is_err());
        assert_eq!(read::<model::Run>(&tx, "run").unwrap(), Some(run));
    }

    #[test]
    fn shared_session_attachments_are_independent_and_detaching_preserves_the_other_thread() {
        let mut db = database();
        let tx = db.transaction().unwrap();
        for id in ["first", "second"] {
            save(&tx, None, &thread(id)).unwrap();
        }
        let session = model::ProviderSession {
            id: "shared".into(),
            driver: "codex".into(),
            provider_instance_id: "codex".into(),
            status: model::ProviderSessionStatus::Ready,
            cwd: "/project".into(),
            model: None,
            capabilities: Default::default(),
            created_at: "2026-11-14T00:00:00.000Z".into(),
            updated_at: "2026-11-14T00:00:00.000Z".into(),
            last_error: None,
        };
        for id in ["first", "second"] {
            apply(
                &tx,
                id,
                &model::Change::ProviderSessionAttached(session.clone()),
            )
            .unwrap();
        }
        tx.commit().unwrap();
        let tx = db.transaction().unwrap();
        apply(
            &tx,
            "first",
            &model::Change::ProviderSessionDetached(model::ProviderSessionDetached {
                provider_session_id: "shared".into(),
                detached_at: "2026-11-14T00:00:01.000Z".into(),
            }),
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(
            load(&db, "first", &[])
                .unwrap()
                .attached_provider_sessions
                .is_empty()
        );
        assert!(
            load(&db, "second", &[])
                .unwrap()
                .attached_provider_sessions
                .contains("shared")
        );
        assert_eq!(
            read::<model::ProviderSession>(&db, "shared").unwrap(),
            Some(session)
        );
    }
    #[test]
    fn a_native_child_resource_keeps_its_own_app_thread_and_session_without_becoming_attached_to_the_parent()
     {
        let mut db = database();
        let tx = db.transaction().unwrap();
        for id in ["parent", "child"] {
            save(&tx, None, &thread(id)).unwrap();
        }
        let session = model::ProviderSession {
            id: "child-session".into(),
            driver: "codex".into(),
            provider_instance_id: "codex".into(),
            status: model::ProviderSessionStatus::Ready,
            cwd: "/project".into(),
            model: None,
            capabilities: Default::default(),
            created_at: "now".into(),
            updated_at: "now".into(),
            last_error: None,
        };
        apply(
            &tx,
            "child",
            &model::Change::ProviderSessionAttached(session.clone()),
        )
        .unwrap();
        let child = model::ProviderThread {
            id: "child-native".into(),
            driver: "codex".into(),
            provider_instance_id: "codex".into(),
            provider_session_id: Some(session.id.clone()),
            app_thread_id: Some("child".into()),
            owner_node_id: Some("parent-subagent".into()),
            native_thread_ref: Some(model::ProviderRef {
                driver: "codex".into(),
                native_id: Some("native-child".into()),
                strength: model::NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            native_conversation_head_ref: None,
            native_metadata: None,
            status: model::ProviderThreadStatus::Active,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            created_at: "now".into(),
            updated_at: "now".into(),
        };
        apply(&tx, "child", &model::Change::ProviderThread(child.clone())).unwrap();
        let root:model::ExecutionNode=serde_json::from_value(serde_json::json!({
            "id":"parent-root","threadId":"parent","runId":null,"parentNodeId":null,"rootNodeId":"parent-root",
            "kind":"root_turn","status":"running","countsForRun":false,"providerThreadId":null,"providerTurnId":null,
            "nativeItemRef":null,"runtimeRequestId":null,"checkpointScopeId":null,"startedAt":null,"completedAt":null
        })).unwrap();
        apply(&tx, "parent", &model::Change::Node(root.clone())).unwrap();
        apply(
            &tx,
            "parent",
            &model::Change::Node(model::ExecutionNode {
                id: "parent-subagent".into(),
                kind: model::NodeKind::Subagent,
                parent_node_id: Some(root.id.clone()),
                provider_thread_id: Some(child.id.clone()),
                ..root
            }),
        )
        .unwrap();
        let item:model::TurnItem=serde_json::from_value(serde_json::json!({
            "id":"parent-subagent-item","threadId":"parent","runId":null,"nodeId":"parent-subagent",
            "parentNodeId":"parent-root","rootNodeId":"parent-root","providerThreadId":"child-native","providerTurnId":null,"nativeItemRef":null,"parentItemId":null,
            "ordinal":1,"status":"running","title":null,"startedAt":null,"completedAt":null,"updatedAt":"now",
            "type":"subagent","subagentId":"parent-subagent","origin":"provider_native","driver":"codex",
            "providerInstanceId":"codex","childThreadId":"child","prompt":"child work","result":null
        })).unwrap();
        apply(&tx, "parent", &model::Change::TurnItem(item)).unwrap();
        let task = model::Subagent {
            id: "parent-subagent".into(),
            thread_id: "parent".into(),
            parent_node_id: "parent-root".into(),
            run_id: None,
            origin: "provider_native".into(),
            created_by: "agent".into(),
            driver: "codex".into(),
            provider_instance_id: "codex".into(),
            provider_thread_id: Some(child.id.clone()),
            child_thread_id: Some("child".into()),
            native_task_ref: None,
            title: None,
            model: None,
            status: model::SubagentStatus::Running,
            started_at: None,
            completed_at: None,
            updated_at: "now".into(),
        };
        apply(&tx, "parent", &model::Change::Subagent(task)).unwrap();
        tx.commit().unwrap();
        let parent = load(&db, "parent", &[]).unwrap();
        assert_eq!(parent.provider_threads["child-native"], child);
        assert_eq!(parent.provider_sessions["child-session"], session);
        assert!(!parent.attached_provider_sessions.contains("child-session"));
        assert!(
            load(&db, "child", &[])
                .unwrap()
                .attached_provider_sessions
                .contains("child-session")
        );
        let tx = db.transaction().unwrap();
        let stolen = model::ProviderThread {
            id: "stolen".into(),
            app_thread_id: Some("parent".into()),
            ..child
        };
        assert!(apply(&tx, "parent", &model::Change::ProviderThread(stolen)).is_err());
        assert!(
            read::<model::ProviderThread>(&tx, "stolen")
                .unwrap()
                .is_none()
        );
    }
}
