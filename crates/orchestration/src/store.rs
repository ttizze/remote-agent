//! A single transaction owns receipts, events, projections and the effect outbox.
//! The publish lane is the same lock: subscribers cannot observe a commit gap.
use crate::{contracts::*, decider, projector};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Mutex, MutexGuard},
};
use tokio::sync::broadcast;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Decision(#[from] decider::DecisionError),
    #[error("command id is already bound to another thread")]
    CommandConflict,
    #[error("command was rejected: {0}")]
    PreviouslyRejected(String),
    #[error("orchestration store lock poisoned")]
    Poisoned,
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error("thread not found")]
    ThreadNotFound,
    #[error("effect lease is no longer owned")]
    LeaseLost,
    #[error("invalid query: {0}")]
    InvalidQuery(&'static str),
}
pub type Result<T> = std::result::Result<T, StoreError>;
pub struct Store {
    connection: Mutex<Connection>,
    committed: broadcast::Sender<StoredEvent>,
}
#[derive(Debug, Clone)]
pub struct Commit {
    pub sequence: u64,
    pub events: Vec<StoredEvent>,
    pub replayed: bool,
}
pub struct Subscription<T> {
    pub initial: Vec<T>,
    pub receiver: broadcast::Receiver<StoredEvent>,
    pub cursor: u64,
}
#[derive(Debug, Clone)]
pub struct ClaimedEffect {
    pub effect: Effect,
    pub attempt: u32,
    pub lease_owner: String,
}

const ENTITIES: &[(&str, &str)] = &[
    ("runs", "run_id"),
    ("run_attempts", "attempt_id"),
    ("nodes", "node_id"),
    ("subagents", "subagent_id"),
    ("provider_sessions", "provider_session_id"),
    ("provider_threads", "provider_thread_id"),
    ("provider_turns", "provider_turn_id"),
    ("runtime_requests", "runtime_request_id"),
    ("messages", "message_id"),
    ("plans", "plan_id"),
    ("turn_items", "turn_item_id"),
    ("checkpoint_scopes", "scope_id"),
    ("checkpoints", "checkpoint_id"),
    ("context_handoffs", "context_handoff_id"),
    ("context_transfers", "context_transfer_id"),
];
impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }
    pub fn memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }
    fn from_connection(connection: Connection) -> Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(include_str!("schema.sql"))?;
        for (table, key) in ENTITIES {
            connection.execute_batch(&format!("CREATE TABLE IF NOT EXISTS orchestration_v2_projection_{table} ({key} TEXT NOT NULL, thread_id TEXT NOT NULL, payload_json TEXT NOT NULL, PRIMARY KEY(thread_id, {key})); CREATE INDEX IF NOT EXISTS orchestration_v2_{table}_thread ON orchestration_v2_projection_{table}(thread_id);"))?;
        }
        let (committed, _) = broadcast::channel(2048);
        Ok(Self {
            connection: Mutex::new(connection),
            committed,
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
    pub fn projection(&self, id: &ThreadId) -> Result<ThreadProjection> {
        load_projection(&*self.lock()?, id)?.ok_or(StoreError::ThreadNotFound)
    }
    pub fn sequence(&self) -> Result<u64> {
        latest_sequence(&*self.lock()?, None)
    }
    pub fn shell_snapshot(&self) -> Result<ShellSnapshot> {
        shell_snapshot(&*self.lock()?)
    }
    pub fn dispatch(
        &self,
        command: &Command,
        now: &Timestamp,
        capabilities: &TurnCapabilities,
        driver: Driver,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let receipt: Option<(String, u64, String, Option<String>)> = transaction.query_row(
            "SELECT aggregate_id,result_sequence,status,error FROM orchestration_command_receipts WHERE command_id=?1",
            [command.command_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional()?;
        if let Some((thread, sequence, status, error)) = receipt {
            if thread != command.thread_id.as_str() {
                return Err(StoreError::CommandConflict);
            }
            if status == "rejected" {
                return Err(StoreError::PreviouslyRejected(error.unwrap_or_default()));
            }
            let events = query_events(
                &transaction,
                "command_id=?1",
                command.command_id.as_str(),
                0,
                u64::MAX,
                usize::MAX,
            )?;
            return Ok(Commit {
                sequence,
                events,
                replayed: true,
            });
        }
        let projection = load_projection(&transaction, &command.thread_id)?;
        let decision = match decider::decide(
            command,
            projection.as_ref(),
            now,
            capabilities,
            driver,
        ) {
            Ok(decision) => decision,
            Err(error) => {
                transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,error,command_type) VALUES(?1,'thread',?2,?3,0,'rejected',?4,?5)", params![command.command_id.as_str(), command.thread_id.as_str(), now.as_str(), error.to_string(), kind_name(&command.body)?])?;
                transaction.commit()?;
                return Err(StoreError::Decision(error));
            }
        };
        transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,command_type) VALUES(?1,'thread',?2,?3,0,'accepted',?4)", params![command.command_id.as_str(), command.thread_id.as_str(), now.as_str(), kind_name(&command.body)?])?;
        let events = commit_decision(&transaction, decision, Some(&command.command_id), now)?;
        let sequence = events
            .last()
            .map_or(latest_sequence(&transaction, None)?, |event| event.sequence);
        transaction.execute(
            "UPDATE orchestration_command_receipts SET result_sequence=?2 WHERE command_id=?1",
            params![command.command_id.as_str(), sequence],
        )?;
        transaction.commit()?;
        self.publish(&events);
        Ok(Commit {
            sequence,
            events,
            replayed: false,
        })
    }
    fn publish(&self, events: &[StoredEvent]) {
        for event in events {
            let _ = self.committed.send(event.clone());
        }
    }
    /// Provider writes use optimistic ownership checks against the committed run.
    pub fn ingest(
        &self,
        events: Vec<DomainEvent>,
        expected_run: Option<(&RunId, Option<&RunAttemptId>)>,
        now: &Timestamp,
    ) -> Result<Commit> {
        self.ingest_events(events, expected_run, now, false)
    }
    pub fn ingest_checkpoint(
        &self,
        events: Vec<DomainEvent>,
        expected_run: Option<(&RunId, Option<&RunAttemptId>)>,
        now: &Timestamp,
    ) -> Result<Commit> {
        self.ingest_events(events, expected_run, now, true)
    }
    fn ingest_events(
        &self,
        mut events: Vec<DomainEvent>,
        expected_run: Option<(&RunId, Option<&RunAttemptId>)>,
        now: &Timestamp,
        checkpoint_capture: bool,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut effects = vec![];
        if checkpoint_capture && expected_run.is_none() {
            return Err(StoreError::InvalidEvent(
                "checkpoint capture requires a run guard".into(),
            ));
        }
        if let Some((run_id, attempt_id)) = expected_run {
            let thread_id = events
                .first()
                .ok_or_else(|| StoreError::InvalidEvent("empty guarded write".into()))?
                .thread_id
                .clone();
            let projection =
                load_projection(&transaction, &thread_id)?.ok_or(StoreError::ThreadNotFound)?;
            if !projection.runs.iter().any(|run| {
                run.id == *run_id
                    && (run.status.is_blocking() && !checkpoint_capture
                        || checkpoint_capture
                            && matches!(
                                run.status,
                                RunStatus::Waiting
                                    | RunStatus::Completed
                                    | RunStatus::Interrupted
                                    | RunStatus::Cancelled
                            ))
                    && run.active_attempt_id.as_ref() == attempt_id
            }) {
                return Ok(Commit {
                    sequence: latest_sequence(&transaction, None)?,
                    events: vec![],
                    replayed: false,
                });
            }
            if checkpoint_capture {
                let run = projection
                    .runs
                    .iter()
                    .find(|run| run.id == *run_id)
                    .expect("guarded run");
                if events.iter().any(|event| !matches!(&event.payload,
                    EventPayload::CheckpointCaptured(checkpoint)
                        if checkpoint.run_id.as_ref() == Some(run_id)
                            && run.root_node_id.as_ref() == Some(&checkpoint.node_id)
                            && checkpoint.app_run_ordinal == Some(run.ordinal)
                            && projection.checkpoint_scopes.iter().any(|scope|
                                scope.id == checkpoint.scope_id && scope.kind == ScopeKind::RootRun)
                )) {
                    return Err(StoreError::InvalidEvent("checkpoint capture does not match its run".into()));
                }
                if run.checkpoint_id.is_some() {
                    return Ok(Commit {
                        sequence: latest_sequence(&transaction, None)?,
                        events: vec![],
                        replayed: false,
                    });
                }
                events.extend(crate::checkpoint::capture_finalization(
                    &events,
                    &projection.runs,
                    &projection.nodes,
                    now,
                ));
            } else {
                let decision =
                    crate::checkpoint::await_capture(events, &projection.checkpoint_scopes);
                events = decision.events;
                effects = decision.effects;
            }
            if events.iter().any(|event| event.thread_id != thread_id) {
                return Err(StoreError::InvalidEvent(
                    "guarded write spans threads".into(),
                ));
            }
        }
        let mut committed = commit_decision(
            &transaction,
            Decision {
                events,
                effects,
                ..Decision::default()
            },
            None,
            now,
        )?;
        let terminal_threads: BTreeSet<_> = committed
            .iter()
            .filter_map(|event| match &event.event.payload {
                EventPayload::RunUpdated(run) if run.status.is_terminal() => {
                    Some(event.event.thread_id.clone())
                }
                _ => None,
            })
            .collect();
        for thread_id in terminal_threads {
            let projection =
                load_projection(&transaction, &thread_id)?.ok_or(StoreError::ThreadNotFound)?;
            let trigger = committed
                .last()
                .expect("terminal event exists")
                .event
                .id
                .as_str();
            let follow_up = decider::after_terminal(&projection, now, trigger);
            committed.extend(commit_decision(&transaction, follow_up, None, now)?);
        }
        let sequence = latest_sequence(&transaction, None)?;
        transaction.commit()?;
        self.publish(&committed);
        Ok(Commit {
            sequence,
            events: committed,
            replayed: false,
        })
    }
    pub fn recover(&self, now: &Timestamp) -> Result<Commit> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let mut events = vec![];
        for id in thread_ids(&transaction)? {
            let projection =
                load_projection(&transaction, &id)?.ok_or(StoreError::ThreadNotFound)?;
            let sequence = latest_sequence(&transaction, Some(&id))?;
            for (index, payload) in decider::recover(&projection, now).into_iter().enumerate() {
                events.push(DomainEvent {
                    id: EventId::new(format!("event:runtime-reconcile:{id}:{sequence}:{index}"))
                        .map_err(|e| StoreError::InvalidEvent(e.to_string()))?,
                    thread_id: id.clone(),
                    occurred_at: now.clone(),
                    payload,
                });
            }
        }
        let unsettled = read_effects(&transaction)?;
        for effect in unsettled {
            transaction.execute("UPDATE orchestration_v2_effect_outbox SET status=?2,lease_owner=NULL,lease_expires_at=NULL,updated_at=?3 WHERE effect_id=?1", params![effect.id, if effect.body.process_bound() {"cancelled"} else {"pending"}, now.as_str()])?;
        }
        let committed = commit_decision(
            &transaction,
            Decision {
                events,
                ..Decision::default()
            },
            None,
            now,
        )?;
        let sequence = latest_sequence(&transaction, None)?;
        transaction.commit()?;
        self.publish(&committed);
        Ok(Commit {
            sequence,
            events: committed,
            replayed: false,
        })
    }
    pub fn subscribe_thread(
        &self,
        id: &ThreadId,
        after: Option<u64>,
    ) -> Result<Subscription<ThreadStreamItem>> {
        let connection = self.lock()?;
        let receiver = self.committed.subscribe();
        let projection = load_projection(&connection, id)?.ok_or(StoreError::ThreadNotFound)?;
        let sequence = latest_sequence(&connection, Some(id))?;
        let replay = after
            .filter(|after| *after <= sequence)
            .map(|after| {
                query_events(
                    &connection,
                    "stream_id=?1",
                    id.as_str(),
                    after,
                    sequence,
                    129,
                )
            })
            .transpose()?;
        let safe_replay = replay.as_ref().is_some_and(|events| {
            events.len() <= 128
                && !events
                    .iter()
                    .any(|event| matches!(event.event.payload, EventPayload::ThreadCreated(_)))
                && serde_json::to_vec(events).is_ok_and(|bytes| bytes.len() <= 1024 * 1024)
                && postcard::to_allocvec(events).is_ok_and(|bytes| bytes.len() <= 1024 * 1024)
        });
        let mut initial = if safe_replay {
            replay
                .unwrap()
                .into_iter()
                .map(|event| ThreadStreamItem::Event(Box::new(event)))
                .collect()
        } else {
            let (projection, cursor, more, ordinal) = bounded_projection(projection, 200);
            vec![ThreadStreamItem::Snapshot {
                snapshot_sequence: sequence,
                projection: Box::new(projection),
                history_cursor: cursor,
                has_more_history: more,
                latest_local_turn_ordinal: ordinal,
            }]
        };
        initial.push(ThreadStreamItem::Synchronized);
        Ok(Subscription {
            initial,
            receiver,
            cursor: sequence,
        })
    }
    pub fn subscribe_shell(&self, after: Option<u64>) -> Result<Subscription<ShellStreamItem>> {
        let connection = self.lock()?;
        let receiver = self.committed.subscribe();
        let sequence = latest_sequence(&connection, None)?;
        let replay = after
            .filter(|after| *after <= sequence)
            .map(|after| {
                query_events(
                    &connection,
                    "application_event_version=?1",
                    "2",
                    after,
                    sequence,
                    1001,
                )
            })
            .transpose()?;
        let safe = replay.as_ref().is_some_and(|events| {
            events.len() <= 1000
                && serde_json::to_vec(events).is_ok_and(|bytes| bytes.len() <= 8 * 1024 * 1024)
        });
        let mut initial = if safe {
            let mut changed = BTreeSet::new();
            let mut updates = vec![];
            for event in replay.unwrap().iter().rev() {
                if changed.insert(event.event.thread_id.clone()) {
                    updates.push(shell_update(
                        &connection,
                        &event.event.thread_id,
                        event.sequence,
                    )?);
                }
            }
            updates.reverse();
            updates
        } else {
            vec![ShellStreamItem::Snapshot(shell_snapshot(&connection)?)]
        };
        initial.push(ShellStreamItem::Synchronized);
        Ok(Subscription {
            initial,
            receiver,
            cursor: sequence,
        })
    }
    pub fn shell_update(&self, event: &StoredEvent) -> Result<ShellStreamItem> {
        shell_update(&*self.lock()?, &event.event.thread_id, event.sequence)
    }
    pub fn turn_item(&self, id: &ThreadId, item_id: &TurnItemId) -> Result<Option<TurnItem>> {
        let connection = self.lock()?;
        let json: Option<String> = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND turn_item_id=?2", params![id.as_str(), item_id.as_str()], |row| row.get(0)).optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }
    pub fn history(
        &self,
        id: &ThreadId,
        cursor: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<ThreadHistoryPage> {
        if !(1..=200).contains(&limit) {
            return Err(StoreError::InvalidQuery("history limit must be 1..200"));
        }
        let connection = self.lock()?;
        let projection = load_projection(&connection, id)?.ok_or(StoreError::ThreadNotFound)?;
        let candidates: Vec<_> = projection
            .visible_turn_items
            .into_iter()
            .filter(|row| {
                cursor.is_none_or(|cursor| {
                    (row.item.ordinal, &row.item.id) < (cursor.ordinal, &cursor.item_id)
                })
            })
            .collect();
        let has_more = candidates.len() > limit;
        let start = candidates.len().saturating_sub(limit);
        let items: Vec<_> = candidates.into_iter().skip(start).collect();
        let next_cursor = if has_more {
            items.first().map(|row| HistoryCursor {
                ordinal: row.item.ordinal,
                item_id: row.item.id.clone(),
            })
        } else {
            None
        };
        Ok(ThreadHistoryPage {
            snapshot_sequence: latest_sequence(&connection, Some(id))?,
            items,
            next_cursor,
            has_more_history: has_more,
        })
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchMatch>> {
        if !(2..=200).contains(&query.chars().count()) || !(1..=50).contains(&limit) {
            return Err(StoreError::InvalidQuery(
                "search needs 2..200 characters and limit 1..50",
            ));
        }
        let connection = self.lock()?;
        let needle = query.to_lowercase();
        let mut matches = vec![];
        for id in thread_ids(&connection)? {
            let projection =
                load_projection(&connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
            if projection.thread.deleted_at.is_some() {
                continue;
            }
            for message in projection
                .messages
                .iter()
                .rev()
                .filter(|message| matches!(message.role, Role::User | Role::Assistant))
            {
                if message.text.to_lowercase().contains(&needle) {
                    matches.push(SearchMatch {
                        thread_id: id.clone(),
                        project_id: projection.thread.project_id.clone(),
                        source: message.role,
                        snippet: message.text.chars().take(240).collect(),
                        message_created_at: Some(message.created_at.clone()),
                    });
                    break;
                }
            }
        }
        matches.sort_by(|a, b| {
            b.message_created_at
                .cmp(&a.message_created_at)
                .then_with(|| a.thread_id.cmp(&b.thread_id))
        });
        matches.truncate(limit);
        Ok(matches)
    }
    pub fn claim_effect(&self, owner: &str, now_ms: i64) -> Result<Option<ClaimedEffect>> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let candidate: Option<(String, String, u32)> = transaction.query_row(
            "SELECT effect_id,payload_json,attempt_count FROM orchestration_v2_effect_outbox AS candidate WHERE ((status='pending' AND available_at<=?1) OR (status='running' AND lease_expires_at<=?1)) AND NOT EXISTS(SELECT 1 FROM orchestration_v2_effect_outbox AS earlier WHERE earlier.thread_id=candidate.thread_id AND earlier.rowid<candidate.rowid AND earlier.status IN('pending','running')) ORDER BY rowid LIMIT 1", [now_ms], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
        let Some((id, json, count)) = candidate else {
            return Ok(None);
        };
        transaction.execute("UPDATE orchestration_v2_effect_outbox SET status='running',attempt_count=attempt_count+1,lease_owner=?2,lease_expires_at=?3 WHERE effect_id=?1", params![id, owner, now_ms.saturating_add(30_000)])?;
        transaction.commit()?;
        Ok(Some(ClaimedEffect {
            effect: serde_json::from_str(&json)?,
            attempt: count + 1,
            lease_owner: owner.into(),
        }))
    }
    pub fn renew_effect(&self, claim: &ClaimedEffect, now_ms: i64) -> Result<()> {
        let changed = self.lock()?.execute("UPDATE orchestration_v2_effect_outbox SET lease_expires_at=?3 WHERE effect_id=?1 AND lease_owner=?2 AND status='running'", params![claim.effect.id, claim.lease_owner, now_ms.saturating_add(30_000)])?;
        if changed == 1 {
            Ok(())
        } else {
            Err(StoreError::LeaseLost)
        }
    }
    pub fn finish_effect(
        &self,
        claim: &ClaimedEffect,
        error: Option<&str>,
        now_ms: i64,
    ) -> Result<bool> {
        let retry = error.is_some() && claim.attempt < 5;
        let delay = (100_i64 * 2_i64.pow(claim.attempt.saturating_sub(1).min(8))).min(30_000);
        let status = if retry {
            "pending"
        } else if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        let changed = self.lock()?.execute("UPDATE orchestration_v2_effect_outbox SET status=?3,available_at=?4,lease_owner=NULL,lease_expires_at=NULL,completed_at=?5,last_error=?6 WHERE effect_id=?1 AND lease_owner=?2 AND status='running'", params![claim.effect.id, claim.lease_owner, status, now_ms.saturating_add(delay), if retry {None} else {Some(now_ms)}, error])?;
        if changed != 1 {
            return Err(StoreError::LeaseLost);
        }
        Ok(retry)
    }
}

fn kind_name(body: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(body)?;
    match value {
        serde_json::Value::String(value) => Ok(value),
        serde_json::Value::Object(value) => value
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| StoreError::InvalidEvent("empty command".into())),
        _ => Err(StoreError::InvalidEvent("invalid command".into())),
    }
}
fn latest_sequence(connection: &Connection, thread: Option<&ThreadId>) -> Result<u64> {
    Ok(match thread {
        Some(id) => connection.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM orchestration_events WHERE stream_id=?1",
            [id.as_str()],
            |row| row.get(0),
        )?,
        None => connection.query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM orchestration_events",
            [],
            |row| row.get(0),
        )?,
    })
}
fn thread_ids(connection: &Connection) -> Result<Vec<ThreadId>> {
    let mut statement = connection
        .prepare("SELECT thread_id FROM orchestration_v2_projection_threads ORDER BY thread_id")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|id| ThreadId::new(id?).map_err(|error| StoreError::InvalidEvent(error.to_string())))
        .collect()
}
fn records<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    thread_id: &ThreadId,
) -> Result<Vec<T>> {
    let mut statement = connection.prepare(&format!("SELECT payload_json FROM orchestration_v2_projection_{table} WHERE thread_id=?1 ORDER BY rowid"))?;
    statement
        .query_map([thread_id.as_str()], |row| row.get::<_, String>(0))?
        .map(|json| serde_json::from_str(&json?).map_err(Into::into))
        .collect()
}
fn load_projection(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<Option<ThreadProjection>> {
    let row: Option<(String, String)> = connection.query_row("SELECT payload_json,projection_updated_at FROM orchestration_v2_projection_threads WHERE thread_id=?1", [thread_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    let Some((json, updated_at)) = row else {
        return Ok(None);
    };
    let mut projection = ThreadProjection::empty(serde_json::from_str(&json)?);
    projection.updated_at =
        Timestamp::parse(&updated_at).map_err(|e| StoreError::InvalidEvent(e.to_string()))?;
    projection.runs = records(connection, "runs", thread_id)?;
    projection.attempts = records(connection, "run_attempts", thread_id)?;
    projection.nodes = records(connection, "nodes", thread_id)?;
    projection.subagents = records(connection, "subagents", thread_id)?;
    projection.provider_sessions = records(connection, "provider_sessions", thread_id)?;
    projection.provider_threads = records(connection, "provider_threads", thread_id)?;
    projection.provider_turns = records(connection, "provider_turns", thread_id)?;
    projection.runtime_requests = records(connection, "runtime_requests", thread_id)?;
    projection.messages = records(connection, "messages", thread_id)?;
    projection.plans = records(connection, "plans", thread_id)?;
    projection.turn_items = records(connection, "turn_items", thread_id)?;
    projection.checkpoint_scopes = records(connection, "checkpoint_scopes", thread_id)?;
    projection.checkpoints = records(connection, "checkpoints", thread_id)?;
    projection.context_handoffs = records(connection, "context_handoffs", thread_id)?;
    projection.context_transfers = records(connection, "context_transfers", thread_id)?;
    projection.visible_turn_items = projector::visible_items(&projection);
    Ok(Some(projection))
}
fn upsert<T: Serialize>(
    connection: &Connection,
    table: &str,
    key: &str,
    id: &str,
    thread: &ThreadId,
    payload: &T,
) -> Result<()> {
    connection.execute(&format!("INSERT INTO orchestration_v2_projection_{table}({key},thread_id,payload_json) VALUES(?1,?2,?3) ON CONFLICT(thread_id,{key}) DO UPDATE SET payload_json=excluded.payload_json"), params![id, thread.as_str(), serde_json::to_string(payload)?])?;
    Ok(())
}
fn persist_event(connection: &Connection, event: &DomainEvent) -> Result<()> {
    use EventPayload::*;
    let thread_id = &event.thread_id;
    validate_event(event)?;
    if !matches!(event.payload, ThreadCreated(_))
        && !connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_threads WHERE thread_id=?1)",
            [thread_id.as_str()],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(StoreError::ThreadNotFound);
    }
    macro_rules! record {
        ($table:literal, $key:literal, $value:expr) => {
            upsert(
                connection,
                $table,
                $key,
                $value.id.as_str(),
                thread_id,
                $value,
            )?
        };
    }
    match &event.payload {
        ThreadCreated(thread)
        | ThreadArchived(thread)
        | ThreadUnarchived(thread)
        | ThreadDeleted(thread)
        | ThreadSettled(thread)
        | ThreadUnsettled(thread)
        | ThreadSnoozed(thread)
        | ThreadUnsnoozed(thread)
        | ThreadPinned(thread)
        | ThreadUnpinned(thread)
        | ThreadAutoSettleSet(thread)
        | ThreadPinReordered(thread)
        | ThreadActiveReordered(thread)
        | ThreadVisited(thread)
        | ThreadMarkedUnread(thread)
        | ThreadMetadataUpdated(thread)
        | ThreadRuntimeModeUpdated(thread)
        | ThreadInteractionModeUpdated(thread)
        | ThreadModelSelectionUpdated(thread)
        | ThreadProviderSwitched(thread) => {
            if thread.id != *thread_id {
                return Err(StoreError::InvalidEvent(
                    "thread payload does not match envelope".into(),
                ));
            }
            connection.execute("INSERT INTO orchestration_v2_projection_threads(thread_id,project_id,title,archived_at,deleted_at,payload_json,projection_updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(thread_id) DO UPDATE SET project_id=excluded.project_id,title=excluded.title,archived_at=excluded.archived_at,deleted_at=excluded.deleted_at,payload_json=excluded.payload_json", params![thread.id.as_str(), thread.project_id.as_str(), thread.title, thread.archived_at.as_ref().map(Timestamp::as_str), thread.deleted_at.as_ref().map(Timestamp::as_str), serde_json::to_string(thread)?, event.occurred_at.as_str()])?;
        }
        RunCreated(value) | RunUpdated(value) => record!("runs", "run_id", value),
        RunAttemptCreated(value) | RunAttemptUpdated(value) => {
            record!("run_attempts", "attempt_id", value)
        }
        NodeUpdated(value) => record!("nodes", "node_id", value),
        SubagentUpdated(value) => record!("subagents", "subagent_id", value),
        ProviderSessionAttached(value) | ProviderSessionUpdated(value) => {
            record!("provider_sessions", "provider_session_id", value)
        }
        ProviderSessionDetached(value) => {
            connection.execute("DELETE FROM orchestration_v2_projection_provider_sessions WHERE thread_id=?1 AND provider_session_id=?2", params![thread_id.as_str(), value.as_str()])?;
        }
        ProviderThreadUpdated(value) => {
            record!("provider_threads", "provider_thread_id", value);
            let json: String = connection.query_row(
                "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id=?1",
                [thread_id.as_str()],
                |row| row.get(0),
            )?;
            let thread: AppThread = serde_json::from_str(&json)?;
            if let Some(thread) = projector::updated_thread_for_provider(&thread, value) {
                connection.execute("UPDATE orchestration_v2_projection_threads SET payload_json=?2 WHERE thread_id=?1",params![thread_id.as_str(),serde_json::to_string(&thread)?])?;
            }
        }
        ProviderTurnUpdated(value) => {
            let previous: Option<String> = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_provider_turns WHERE thread_id=?1 AND provider_turn_id=?2", params![thread_id.as_str(), value.id.as_str()], |row| row.get(0)).optional()?;
            let previous: Option<ProviderTurn> = previous
                .map(|json| serde_json::from_str(&json))
                .transpose()?;
            let value = projector::updated_provider_turn(previous.as_ref(), value);
            record!("provider_turns", "provider_turn_id", &value);
        }
        RuntimeRequestUpdated(value) => record!("runtime_requests", "runtime_request_id", value),
        MessageUpdated(value) => record!("messages", "message_id", value),
        TurnItemUpdated(value) => record!("turn_items", "turn_item_id", value),
        PlanUpdated(value) => record!("plans", "plan_id", value),
        CheckpointScopeCreated(value) => record!("checkpoint_scopes", "scope_id", value),
        CheckpointCaptured(value) => record!("checkpoints", "checkpoint_id", value),
        ContextHandoffUpdated(value) => record!("context_handoffs", "context_handoff_id", value),
        ContextTransferCreated(value) | ContextTransferUpdated(value) => {
            record!("context_transfers", "context_transfer_id", value)
        }
        CheckpointRollbackRequested(_) => {}
    }
    if !matches!(event.payload, ThreadVisited(_) | ThreadMarkedUnread(_)) {
        connection.execute("UPDATE orchestration_v2_projection_threads SET projection_updated_at=?2 WHERE thread_id=?1", params![thread_id.as_str(), event.occurred_at.as_str()])?;
    }
    Ok(())
}
fn validate_event(event: &DomainEvent) -> Result<()> {
    use EventPayload::*;
    let owner = match &event.payload {
        ThreadCreated(value)
        | ThreadArchived(value)
        | ThreadUnarchived(value)
        | ThreadDeleted(value)
        | ThreadSettled(value)
        | ThreadUnsettled(value)
        | ThreadSnoozed(value)
        | ThreadUnsnoozed(value)
        | ThreadPinned(value)
        | ThreadUnpinned(value)
        | ThreadAutoSettleSet(value)
        | ThreadPinReordered(value)
        | ThreadActiveReordered(value)
        | ThreadVisited(value)
        | ThreadMarkedUnread(value)
        | ThreadMetadataUpdated(value)
        | ThreadRuntimeModeUpdated(value)
        | ThreadInteractionModeUpdated(value)
        | ThreadModelSelectionUpdated(value)
        | ThreadProviderSwitched(value) => Some(&value.id),
        RunCreated(value) | RunUpdated(value) => {
            if value.ordinal == 0 || value.ordinal > (i64::MAX as u64 / 1_000_000 - 1) {
                return Err(StoreError::InvalidEvent("run ordinal out of range".into()));
            }
            Some(&value.thread_id)
        }
        NodeUpdated(value) => Some(&value.thread_id),
        SubagentUpdated(value) => Some(&value.thread_id),
        ProviderThreadUpdated(value) => value.app_thread_id.as_ref(),
        MessageUpdated(value) => Some(&value.thread_id),
        TurnItemUpdated(value) => Some(&value.thread_id),
        PlanUpdated(value) => Some(&value.thread_id),
        CheckpointScopeCreated(value) => Some(&value.thread_id),
        CheckpointCaptured(value) => Some(&value.thread_id),
        ContextHandoffUpdated(value) => Some(&value.thread_id),
        ContextTransferCreated(value) | ContextTransferUpdated(value) => {
            Some(&value.target_thread_id)
        }
        _ => None,
    };
    if owner.is_some_and(|owner| *owner != event.thread_id) {
        return Err(StoreError::InvalidEvent(
            "entity does not belong to event thread".into(),
        ));
    }
    Ok(())
}
fn commit_decision(
    connection: &Connection,
    mut decision: Decision,
    command: Option<&CommandId>,
    now: &Timestamp,
) -> Result<Vec<StoredEvent>> {
    if decision.cancel_unsettled_effects {
        let ids: BTreeSet<_> = decision
            .events
            .iter()
            .map(|event| &event.thread_id)
            .collect();
        for id in ids {
            let deleting = decision.events.iter().any(|event| {
                event.thread_id == *id && matches!(event.payload, EventPayload::ThreadDeleted(_))
            });
            connection.execute("UPDATE orchestration_v2_effect_outbox SET status='cancelled',lease_owner=NULL,lease_expires_at=NULL WHERE thread_id=?1 AND status IN('pending','running') AND (?2 OR effect_type IN('provider-turn.start','provider-turn.restart'))", params![id.as_str(), deleting])?;
        }
    }
    let mut committed = vec![];
    for event in &mut decision.events {
        if let EventPayload::TurnItemUpdated(item) = &mut event.payload {
            normalize_ordinal(connection, &event.thread_id, item)?;
        }
        connection.execute("INSERT INTO orchestration_events(event_id,aggregate_kind,stream_id,stream_version,event_type,occurred_at,command_id,payload_json,application_event_version) VALUES(?1,'thread',?2,(SELECT COALESCE(MAX(stream_version),0)+1 FROM orchestration_events WHERE stream_id=?2),?3,?4,?5,?6,2)", params![event.id.as_str(), event.thread_id.as_str(), event.payload.event_type(), event.occurred_at.as_str(), command.map(CommandId::as_str), serde_json::to_string(event)?])?;
        let sequence = connection.last_insert_rowid() as u64;
        persist_event(connection, event)?;
        connection.execute("UPDATE orchestration_v2_projection_metadata SET last_sequence=?1,updated_at=?2 WHERE projection_name='v2'", params![sequence, now.as_str()])?;
        committed.push(StoredEvent {
            sequence,
            command_id: command.cloned(),
            event: event.clone(),
        });
    }
    for effect in decision.effects {
        let kind = kind_name(&effect.body)?;
        connection.execute("INSERT INTO orchestration_v2_effect_outbox(effect_id,command_id,thread_id,effect_type,payload_json,status,attempt_count,available_at,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'pending',0,0,?6,?6) ON CONFLICT(effect_id) DO NOTHING", params![effect.id, command.map(CommandId::as_str), effect.thread_id.as_str(), kind, serde_json::to_string(&effect)?, now.as_str()])?;
    }
    Ok(committed)
}
fn normalize_ordinal(
    connection: &Connection,
    thread_id: &ThreadId,
    item: &mut TurnItem,
) -> Result<()> {
    let old: Option<u64> = connection.query_row("SELECT ordinal FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND turn_item_id=?2", params![thread_id.as_str(), item.id.as_str()], |row| row.get(0)).optional()?;
    if let Some(ordinal) = old {
        item.ordinal = ordinal;
        return Ok(());
    }
    let run_ordinal: u64 = match &item.run_id {
        Some(id) => {
            let json: String = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND run_id=?2", params![thread_id.as_str(), id.as_str()], |row| row.get(0))?;
            serde_json::from_str::<Run>(&json)?.ordinal
        }
        None => 0,
    };
    let band = run_ordinal
        .checked_mul(1_000_000)
        .ok_or_else(|| StoreError::InvalidEvent("turn ordinal overflow".into()))?;
    let max: u64 = connection.query_row("SELECT COALESCE(MAX(ordinal),?2) FROM orchestration_v2_turn_item_positions WHERE thread_id=?1 AND ordinal>=?2 AND ordinal<?3", params![thread_id.as_str(), band, band + 1_000_000], |row| row.get(0))?;
    item.ordinal = max + 1;
    if item.ordinal >= band + 1_000_000 {
        return Err(StoreError::InvalidEvent("turn item band exhausted".into()));
    }
    connection.execute("INSERT INTO orchestration_v2_turn_item_positions(thread_id,turn_item_id,ordinal) VALUES(?1,?2,?3)", params![thread_id.as_str(), item.id.as_str(), item.ordinal])?;
    Ok(())
}
fn query_events(
    connection: &Connection,
    predicate: &str,
    key: &str,
    after: u64,
    through: u64,
    limit: usize,
) -> Result<Vec<StoredEvent>> {
    let mut statement = connection.prepare(&format!("SELECT sequence,command_id,payload_json FROM orchestration_events WHERE {predicate} AND sequence>?2 AND sequence<=?3 ORDER BY sequence LIMIT ?4"))?;
    statement
        .query_map(
            params![
                key,
                after.min(i64::MAX as u64),
                through.min(i64::MAX as u64),
                limit.min(i64::MAX as usize)
            ],
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?
        .map(|row| {
            let (sequence, command_id, json) = row?;
            Ok(StoredEvent {
                sequence,
                command_id: command_id
                    .map(CommandId::new)
                    .transpose()
                    .map_err(|e| StoreError::InvalidEvent(e.to_string()))?,
                event: serde_json::from_str(&json)?,
            })
        })
        .collect()
}
fn shell_snapshot(connection: &Connection) -> Result<ShellSnapshot> {
    let mut threads = vec![];
    let mut archived_threads = vec![];
    for id in thread_ids(connection)? {
        let projection = load_projection(connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
        if projection.thread.deleted_at.is_some() {
            continue;
        }
        let shell = projector::shell(&projection);
        if projection.thread.archived_at.is_some() {
            archived_threads.push(shell);
        } else {
            threads.push(shell);
        }
    }
    Ok(ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: latest_sequence(connection, None)?,
        threads,
        archived_threads,
    })
}
fn shell_update(
    connection: &Connection,
    thread_id: &ThreadId,
    sequence: u64,
) -> Result<ShellStreamItem> {
    let projection = load_projection(connection, thread_id)?;
    match projection {
        Some(projection) if projection.thread.deleted_at.is_none() => {
            Ok(ShellStreamItem::ThreadUpdated {
                sequence,
                archived: projection.thread.archived_at.is_some(),
                thread: Box::new(projector::shell(&projection)),
            })
        }
        _ => Ok(ShellStreamItem::ThreadRemoved {
            sequence,
            thread_id: thread_id.clone(),
        }),
    }
}
fn read_effects(connection: &Connection) -> Result<Vec<Effect>> {
    let mut statement = connection.prepare("SELECT payload_json FROM orchestration_v2_effect_outbox WHERE status IN('pending','running') ORDER BY rowid")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|json| serde_json::from_str(&json?).map_err(Into::into))
        .collect()
}
fn bounded_projection(
    mut projection: ThreadProjection,
    limit: usize,
) -> (ThreadProjection, Option<HistoryCursor>, bool, Option<u64>) {
    let ordinal = projection
        .visible_turn_items
        .iter()
        .filter(|row| row.visibility == Visibility::Local)
        .map(|row| row.item.ordinal)
        .max();
    let more = projection.visible_turn_items.len() > limit;
    if more {
        projection
            .visible_turn_items
            .drain(..projection.visible_turn_items.len() - limit);
        let ids: BTreeSet<_> = projection
            .visible_turn_items
            .iter()
            .map(|row| &row.source_item_id)
            .collect();
        projection.turn_items.retain(|item| ids.contains(&item.id));
        let message_ids: BTreeSet<_> = projection
            .turn_items
            .iter()
            .filter_map(|item| match &item.body {
                TurnItemBody::UserMessage { message_id, .. }
                | TurnItemBody::AssistantMessage { message_id, .. } => Some(message_id),
                _ => None,
            })
            .collect();
        projection
            .messages
            .retain(|message| message_ids.contains(&message.id));
    }
    let cursor = if more {
        projection
            .visible_turn_items
            .first()
            .map(|row| HistoryCursor {
                ordinal: row.item.ordinal,
                item_id: row.source_item_id.clone(),
            })
    } else {
        None
    };
    (projection, cursor, more, ordinal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    fn setup() -> Store {
        let store = Store::memory().unwrap();
        store
            .dispatch(&create(), &now(), &turns(), Driver::Codex)
            .unwrap();
        store
    }
    fn dispatch(store: &Store, command: &Command) -> Commit {
        store
            .dispatch(command, &now(), &turns(), Driver::Codex)
            .unwrap()
    }
    #[test]
    fn receipts_replay_without_appending_or_publishing_twice() {
        let store = setup();
        let mut shell = store.subscribe_shell(None).unwrap();
        let input = send("one", DispatchMode::StartImmediately);
        let first = dispatch(&store, &input);
        let sequence = store.sequence().unwrap();
        let replay = dispatch(&store, &input);
        assert_eq!(replay.sequence, first.sequence);
        assert!(replay.replayed);
        assert_eq!(replay.events, first.events);
        assert_eq!(store.sequence().unwrap(), sequence);
        let mut count = 0;
        while shell.receiver.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, first.events.len());
    }
    #[test]
    fn command_ids_conflict_across_threads_even_for_rejections() {
        let store = setup();
        let mut duplicate = create();
        duplicate.thread_id = ThreadId::new("other").unwrap();
        assert!(matches!(
            store.dispatch(&duplicate, &now(), &turns(), Driver::Codex),
            Err(StoreError::CommandConflict)
        ));
        let input = command("bad", CommandBody::ThreadMarkUnread);
        assert!(matches!(
            store.dispatch(&input, &now(), &turns(), Driver::Codex),
            Err(StoreError::Decision(_))
        ));
        assert!(matches!(
            store.dispatch(&input, &now(), &turns(), Driver::Codex),
            Err(StoreError::PreviouslyRejected(_))
        ));
        assert_eq!(store.sequence().unwrap(), 1);
    }
    #[test]
    fn event_projection_failure_rolls_back_the_entire_batch() {
        let store = setup();
        let before = store.projection(&create().thread_id).unwrap();
        let mut thread = before.thread.clone();
        thread.title = "Not committed".into();
        let first = DomainEvent {
            id: EventId::new("valid").unwrap(),
            thread_id: thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::ThreadMetadataUpdated(thread.clone()),
        };
        thread.id = ThreadId::new("wrong").unwrap();
        let second = DomainEvent {
            id: EventId::new("invalid").unwrap(),
            thread_id: before.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::ThreadMetadataUpdated(thread),
        };
        assert!(store.ingest(vec![first, second], None, &now()).is_err());
        assert_eq!(store.sequence().unwrap(), 1);
        assert_eq!(store.projection(&before.thread.id).unwrap(), before);
    }
    #[test]
    fn effects_are_serial_per_thread_and_backoff_blocks_later_effects() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        let first = store.claim_effect("worker-a", 1000).unwrap().unwrap();
        assert!(store.claim_effect("worker-b", 1000).unwrap().is_none());
        assert!(
            store
                .finish_effect(&first, Some("temporary"), 1000)
                .unwrap()
        );
        assert!(store.claim_effect("worker-b", 1099).unwrap().is_none());
        let second = store.claim_effect("worker-b", 1100).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert!(matches!(
            store.finish_effect(&first, None, 1100),
            Err(StoreError::LeaseLost)
        ));
        assert!(!store.finish_effect(&second, None, 1100).unwrap());
    }
    #[test]
    fn crash_recovery_cancels_provider_io_and_preserves_held_queue() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(file.path()).unwrap();
        dispatch(&store, &create());
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        assert!(store.claim_effect("lost-process", 0).unwrap().is_some());
        drop(store);
        let store = Store::open(file.path()).unwrap();
        store.recover(&now()).unwrap();
        let projection = store.projection(&create().thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Interrupted);
        assert!(projection.runs[1].queue_held);
        assert!(
            store
                .claim_effect("new-process", 100_000)
                .unwrap()
                .is_none()
        );
        dispatch(&store, &command("resume", CommandBody::QueueResume));
        assert_eq!(
            store.projection(&create().thread_id).unwrap().runs[1].status,
            RunStatus::Starting
        );
        assert!(
            store
                .claim_effect("new-process", 100_000)
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn snapshots_replays_and_live_writes_do_not_leave_a_sequence_gap() {
        let store = setup();
        let thread_id = create().thread_id;
        let mut stream = store.subscribe_thread(&thread_id, None).unwrap();
        assert!(matches!(
            stream.initial[0],
            ThreadStreamItem::Snapshot { .. }
        ));
        assert!(matches!(stream.initial[1], ThreadStreamItem::Synchronized));
        let commit = dispatch(&store, &send("one", DispatchMode::StartImmediately));
        assert_eq!(stream.receiver.try_recv().unwrap().sequence, 2);
        let replay = store.subscribe_thread(&thread_id, Some(1)).unwrap();
        assert_eq!(replay.initial.len(), commit.events.len() + 1);
        assert!(matches!(replay.initial[0], ThreadStreamItem::Event(_)));
        assert!(matches!(
            store
                .subscribe_thread(&thread_id, Some(9999))
                .unwrap()
                .initial[0],
            ThreadStreamItem::Snapshot { .. }
        ));
        assert!(matches!(
            store.subscribe_thread(&thread_id, Some(0)).unwrap().initial[0],
            ThreadStreamItem::Snapshot { .. }
        ));
    }
    #[test]
    fn stale_attempt_events_cannot_finish_a_restarted_run() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        let projection = store.projection(&create().thread_id).unwrap();
        let run = &projection.runs[0];
        let mut finished = run.clone();
        finished.status = RunStatus::Completed;
        let event = DomainEvent {
            id: EventId::new("stale").unwrap(),
            thread_id: projection.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::RunUpdated(finished),
        };
        let wrong = RunAttemptId::new("wrong-attempt").unwrap();
        assert!(
            store
                .ingest(vec![event], Some((&run.id, Some(&wrong))), &now())
                .unwrap()
                .events
                .is_empty()
        );
        assert_eq!(
            store.projection(&projection.thread.id).unwrap().runs[0].status,
            RunStatus::Starting
        );
    }
    #[test]
    fn terminal_event_promotes_the_next_queue_entry_atomically() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        let projection = store.projection(&create().thread_id).unwrap();
        let mut run = projection.runs[0].clone();
        run.status = RunStatus::Completed;
        run.completed_at = Some(now());
        let event = DomainEvent {
            id: EventId::new("done").unwrap(),
            thread_id: projection.thread.id.clone(),
            occurred_at: now(),
            payload: EventPayload::RunUpdated(run),
        };
        let commit = store.ingest(vec![event], None, &now()).unwrap();
        assert_eq!(commit.events.len(), 2);
        assert_eq!(
            store.projection(&projection.thread.id).unwrap().runs[1].status,
            RunStatus::Starting
        );
    }
    #[test]
    fn stopping_a_waiting_run_keeps_one_capture_and_holds_the_queue() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        let run = store.projection(&create().thread_id).unwrap().runs[0].clone();
        let event = |id: &str, payload| DomainEvent {
            id: EventId::new(id).unwrap(),
            thread_id: run.thread_id.clone(),
            occurred_at: now(),
            payload,
        };
        store
            .ingest(
                vec![event(
                    "scope",
                    EventPayload::CheckpointScopeCreated(checkpoint_scope(&run)),
                )],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let mut finished = run.clone();
        finished.status = RunStatus::Completed;
        store
            .ingest(
                vec![event("finish", EventPayload::RunUpdated(finished))],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        dispatch(
            &store,
            &command(
                "stop",
                CommandBody::RunInterrupt {
                    run_id: run.id.clone(),
                    reason: None,
                    hold_queue: true,
                },
            ),
        );
        let effects = read_effects(&store.lock().unwrap()).unwrap();
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect.body, EffectBody::CaptureCheckpoint { .. }))
                .count(),
            1
        );
        store
            .ingest_checkpoint(
                vec![event(
                    "capture",
                    EventPayload::CheckpointCaptured(checkpoint(&run, CheckpointStatus::Ready)),
                )],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let projection = store.projection(&run.thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Interrupted);
        assert!(projection.runs[1].queue_held);
        assert_eq!(projection.runs[1].status, RunStatus::Queued);
    }
    #[test]
    fn checkpoint_commit_completes_run_and_promotes_queue_exactly_once() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        let projection = store.projection(&create().thread_id).unwrap();
        let run = projection.runs[0].clone();
        let event = |id: &str, payload| DomainEvent {
            id: EventId::new(id).unwrap(),
            thread_id: run.thread_id.clone(),
            occurred_at: now(),
            payload,
        };
        store
            .ingest(
                vec![event(
                    "scope",
                    EventPayload::CheckpointScopeCreated(checkpoint_scope(&run)),
                )],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let mut finished = run.clone();
        finished.status = RunStatus::Completed;
        store
            .ingest(
                vec![event("finish", EventPayload::RunUpdated(finished))],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let projection = store.projection(&run.thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Waiting);
        assert_eq!(projection.runs[1].status, RunStatus::Queued);
        let captured = event(
            "capture",
            EventPayload::CheckpointCaptured(checkpoint(&run, CheckpointStatus::Ready)),
        );
        let wrong = RunAttemptId::new("stale-attempt").unwrap();
        assert!(
            store
                .ingest_checkpoint(
                    vec![captured.clone()],
                    Some((&run.id, Some(&wrong))),
                    &now()
                )
                .unwrap()
                .events
                .is_empty()
        );
        let commit = store
            .ingest_checkpoint(
                vec![captured.clone()],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        assert_eq!(commit.events.len(), 5);
        let projection = store.projection(&run.thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Completed);
        assert!(projection.runs[0].checkpoint_id.is_some());
        assert_eq!(projection.runs[1].status, RunStatus::Starting);
        assert!(
            store
                .ingest_checkpoint(
                    vec![captured],
                    Some((&run.id, run.active_attempt_id.as_ref())),
                    &now()
                )
                .unwrap()
                .events
                .is_empty()
        );
        assert_eq!(store.sequence().unwrap(), commit.sequence);
    }
    #[test]
    fn process_loss_retains_capture_without_resuming_queued_work() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        let run = store.projection(&create().thread_id).unwrap().runs[0].clone();
        let mut finished = run.clone();
        finished.status = RunStatus::Completed;
        store
            .ingest(
                vec![DomainEvent {
                    id: EventId::new("scope").unwrap(),
                    thread_id: run.thread_id.clone(),
                    occurred_at: now(),
                    payload: EventPayload::CheckpointScopeCreated(checkpoint_scope(&run)),
                }],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        store
            .ingest(
                vec![DomainEvent {
                    id: EventId::new("finish").unwrap(),
                    thread_id: run.thread_id.clone(),
                    occurred_at: now(),
                    payload: EventPayload::RunUpdated(finished),
                }],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        store.recover(&now()).unwrap();
        let effect = store.claim_effect("recovered", 100_000).unwrap().unwrap();
        assert!(matches!(
            effect.effect.body,
            EffectBody::CaptureCheckpoint { .. }
        ));
        store
            .ingest_checkpoint(
                vec![DomainEvent {
                    id: EventId::new("capture").unwrap(),
                    thread_id: run.thread_id.clone(),
                    occurred_at: now(),
                    payload: EventPayload::CheckpointCaptured(checkpoint(
                        &run,
                        CheckpointStatus::Ready,
                    )),
                }],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let projection = store.projection(&run.thread_id).unwrap();
        assert_eq!(projection.runs[0].status, RunStatus::Interrupted);
        assert_eq!(projection.runs[1].status, RunStatus::Queued);
        assert!(projection.runs[1].queue_held);
        let mut invalid = checkpoint(&run, CheckpointStatus::Ready);
        invalid.node_id = NodeId::new("different-root").unwrap();
        assert!(matches!(
            store.ingest_checkpoint(
                vec![DomainEvent {
                    id: EventId::new("invalid").unwrap(),
                    thread_id: run.thread_id,
                    occurred_at: now(),
                    payload: EventPayload::CheckpointCaptured(invalid)
                }],
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now()
            ),
            Err(StoreError::InvalidEvent(_))
        ));
    }
}
