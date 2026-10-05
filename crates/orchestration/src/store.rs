//! A single transaction owns receipts, events, projections and the effect outbox.
//! The publish lane is the same lock: subscribers cannot observe a commit gap.
use crate::{contracts::*, decider, projector};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    shell_cache: Mutex<BTreeMap<ThreadId, (u64, Option<ThreadShell>)>>,
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
            shell_cache: Mutex::new(BTreeMap::new()),
        })
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
    pub fn instance_id(&self) -> Result<String> {
        Ok(self.lock()?.query_row(
            "SELECT value FROM orchestration_host_metadata WHERE key='instance'",
            [],
            |row| row.get(0),
        )?)
    }
    pub fn import_completed(&self, driver: Driver) -> Result<bool> {
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestration_host_metadata WHERE key=?1)",
            [format!("import:{}", driver.as_str())],
            |row| row.get(0),
        )?)
    }
    pub fn complete_import(&self, driver: Driver) -> Result<()> {
        self.lock()?.execute(
            "INSERT OR IGNORE INTO orchestration_host_metadata(key,value) VALUES(?1,'completed')",
            [format!("import:{}", driver.as_str())],
        )?;
        Ok(())
    }
    pub fn native_session_registered(&self, driver: Driver, native: &str) -> Result<bool> {
        Ok(self.lock()?.query_row("SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_provider_threads WHERE json_extract(payload_json,'$.driver')=?1 AND json_extract(payload_json,'$.nativeThreadRef.nativeId')=?2)", params![driver.as_str(), native], |row| row.get(0))?)
    }
    pub(crate) fn subscribe_commits(&self) -> broadcast::Receiver<StoredEvent> {
        self.committed.subscribe()
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
        self.dispatch_inner(command, now, capabilities, driver, None)
    }
    pub fn steer_follow_up(&self, effect: &Effect, now: &Timestamp) -> Result<Commit> {
        let EffectBody::Steer { message_id, .. } = &effect.body else {
            return Err(StoreError::InvalidEvent("not a steer effect".into()));
        };
        let projection = self.projection(&effect.thread_id)?;
        let message = projection
            .messages
            .iter()
            .find(|m| m.id == *message_id)
            .ok_or(StoreError::InvalidEvent("steer message missing".into()))?;
        let driver = crate::capabilities::driver(&projection.thread.provider_instance_id)
            .ok_or_else(|| StoreError::InvalidEvent("unknown steer provider".into()))?;
        let capabilities = crate::capabilities::capabilities(driver).turns;
        let command = Command {
            command_id: CommandId::new(format!("command:steer-follow-up:{}", effect.id))
                .expect("derived id"),
            thread_id: effect.thread_id.clone(),
            body: CommandBody::MessageDispatch(MessageDispatch {
                delegated_completion: message.delegated_completion.clone(),
                source_plan_ref: None,
                created_by: message.created_by,
                creation_source: message.creation_source,
                message_id: message.id.clone(),
                text: message.text.clone(),
                context: message.context.clone(),
                attachments: message.attachments.clone(),
                model_selection: None,
                delivery_intent: None,
                dispatch_mode: DispatchMode::QueueAfterActive,
            }),
        };
        self.dispatch_inner(&command, now, &capabilities, driver, Some(message_id))
    }
    fn dispatch_inner(
        &self,
        command: &Command,
        now: &Timestamp,
        capabilities: &TurnCapabilities,
        driver: Driver,
        reused_message: Option<&MessageId>,
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
        let mut projection = load_projection(&transaction, &command.thread_id)?;
        if let (Some(projection), Some(message_id)) = (&mut projection, reused_message) {
            projection
                .messages
                .retain(|message| message.id != *message_id);
        }
        let planned = if let CommandBody::ThreadFork {
            target_thread_id, ..
        }
        | CommandBody::ThreadMergeBack {
            target_thread_id, ..
        } = &command.body
        {
            let target = load_projection(&transaction, target_thread_id)?;
            crate::context::plan(command, projection.as_ref(), target.as_ref(), now)
        } else {
            decider::decide(command, projection.as_ref(), now, capabilities, driver)
        };
        let planned = if let CommandBody::MessageDispatch(message) = &command.body
            && let Some(reference) = &message.source_plan_ref
        {
            let source = load_projection(&transaction, &reference.thread_id)?;
            planned.and_then(|mut decision| {
                let source = source
                    .as_ref()
                    .ok_or_else(|| decider::DecisionError("source plan thread missing".into()))?;
                if projection
                    .as_ref()
                    .is_none_or(|target| target.thread.project_id != source.thread.project_id)
                {
                    return Err(decider::DecisionError(
                        "source plan belongs to another project".into(),
                    ));
                }
                let mut plan = source
                    .plans
                    .iter()
                    .find(|plan| {
                        plan.id == reference.plan_id
                            && plan.status == PlanStatus::Active
                            && matches!(plan.body, PlanBody::ProposedPlan { .. })
                    })
                    .ok_or_else(|| {
                        decider::DecisionError("source proposed plan is not active".into())
                    })?
                    .clone();
                plan.status = PlanStatus::Completed;
                decision.events.extend(crate::events(
                    &reference.thread_id,
                    &format!("{}:source-plan", command.command_id),
                    vec![EventPayload::PlanUpdated(plan)],
                    now,
                ));
                Ok(decision)
            })
        } else {
            planned
        };
        let decision = match planned {
            Ok(decision) => decision,
            Err(error) => {
                transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,error,command_type) VALUES(?1,'thread',?2,?3,0,'rejected',?4,?5)", params![command.command_id.as_str(), command.thread_id.as_str(), now.as_str(), error.to_string(), kind_name(&command.body)?])?;
                transaction.commit()?;
                return Err(StoreError::Decision(error));
            }
        };
        transaction.execute("INSERT INTO orchestration_command_receipts(command_id,aggregate_kind,aggregate_id,accepted_at,result_sequence,status,command_type) VALUES(?1,'thread',?2,?3,0,'accepted',?4)", params![command.command_id.as_str(), command.thread_id.as_str(), now.as_str(), kind_name(&command.body)?])?;
        let inherited = decision
            .events
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::ContextTransferCreated(transfer)
                    if transfer.kind == TransferKind::Fork =>
                {
                    let source = projection.as_ref()?;
                    let ordinal = source
                        .runs
                        .iter()
                        .find(|r| Some(&r.id) == transfer.source_point.run_id.as_ref())?
                        .ordinal;
                    let rows: Vec<_> = source
                        .visible_turn_items
                        .iter()
                        .filter(|row| {
                            row.visibility == Visibility::Inherited
                                || row.item.run_id.as_ref().is_none_or(|id| {
                                    source
                                        .runs
                                        .iter()
                                        .any(|r| &r.id == id && r.ordinal <= ordinal)
                                })
                        })
                        .cloned()
                        .map(|mut row| {
                            row.visibility = Visibility::Inherited;
                            row
                        })
                        .collect();
                    Some((transfer.target_thread_id.clone(), rows))
                }
                _ => None,
            });
        let mut events = commit_decision(&transaction, decision, Some(&command.command_id), now)?;
        if matches!(
            command.body,
            CommandBody::DelegatedTaskRequest(_)
                | CommandBody::DelegatedTaskWakePolicy { .. }
                | CommandBody::DelegatedTaskAcknowledge { .. }
                | CommandBody::DelegatedTaskDispose { .. }
                | CommandBody::RunInterrupt { .. }
                | CommandBody::QueueResume
                | CommandBody::MessageDispatch(_)
                | CommandBody::ThreadDelete
        ) {
            events.extend(reconcile_delegations(
                &transaction,
                &command.thread_id,
                now,
            )?);
        }
        if let Some((thread, rows)) = inherited {
            transaction.execute("INSERT INTO orchestration_v2_projection_fork_history(thread_id,payload_json) VALUES(?1,?2)", params![thread.as_str(), serde_json::to_string(&rows)?])?;
        }
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
    pub fn ingest_rollback(
        &self,
        mut events: Vec<DomainEvent>,
        request_id: &CommandId,
        now: &Timestamp,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let thread_id = events
            .first()
            .ok_or_else(|| StoreError::InvalidEvent("empty rollback write".into()))?
            .thread_id
            .clone();
        let projection =
            load_projection(&transaction, &thread_id)?.ok_or(StoreError::ThreadNotFound)?;
        if projection.thread.rollback_request_id.as_ref() != Some(request_id) {
            return Ok(Commit {
                sequence: latest_sequence(&transaction, None)?,
                events: vec![],
                replayed: false,
            });
        }
        // Rollback owns only these fields. Metadata may have changed while
        // the provider and filesystem operations were running.
        for event in &mut events {
            if let EventPayload::ThreadMetadataUpdated(completed) = &mut event.payload {
                let mut current = projection.thread.clone();
                current.rollback_request_id = completed.rollback_request_id.clone();
                current.rollback_failure = completed.rollback_failure.clone();
                current.updated_at = now.clone();
                *completed = current;
            }
        }
        let mut stored = commit_decision(
            &transaction,
            Decision {
                events,
                ..Decision::default()
            },
            None,
            now,
        )?;
        stored.extend(reconcile_delegations(&transaction, &thread_id, now)?);
        let sequence = latest_sequence(&transaction, None)?;
        transaction.commit()?;
        self.publish(&stored);
        Ok(Commit {
            sequence,
            events: stored,
            replayed: false,
        })
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
            let needs_history = checkpoint_capture
                || events.iter().any(|e| {
                    matches!(
                        e.payload,
                        EventPayload::PlanUpdated(_)
                            | EventPayload::ProviderTurnUpdated(_)
                            | EventPayload::RunUpdated(_)
                    )
                });
            let projection = if needs_history {
                load_projection(&transaction, &thread_id)?.ok_or(StoreError::ThreadNotFound)?
            } else {
                // A stream delta needs only its current run guard, not every
                // historical message, tool output and inherited timeline item.
                let json: String = transaction.query_row("SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id=?1", [thread_id.as_str()], |row| row.get(0)).optional()?.ok_or(StoreError::ThreadNotFound)?;
                let mut projection = ThreadProjection::empty(serde_json::from_str(&json)?);
                let run: Option<String> = transaction.query_row("SELECT payload_json FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND run_id=?2", params![thread_id.as_str(), run_id.as_str()], |row| row.get(0)).optional()?;
                if let Some(json) = run {
                    projection.runs.push(serde_json::from_str(&json)?);
                }
                projection
            };
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
            let mut plans = projection.plans.clone();
            let mut ordered = vec![];
            for mut event in events {
                if let EventPayload::RunUpdated(run) = &mut event.payload
                    && let Some(previous) = projection.runs.iter().find(|r| r.id == run.id)
                {
                    run.delegated_completion = previous.delegated_completion.clone();
                }
                if let EventPayload::PlanUpdated(plan) = &event.payload {
                    if matches!(plan.status, PlanStatus::Draft | PlanStatus::Active) {
                        for old in plans.iter_mut().filter(|old| {
                            old.id != plan.id
                                && std::mem::discriminant(&old.body)
                                    == std::mem::discriminant(&plan.body)
                                && matches!(old.status, PlanStatus::Draft | PlanStatus::Active)
                        }) {
                            old.status = PlanStatus::Superseded;
                            ordered.extend(crate::events(
                                &thread_id,
                                &format!("{}:superseded:{}", event.id, old.id),
                                vec![EventPayload::PlanUpdated(old.clone())],
                                now,
                            ));
                        }
                    }
                    if let Some(old) = plans.iter_mut().find(|old| old.id == plan.id) {
                        *old = plan.clone();
                    } else {
                        plans.push(plan.clone());
                    }
                }
                ordered.push(event);
            }
            events = ordered;
            if events.iter().any(|e| matches!(&e.payload, EventPayload::ProviderTurnUpdated(t) if t.status == TurnStatus::Running)) {
                if let Some(message) = projection.runs.iter().find(|r|r.id==*run_id).and_then(|r|projection.messages.iter().find(|m|m.id==r.user_message_id && m.delegated_completion.is_some())) {
                    let command=Command{command_id:CommandId::new(format!("notification-accepted:{}:{}",run_id,events.first().expect("guarded events").id)).expect("derived id"),thread_id:thread_id.clone(),body:CommandBody::NotificationDeliveryAccept{message_id:message.id.clone()}};
                    events.extend(crate::delegation::update(&command,&projection,now)?.events);
                }

                let payloads = crate::context::consumed(&projection.context_transfers, run_id, now);
                let trigger = events.first().expect("guarded events").id.to_string();
                events.extend(crate::events(&thread_id, &format!("{trigger}:consume"), payloads, now));
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
            committed.extend(reconcile_delegations(&transaction, &thread_id, now)?);
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
        let unsettled = read_effects(&transaction)?;
        for id in thread_ids(&transaction)? {
            let projection =
                load_projection(&transaction, &id)?.ok_or(StoreError::ThreadNotFound)?;
            let sequence = latest_sequence(&transaction, Some(&id))?;
            let captures = unsettled
                .iter()
                .filter_map(|effect| match &effect.body {
                    EffectBody::CaptureCheckpoint { run_id } if effect.thread_id == id => {
                        Some(run_id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            for (index, payload) in decider::recover(&projection, &captures, now)
                .into_iter()
                .enumerate()
            {
                events.push(DomainEvent {
                    id: EventId::new(format!("event:runtime-reconcile:{id}:{sequence}:{index}"))
                        .map_err(|e| StoreError::InvalidEvent(e.to_string()))?,
                    thread_id: id.clone(),
                    occurred_at: now.clone(),
                    payload,
                });
            }
        }
        for effect in unsettled {
            transaction.execute("UPDATE orchestration_v2_effect_outbox SET status=?2,lease_owner=NULL,lease_expires_at=NULL,updated_at=?3 WHERE effect_id=?1", params![effect.id, if effect.body.process_bound() {"cancelled"} else {"pending"}, now.as_str()])?;
        }
        let mut committed = commit_decision(
            &transaction,
            Decision {
                events,
                ..Decision::default()
            },
            None,
            now,
        )?;
        for id in thread_ids(&transaction)? {
            committed.extend(reconcile_delegations_with_offer(
                &transaction,
                &id,
                now,
                false,
            )?);
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
        let connection = self.lock()?;
        let mut cache = self.shell_cache.lock().map_err(|_| StoreError::Poisoned)?;
        let latest = latest_sequence(&connection, Some(&event.event.thread_id))?;
        let id = &event.event.thread_id;
        if cache
            .get(id)
            .is_none_or(|(sequence, _)| *sequence != latest)
        {
            let streamed = match cache.get(id) {
                Some((after, Some(previous))) => {
                    streaming_shell(&connection, id, previous, *after, latest)?
                }
                _ => None,
            };
            let shell = match streamed {
                Some(shell) => Some(shell),
                None => load_projection(&connection, id)?
                    .filter(|p| p.thread.deleted_at.is_none())
                    .map(|p| projector::shell(&p)),
            };
            cache.insert(id.clone(), (latest, shell));
        }
        // Replay sequences stay at the original event cursor. A cached record
        // must not move a device past changes to other threads in that replay.
        Ok(match &cache[id].1 {
            Some(shell) => ShellStreamItem::ThreadUpdated {
                sequence: event.sequence,
                archived: shell.thread.archived_at.is_some(),
                thread: Box::new(shell.clone()),
            },
            None => ShellStreamItem::ThreadRemoved {
                sequence: event.sequence,
                thread_id: id.clone(),
            },
        })
    }
    pub fn turn_item(&self, id: &ThreadId, item_id: &TurnItemId) -> Result<Option<TurnItem>> {
        let connection = self.lock()?;
        let json: Option<String> = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND turn_item_id=?2", params![id.as_str(), item_id.as_str()], |row| row.get(0)).optional()?;
        if let Some(json) = json {
            return Ok(Some(serde_json::from_str(&json)?));
        }
        Ok(load_projection(&connection, id)?.and_then(|p| {
            p.visible_turn_items
                .into_iter()
                .find(|row| row.source_item_id == *item_id)
                .map(|row| row.item)
        }))
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
            .filter(|row| cursor.is_none_or(|cursor| row.position < cursor.position))
            .collect();
        let has_more = candidates.len() > limit;
        let start = candidates.len().saturating_sub(limit);
        let items: Vec<_> = candidates.into_iter().skip(start).collect();
        let next_cursor = if has_more {
            items.first().map(|row| HistoryCursor {
                position: row.position,
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
        let effect = serde_json::from_str(&json)?;
        transaction.execute("UPDATE orchestration_v2_effect_outbox SET status='running',attempt_count=attempt_count+1,lease_owner=?2,lease_expires_at=?3 WHERE effect_id=?1", params![id, owner, now_ms.saturating_add(30_000)])?;
        transaction.commit()?;
        Ok(Some(ClaimedEffect {
            effect,
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
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        let changed=transaction.execute("UPDATE orchestration_v2_effect_outbox SET status=?3,available_at=?4,lease_owner=NULL,lease_expires_at=NULL,completed_at=?5,last_error=?6 WHERE effect_id=?1 AND lease_owner=?2 AND status='running'", params![claim.effect.id, claim.lease_owner, status, now_ms.saturating_add(delay), if retry {None} else {Some(now_ms)}, error])?;
        if changed != 1 {
            return Err(StoreError::LeaseLost);
        }
        let mut committed = vec![];
        if error.is_none()
            && let EffectBody::Steer { message_id, .. } = &claim.effect.body
        {
            let projection = load_projection(&transaction, &claim.effect.thread_id)?
                .ok_or(StoreError::ThreadNotFound)?;
            if projection
                .messages
                .iter()
                .any(|m| m.id == *message_id && m.delegated_completion.is_some())
            {
                let timestamp = Timestamp::from_millis(now_ms)
                    .map_err(|e| StoreError::InvalidEvent(e.to_string()))?;
                let command = Command {
                    command_id: CommandId::new(format!("command:accepted:{}", claim.effect.id))
                        .expect("derived id"),
                    thread_id: claim.effect.thread_id.clone(),
                    body: CommandBody::NotificationDeliveryAccept {
                        message_id: message_id.clone(),
                    },
                };
                committed = commit_decision(
                    &transaction,
                    crate::delegation::update(&command, &projection, &timestamp)?,
                    None,
                    &timestamp,
                )?;
            }
        }
        transaction.commit()?;
        self.publish(&committed);
        Ok(retry)
    }
}
// Streaming changes only the visible message and item counts. Lifecycle and
// metadata changes use the complete projector, including fork lineage.
fn streaming_shell(
    connection: &Connection,
    id: &ThreadId,
    previous: &ThreadShell,
    after: u64,
    through: u64,
) -> Result<Option<ThreadShell>> {
    let events = query_events(connection, "stream_id=?1", id.as_str(), after, through, 257)?;
    if events.len() > 256
        || events.iter().any(|event| {
            !matches!(
                &event.event.payload,
                EventPayload::TurnItemTextDelta(_)
                    | EventPayload::MessageUpdated(_)
                    | EventPayload::TurnItemUpdated(TurnItem {
                        body: TurnItemBody::AssistantMessage { .. }
                            | TurnItemBody::Reasoning { .. }
                            | TurnItemBody::CommandExecution { .. },
                        ..
                    })
            )
        })
    {
        return Ok(None);
    }
    let mut shell = previous.clone();
    for event in events {
        if let EventPayload::MessageUpdated(message) = event.event.payload {
            if !matches!(message.role, Role::User | Role::Assistant) {
                return Ok(None);
            }
            if let Some(run) = &message.run_id {
                let eligible = connection.query_row("SELECT json_extract(payload_json,'$.status') NOT IN ('queued','cancelled','rolled_back') FROM orchestration_v2_projection_runs WHERE thread_id=?1 AND run_id=?2", params![id.as_str(),run.as_str()], |row|row.get::<_, bool>(0)).optional()?.unwrap_or(true);
                if !eligible {
                    continue;
                }
            }
            if message.role == Role::User
                && shell
                    .latest_user_message_at
                    .as_ref()
                    .is_none_or(|at| at < &message.created_at)
            {
                shell.latest_user_message_at = Some(message.created_at.clone());
            }
            if shell
                .latest_visible_message
                .as_ref()
                .is_none_or(|old| (&old.updated_at, &old.id) <= (&message.updated_at, &message.id))
            {
                shell.latest_visible_message = Some(VisibleMessage {
                    id: message.id,
                    role: message.role,
                    text: message.text.chars().take(400).collect(),
                    updated_at: message.updated_at,
                });
            }
        }
    }
    let count = connection.query_row(
        "SELECT COUNT(*) FROM orchestration_v2_projection_turn_items WHERE thread_id=?1",
        [id.as_str()],
        |row| row.get::<_, u64>(0),
    )?;
    shell.visible_item_count += count.saturating_sub(shell.item_count);
    shell.item_count = count;
    Ok(Some(shell))
}

fn delegation_children(
    connection: &Connection,
    parent: &ThreadProjection,
) -> Result<Vec<ThreadProjection>> {
    parent
        .subagents
        .iter()
        .filter(|s| s.origin == SubagentOrigin::AppOwned)
        .filter_map(|s| s.child_thread_id.as_ref())
        .map(|id| load_projection(connection, id))
        .collect::<Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}
fn cancel_delegation_descendants(
    connection: &Connection,
    parent: &ThreadProjection,
    now: &Timestamp,
    visited: &mut std::collections::BTreeSet<ThreadId>,
) -> Result<Vec<StoredEvent>> {
    if !visited.insert(parent.thread.id.clone()) || visited.len() > 128 {
        return Err(StoreError::InvalidEvent(
            "invalid delegation cancellation lineage".into(),
        ));
    }
    let children = delegation_children(connection, parent)?;
    let trigger = latest_sequence(connection, None)?.to_string();
    let decision = crate::delegation::cancel_children(parent, &children, now, &trigger)?;
    let changed: std::collections::BTreeSet<_> = decision
        .events
        .iter()
        .filter(|e| e.thread_id != parent.thread.id)
        .map(|e| e.thread_id.clone())
        .collect();
    let mut committed = commit_decision(connection, decision, None, now)?;
    for id in changed {
        let child = load_projection(connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
        if !child.subagents.is_empty() {
            committed.extend(cancel_delegation_descendants(
                connection, &child, now, visited,
            )?);
        }
    }
    Ok(committed)
}

fn reconcile_delegations(
    connection: &Connection,
    changed: &ThreadId,
    now: &Timestamp,
) -> Result<Vec<StoredEvent>> {
    reconcile_delegations_with_offer(connection, changed, now, true)
}
fn reconcile_delegations_with_offer(
    connection: &Connection,
    changed: &ThreadId,
    now: &Timestamp,
    offer: bool,
) -> Result<Vec<StoredEvent>> {
    let projection = load_projection(connection, changed)?.ok_or(StoreError::ThreadNotFound)?;
    let failed_wake = projection
        .runs
        .iter()
        .max_by_key(|r| r.ordinal)
        .is_some_and(|r| {
            matches!(
                r.status,
                RunStatus::Failed | RunStatus::Cancelled | RunStatus::Interrupted
            ) && projection
                .messages
                .iter()
                .any(|m| m.id == r.user_message_id && m.delegated_completion.is_some())
        });
    let mut owners = vec![changed.clone()];
    let mut ancestor = projection.thread;
    while ancestor.lineage.relationship_to_parent == Some(Relationship::Subagent) {
        let Some(parent) = ancestor.lineage.parent_thread_id else {
            break;
        };
        if owners.contains(&parent) || owners.len() > 128 {
            return Err(StoreError::InvalidEvent(
                "invalid delegation lineage".into(),
            ));
        }
        owners.push(parent.clone());
        let Some(projection) = load_projection(connection, &parent)? else {
            break;
        };
        ancestor = projection.thread;
    }
    let mut committed = vec![];
    for id in owners {
        let Some(parent) = load_projection(connection, &id)? else {
            continue;
        };
        if parent.subagents.is_empty() {
            continue;
        }
        committed.extend(cancel_delegation_descendants(
            connection,
            &parent,
            now,
            &mut std::collections::BTreeSet::new(),
        )?);
        let parent = load_projection(connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
        let children = delegation_children(connection, &parent)?;
        let trigger = latest_sequence(connection, None)?.to_string();
        committed.extend(commit_decision(
            connection,
            crate::delegation::finalize(&parent, &children, now, &trigger),
            None,
            now,
        )?);
        let parent = load_projection(connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
        let trigger = latest_sequence(connection, None)?.to_string();
        committed.extend(commit_decision(
            connection,
            crate::delegation::repair(&parent, now, &trigger)?,
            None,
            now,
        )?);
        if offer && parent.thread.deleted_at.is_none() && !(id == *changed && failed_wake) {
            let parent = load_projection(connection, &id)?.ok_or(StoreError::ThreadNotFound)?;
            let trigger = latest_sequence(connection, None)?.to_string();
            committed.extend(commit_decision(
                connection,
                crate::delegation::offer(&parent, now, &trigger)?,
                None,
                now,
            )?);
        }
    }
    Ok(committed)
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
    projection.runs = records(connection, "runs", thread_id)?.into();
    projection.attempts = records(connection, "run_attempts", thread_id)?.into();
    projection.nodes = records(connection, "nodes", thread_id)?.into();
    projection.subagents = records(connection, "subagents", thread_id)?.into();
    projection.provider_sessions = records(connection, "provider_sessions", thread_id)?.into();
    projection.provider_threads = records(connection, "provider_threads", thread_id)?.into();
    projection.provider_turns = records(connection, "provider_turns", thread_id)?.into();
    projection.runtime_requests = records(connection, "runtime_requests", thread_id)?.into();
    projection.messages = records(connection, "messages", thread_id)?.into();
    projection.plans = records(connection, "plans", thread_id)?.into();
    projection.turn_items = records(connection, "turn_items", thread_id)?.into();
    projection.checkpoint_scopes = records(connection, "checkpoint_scopes", thread_id)?.into();
    projection.checkpoints = records(connection, "checkpoints", thread_id)?.into();
    projection.context_handoffs = records(connection, "context_handoffs", thread_id)?.into();
    projection.context_transfers = records(connection, "context_transfers", thread_id)?.into();
    let inherited: Option<String> = connection
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_fork_history WHERE thread_id=?1",
            [thread_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(json) = inherited {
        projection.visible_turn_items = serde_json::from_str(&json)?;
    }
    projection.visible_turn_items = projector::visible_items(&projection).into();
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
        TurnItemTextDelta(delta) => {
            let json: String = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND turn_item_id=?2", params![thread_id.as_str(), delta.item_id.as_str()], |row| row.get(0))?;
            let mut item: TurnItem = serde_json::from_str(&json)?;
            let mut messages = if let TurnItemBody::AssistantMessage { message_id, .. } = &item.body
            {
                let json: Option<String> = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_messages WHERE thread_id=?1 AND message_id=?2", params![thread_id.as_str(), message_id.as_str()], |row| row.get(0)).optional()?;
                json.map(|value| serde_json::from_str(&value))
                    .transpose()?
                    .into_iter()
                    .collect::<Vec<ConversationMessage>>()
            } else {
                vec![]
            };
            let mut plans = if let TurnItemBody::ProposedPlan { plan_id, .. } = &item.body {
                let json: Option<String> = connection.query_row("SELECT payload_json FROM orchestration_v2_projection_plans WHERE thread_id=?1 AND plan_id=?2", params![thread_id.as_str(), plan_id.as_str()], |row| row.get(0)).optional()?;
                json.map(|value| serde_json::from_str(&value))
                    .transpose()?
                    .into_iter()
                    .collect::<Vec<PlanArtifact>>()
            } else {
                vec![]
            };
            if projector::append_text_delta(
                &mut item,
                &mut messages,
                &mut plans,
                delta,
                &event.occurred_at,
            ) {
                record!("turn_items", "turn_item_id", &item);
                for message in messages {
                    record!("messages", "message_id", &message);
                }
                for plan in plans {
                    record!("plans", "plan_id", &plan);
                }
            }
        }
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
        TurnItemTextDelta(_) => None,
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
    let max: u64 = connection.query_row("SELECT COALESCE(MAX(ordinal),0) FROM orchestration_v2_turn_item_positions WHERE thread_id=?1", [thread_id.as_str()], |row| row.get(0))?;
    item.ordinal = max
        .checked_add(1)
        .filter(|ordinal| *ordinal <= i64::MAX as u64)
        .ok_or_else(|| StoreError::InvalidEvent("turn ordinal overflow".into()))?;
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
        let excess = projection.visible_turn_items.len() - limit;
        projection.visible_turn_items.drain(..excess);
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
                position: row.position,
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
    fn completed_steer_becomes_one_idempotent_followup_on_the_selected_provider() {
        let store = setup();
        dispatch(&store, &send("start", DispatchMode::StartImmediately));
        let running = running();
        store
            .ingest(
                crate::events(
                    &running.thread.id,
                    "running",
                    vec![
                        EventPayload::RunUpdated(running.runs[0].clone()),
                        EventPayload::RunAttemptUpdated(running.attempts[0].clone()),
                        EventPayload::ProviderTurnUpdated(running.provider_turns[0].clone()),
                    ],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let long_text = "日本語の追加依頼 ".repeat(100);
        let mut steer = send(
            "steer",
            DispatchMode::SteerActive {
                target_run_id: running.runs[0].id.clone(),
            },
        );
        if let CommandBody::MessageDispatch(message) = &mut steer.body {
            message.text = long_text.clone();
        }
        let commit = dispatch(&store, &steer);
        let effect = read_effects(&store.lock().unwrap())
            .unwrap()
            .into_iter()
            .find(|e| matches!(e.body, EffectBody::Steer { .. }))
            .unwrap();
        assert!(!commit.events.is_empty());
        let mut completed = running.runs[0].clone();
        completed.status = RunStatus::Completed;
        completed.completed_at = Some(now());
        store
            .ingest(
                crate::events(
                    &running.thread.id,
                    "completed",
                    vec![EventPayload::RunUpdated(completed)],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let mut selection = running.thread.model_selection.clone();
        selection.instance_id = ProviderInstanceId::new("claude").unwrap();
        store
            .dispatch(
                &command(
                    "switch",
                    CommandBody::ProviderSwitch {
                        model_selection: selection,
                    },
                ),
                &now(),
                &turns(),
                Driver::Claude,
            )
            .unwrap();
        assert!(!store.steer_follow_up(&effect, &now()).unwrap().replayed);
        assert!(store.steer_follow_up(&effect, &now()).unwrap().replayed);
        let p = store.projection(&running.thread.id).unwrap();
        assert_eq!(
            p.messages
                .iter()
                .filter(|m| m.id.as_str() == "message:steer")
                .count(),
            1
        );
        assert_eq!(p.runs.len(), 2);
        assert_eq!(p.runs[0].status, RunStatus::Completed);
        assert_eq!(p.runs[1].provider_instance_id.as_str(), "claude");
        assert_eq!(
            p.messages
                .iter()
                .find(|m| m.id.as_str() == "message:steer")
                .unwrap()
                .text,
            long_text
        );
    }
    #[test]
    fn delta_ingest_skips_unrelated_history_and_shell_subscribers_share_the_projection() {
        let store = setup();
        dispatch(&store, &send("start", DispatchMode::StartImmediately));
        let p = store.projection(&create().thread_id).unwrap();
        let mut item = p.turn_items[0].clone();
        item.id = TurnItemId::new("live-delta").unwrap();
        item.body = TurnItemBody::Reasoning {
            text: "live".into(),
            streaming: true,
        };
        let run = &p.runs[0];
        // Invalid unrelated history detects accidental full projection reads.
        store.lock().unwrap().execute("UPDATE orchestration_v2_projection_nodes SET payload_json='invalid' WHERE thread_id=?1", [p.thread.id.as_str()]).unwrap();
        let commit = store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "delta",
                    vec![EventPayload::TurnItemUpdated(item.clone())],
                    &now(),
                ),
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        assert_eq!(commit.events.len(), 1);
        // Restore history before the first subscriber computes the shell.
        for node in &p.nodes {
            upsert(
                &store.lock().unwrap(),
                "nodes",
                "node_id",
                node.id.as_str(),
                &p.thread.id,
                node,
            )
            .unwrap();
        }
        let first = store.shell_update(&commit.events[0]).unwrap();
        store.lock().unwrap().execute("UPDATE orchestration_v2_projection_nodes SET payload_json='invalid' WHERE thread_id=?1", [p.thread.id.as_str()]).unwrap();
        let second = store.shell_update(&commit.events[0]).unwrap();
        assert_eq!(first, second);
        item.body = TurnItemBody::Reasoning {
            text: "live continued".into(),
            streaming: true,
        };
        let next = store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "delta-next",
                    vec![EventPayload::TurnItemUpdated(item)],
                    &now(),
                ),
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let streamed = store.shell_update(&next.events[0]).unwrap();
        assert!(
            matches!(streamed, ShellStreamItem::ThreadUpdated { sequence, thread, .. } if sequence == next.events[0].sequence && thread.item_count == p.turn_items.len() as u64 + 1)
        );
    }
    #[test]
    fn native_session_lookup_includes_bex_owned_threads_not_only_import_ids() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        let mut provider = store
            .projection(&create().thread_id)
            .unwrap()
            .provider_threads[0]
            .clone();
        provider.native_thread_ref = Some(ProviderRef {
            driver: Driver::Codex,
            native_id: Some("native-owned-by-bex".into()),
            strength: Strength::Strong,
            fingerprint: None,
            ordinal: None,
        });
        store
            .ingest(
                crate::events(
                    &create().thread_id,
                    "native",
                    vec![EventPayload::ProviderThreadUpdated(provider)],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        assert!(
            store
                .native_session_registered(Driver::Codex, "native-owned-by-bex")
                .unwrap()
        );
        assert!(
            !store
                .native_session_registered(Driver::Claude, "native-owned-by-bex")
                .unwrap()
        );
    }
    #[test]
    fn import_completion_and_checkpoint_namespace_are_durable_and_store_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store.sqlite");
        let store = Store::open(&path).unwrap();
        let instance = store.instance_id().unwrap();
        assert!(!store.import_completed(Driver::Codex).unwrap());
        store.complete_import(Driver::Codex).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.instance_id().unwrap(), instance);
        assert!(reopened.import_completed(Driver::Codex).unwrap());
        assert!(!reopened.import_completed(Driver::Claude).unwrap());
        assert_ne!(Store::memory().unwrap().instance_id().unwrap(), instance);
    }
    #[test]
    fn queued_message_does_not_hide_later_active_run_output_on_partial_clients() {
        let store = setup();
        dispatch(&store, &send("one", DispatchMode::StartImmediately));
        dispatch(&store, &send("two", DispatchMode::QueueAfterActive));
        let before = store.projection(&create().thread_id).unwrap();
        let watermark = before.turn_items.iter().map(|item| item.ordinal).max();
        let mut output = before.turn_items[0].clone();
        output.id = TurnItemId::new("new-live-output").unwrap();
        output.body = TurnItemBody::AssistantMessage {
            message_id: MessageId::new("reply").unwrap(),
            text: "Live".into(),
            attachments: vec![],
            streaming: true,
        };
        let commit = store
            .ingest(
                crate::events(
                    &before.thread.id,
                    "live",
                    vec![EventPayload::TurnItemUpdated(output)],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let client = projector::apply(
            Some(&before),
            &commit.events[0].event,
            projector::ProjectionOptions {
                partial_timeline: true,
                latest_local_turn_ordinal: watermark,
            },
        )
        .unwrap();
        assert!(
            client
                .turn_items
                .iter()
                .any(|item| item.id.as_str() == "new-live-output")
        );
        assert!(client.turn_items.last().unwrap().ordinal > watermark.unwrap());
    }
    #[test]
    fn new_proposal_supersedes_previous_active_proposal_under_the_run_guard() {
        let store = setup();
        dispatch(&store, &send("plan", DispatchMode::StartImmediately));
        let p = store.projection(&create().thread_id).unwrap();
        let run = &p.runs[0];
        let plan = PlanArtifact {
            id: PlanId::new("first-plan").unwrap(),
            thread_id: p.thread.id.clone(),
            run_id: Some(run.id.clone()),
            node_id: run.root_node_id.clone().unwrap(),
            status: PlanStatus::Active,
            detail_in_turn_item: true,
            body: PlanBody::ProposedPlan {
                markdown: "first".into(),
            },
        };
        store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "first-plan",
                    vec![EventPayload::PlanUpdated(plan.clone())],
                    &now(),
                ),
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        let mut next = plan;
        next.id = PlanId::new("next-plan").unwrap();
        let result = store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "next-plan",
                    vec![EventPayload::PlanUpdated(next)],
                    &now(),
                ),
                Some((&run.id, run.active_attempt_id.as_ref())),
                &now(),
            )
            .unwrap();
        assert_eq!(result.events.len(), 2);
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(p.plans[0].status, PlanStatus::Superseded);
        assert_eq!(p.plans[1].status, PlanStatus::Active);
    }
    #[test]
    fn implementation_links_and_completes_only_an_active_plan_atomically() {
        let store = setup();
        dispatch(&store, &send("plan", DispatchMode::StartImmediately));
        let p = store.projection(&create().thread_id).unwrap();
        let plan = PlanArtifact {
            id: PlanId::new("proposal").unwrap(),
            thread_id: p.thread.id.clone(),
            run_id: Some(p.runs[0].id.clone()),
            node_id: p.runs[0].root_node_id.clone().unwrap(),
            status: PlanStatus::Active,
            detail_in_turn_item: true,
            body: PlanBody::ProposedPlan {
                markdown: "# Build".into(),
            },
        };
        store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "proposal",
                    vec![EventPayload::PlanUpdated(plan.clone())],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let mut c = send("implement", DispatchMode::QueueAfterActive);
        let CommandBody::MessageDispatch(input) = &mut c.body else {
            panic!()
        };
        input.source_plan_ref = Some(SourcePlanRef {
            thread_id: p.thread.id.clone(),
            plan_id: plan.id.clone(),
        });
        let commit = store.dispatch(&c, &now(), &turns(), Driver::Codex).unwrap();
        assert!(commit.events.iter().any(|e|matches!(&e.event.payload,EventPayload::PlanUpdated(p) if p.status==PlanStatus::Completed)));
        let p = store.projection(&p.thread.id).unwrap();
        assert_eq!(p.plans[0].status, PlanStatus::Completed);
        assert_eq!(p.runs[1].source_plan_ref.as_ref().unwrap().plan_id, plan.id);
        let sequence = store.sequence().unwrap();
        assert!(
            store
                .dispatch(&c, &now(), &turns(), Driver::Codex)
                .unwrap()
                .replayed
        );
        assert_eq!(store.sequence().unwrap(), sequence);
        c.command_id = CommandId::new("again").unwrap();
        assert!(store.dispatch(&c, &now(), &turns(), Driver::Codex).is_err());
        assert_eq!(store.sequence().unwrap(), sequence);
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
    fn rollback_completion_preserves_concurrent_thread_metadata() {
        let store = setup();
        let request = CommandId::new("rollback").unwrap();
        let mut thread = store.projection(&create().thread_id).unwrap().thread;
        thread.rollback_request_id = Some(request.clone());
        store
            .ingest(
                crate::events(
                    &thread.id,
                    "rollback-start",
                    vec![EventPayload::ThreadMetadataUpdated(thread.clone())],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let mut finished = thread.clone();
        finished.rollback_request_id = None;
        thread.title = "Renamed during rollback".into();
        thread.pinned_at = Some(now());
        thread.deleted_at = Some(now());
        store
            .ingest(
                crate::events(
                    &thread.id,
                    "concurrent",
                    vec![EventPayload::ThreadMetadataUpdated(thread.clone())],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        store
            .ingest_rollback(
                crate::events(
                    &thread.id,
                    "rollback-finished",
                    vec![EventPayload::ThreadMetadataUpdated(finished)],
                    &now(),
                ),
                &request,
                &now(),
            )
            .unwrap();
        thread.rollback_request_id = None;
        assert_eq!(store.projection(&thread.id).unwrap().thread, thread);
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
        assert_eq!(commit.events.len(), 3);
        assert_eq!(
            store.projection(&projection.thread.id).unwrap().runs[1].status,
            RunStatus::Starting
        );
    }
    #[test]
    fn queued_input_is_created_after_the_active_runs_last_output() {
        let store = setup();
        dispatch(&store, &send("active", DispatchMode::StartImmediately));
        dispatch(&store, &send("queued", DispatchMode::QueueAfterActive));
        let p = store.projection(&create().thread_id).unwrap();
        assert!(
            !p.turn_items
                .iter()
                .any(|item| item.run_id.as_ref() == Some(&p.runs[1].id))
        );
        let mut output = p.turn_items[0].clone();
        output.id = TurnItemId::new("last-output").unwrap();
        output.ordinal = 0;
        output.body = TurnItemBody::AssistantMessage {
            message_id: MessageId::new("last-answer").unwrap(),
            text: "final output".into(),
            attachments: vec![],
            streaming: false,
        };
        let mut finished = p.runs[0].clone();
        finished.status = RunStatus::Completed;
        finished.completed_at = Some(now());
        store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "finish-active",
                    vec![
                        EventPayload::TurnItemUpdated(output),
                        EventPayload::RunUpdated(finished),
                    ],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        let p = store.projection(&p.thread.id).unwrap();
        let last = p
            .turn_items
            .iter()
            .find(|i| i.id.as_str() == "last-output")
            .unwrap();
        let queued = p
            .turn_items
            .iter()
            .find(|i| i.run_id.as_ref() == Some(&p.runs[1].id))
            .unwrap();
        assert!(queued.ordinal > last.ordinal);
        assert_eq!(
            p.turn_items
                .iter()
                .filter(|i| i.run_id.as_ref() == Some(&p.runs[1].id))
                .count(),
            1
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
        assert_eq!(commit.events.len(), 6);
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
        assert_eq!(projection.runs[0].status, RunStatus::Completed);
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
