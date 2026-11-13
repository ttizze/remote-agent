//! Explicit native imports append text without replacing Host-owned executions.
use super::*;
use agent_protocol::{
    conversation::{Item, Turn, TurnState},
    models::{ThreadResponse, TurnStatus},
};
use std::collections::HashMap;

pub(super) fn queue(
    transaction: &Transaction<'_>,
    target: &SessionRef,
    source: &SessionRef,
    now: i64,
) -> Result<(), String> {
    let lane = format!("import:{target}");
    let pending = {
        let mut statement = transaction
            .prepare("SELECT kind,body FROM jobs WHERE lane=?1 AND state='pending'")
            .map_err(failure)?;
        let mut rows = statement.query([&lane]).map_err(failure)?;
        let mut same_source = false;
        while let Some(row) = rows.next().map_err(failure)? {
            let kind: String = row.get(0).map_err(failure)?;
            let body: Vec<u8> = row.get(1).map_err(failure)?;
            if matches!(codec::enum_record(&kind,&body)?, jobs::Work::Import { source: saved, .. } if saved == *source)
            {
                same_source = true;
                break;
            }
        }
        same_source
    };
    if !pending {
        jobs::enqueue(
            transaction,
            Uuid::new_v4(),
            &lane,
            now,
            &jobs::Work::Import {
                target: target.clone(),
                source: source.clone(),
            },
        )?;
    }
    Ok(())
}

impl Storage {
    pub(in crate::host_rpc) fn discover(
        &self,
        target: &SessionRef,
        source: &SessionRef,
        observed: &Header,
        now: i64,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let sequence = Self::advance(&transaction)?;
            let key = match existing_key(&transaction, target)? {
                Some(key) => key,
                None => history::insert_header(
                    &transaction,
                    target,
                    observed,
                    &Counters::default(),
                    sequence.sequence,
                )?,
            };
            let (mut header, counters) = rows::header(&transaction, key)?;
            header.details.can_accept_direct_input = Some(false);
            if header.state.workspace == decision::Workspace::Missing {
                header.state.workspace = observed.state.workspace.clone();
            }
            transaction
                .execute(
                    "UPDATE threads SET loaded=1,cwd=?1,body=?2 WHERE key=?3",
                    params![
                        header.state.workspace.cwd(),
                        codec::header(&header, &counters)?,
                        key
                    ],
                )
                .map_err(failure)?;
            history::link_parent(
                &transaction,
                key,
                observed.details.parent.as_ref(),
                sequence.sequence,
            )?;
            let mut state = rows::load(&transaction, key, self.process)?;
            let provider = source.native_provider().map_err(failure)?;
            rows::source(&transaction, key, provider, &source.id)?;
            let mut updated = Vec::new();
            if !state.bindings.contains_key(&provider) {
                let binding = Change::Binding {
                    provider,
                    value: Some(decision::Binding::Ready {
                        source: source.id.clone(),
                        covered: state.history_end(),
                    }),
                };
                state = state.apply(std::slice::from_ref(&binding));
                rows::write(
                    &transaction,
                    &rows::WriteScope {
                        key,
                        id: target,
                        sequence: sequence.sequence,
                        at: now,
                        process: self.process,
                    },
                    &binding,
                    &state,
                    &mut updated,
                    &mut Vec::new(),
                )?;
            }
            updated.push(rows::write_header(
                &transaction,
                key,
                target,
                &state,
                sequence.sequence,
                header.details.updated_at,
                None,
            )?);
            transaction.commit().map_err(failure)?;
            Ok(Committed {
                state: Some(state),
                step: Step {
                    outcome: Outcome::Accepted,
                    ..Default::default()
                },
                cursor: sequence,
                rows: updated,
                removed: vec![],
                reset: false,
            })
        })
    }

    pub(in crate::host_rpc) fn import_status(
        &self,
        target: &SessionRef,
        error: Option<String>,
        now: i64,
    ) -> Result<Committed, String> {
        use agent_protocol::session::{HistoryReadKind, HistoryReadState};
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let existing = existing_key(&transaction, target)?;
            if let Some(key) = existing
                && transaction
                    .query_row("SELECT loaded=1 FROM threads WHERE key=?1", [key], |row| {
                        row.get::<_, bool>(0)
                    })
                    .map_err(failure)?
            {
                return Ok(Committed {
                    state: None,
                    step: Step::default(),
                    cursor: cursor(&transaction)?,
                    rows: vec![],
                    removed: vec![],
                    reset: false,
                });
            }
            let sequence = Self::advance(&transaction)?;
            let (mut header, counters) = if let Some(key) = existing {
                rows::header(&transaction, key)?
            } else {
                (
                    Header {
                        state: decision::Thread {
                            name: String::new(),
                            workspace: decision::Workspace::Missing,
                            provider: target.native_provider().map_err(failure)?,
                            queue_held: false,
                        },
                        details: decision::records::Details::default(),
                    },
                    Counters::default(),
                )
            };
            header.details.history_read_state = Some(match error {
                Some(error) => HistoryReadState::new(HistoryReadKind::Unavailable, vec![error]),
                None => {
                    queue(&transaction, target, target, now)?;
                    HistoryReadState::new(HistoryReadKind::Loading, vec![])
                }
            });
            let key = match existing {
                Some(key) => key,
                None => history::insert_header(
                    &transaction,
                    target,
                    &header,
                    &counters,
                    sequence.sequence,
                )?,
            };
            transaction
                .execute(
                    "UPDATE threads SET loaded=0,seq=?1,body=?2 WHERE key=?3",
                    params![
                        integer(sequence.sequence)?,
                        codec::header(&header, &counters)?,
                        key
                    ],
                )
                .map_err(failure)?;
            transaction.commit().map_err(failure)?;
            Ok(Committed {
                state: None,
                step: Step::default(),
                cursor: sequence,
                rows: vec![Row {
                    key: Key::Thread(target.clone()),
                    version: sequence.sequence,
                    position: Position::Unordered,
                    content: Content::Full(Arc::new(Record::Thread(header))),
                }],
                removed: vec![],
                reset: false,
            })
        })
    }

    pub(in crate::host_rpc) fn queue_import(
        &self,
        target: &SessionRef,
        source: &SessionRef,
        now: i64,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            queue(&transaction, target, source, now)?;
            transaction.commit().map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn reimport(
        &self,
        target: &SessionRef,
        source: &SessionRef,
        operation: Uuid,
        claim: u64,
        response: &ThreadResponse,
        now: i64,
    ) -> Result<Committed, String> {
        agent_protocol::operations::validate_thread(response, Some(source))
            .map_err(str::to_owned)?;
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let key = thread_key(&transaction, target)?;
            let receipt = decision::OperationId::Job(operation).receipt_key();
            let accepted: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM receipts WHERE operation_id=?1 AND provider IS ?2 AND thread_id=?3)", params![receipt,target.provider.map(ProviderKind::key),target.id], |row| row.get(0)).map_err(failure)?;
            let mut state = rows::load(&transaction, key, self.process)?;
            if accepted { return Ok(Committed { state: Some(state), step: Step::default(), cursor: cursor(&transaction)?, rows: vec![], removed: vec![], reset: false }); }
            let current_claim: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='running' AND attempts=?2)", params![operation.to_string(),integer(claim)?], |row| row.get(0)).map_err(failure)?;
            if !current_claim { return Err("import claim is obsolete".into()); }
            let loaded: bool = transaction.query_row("SELECT loaded=1 FROM threads WHERE key=?1", [key], |row| row.get(0)).map_err(failure)?;
            if !loaded {
                let sequence = Self::advance(&transaction)?;
                history::install(&transaction, target, response, Some(source), sequence.sequence, self.process, now)?;
                let state = rows::load(&transaction, key, self.process)?;
                settle(&transaction, target, operation, claim, sequence, &Outcome::Accepted)?;
                transaction.commit().map_err(failure)?;
                return Ok(Committed { state: Some(state), step: Step { outcome: Outcome::Accepted, ..Default::default() }, cursor: sequence, rows: vec![], removed: vec![], reset: true });
            }
            let binding: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM bindings b JOIN sources s ON s.id=b.source_id WHERE b.thread=?1 AND b.provider=?2 AND b.state='ready' AND s.native_id=?3)", params![key,source.native_provider().map_err(failure)?.key(),source.id], |row| row.get(0)).map_err(failure)?;
            if !binding || Some(state.thread.provider) != source.provider {
                let sequence = Self::advance(&transaction)?;
                let outcome = Outcome::Rejected("native import source changed".into());
                settle(&transaction,target,operation,claim,sequence,&outcome)?;
                transaction.commit().map_err(failure)?;
                return Ok(Committed { state: Some(state), step: Step { outcome, ..Default::default() }, cursor: sequence, rows: vec![], removed: vec![], reset: false });
            }
            if state.active_turn().is_some_and(|turn| !turn.observed || state.attempts.contains_key(&turn.id) || response.thread.status==agent_protocol::models::SessionStatus::Running) || state.inputs.values().any(|input| !matches!(input.state, decision::InputState::Queued { .. } | decision::InputState::Failed(_))) { return Err("conversation is busy; native import will resume when it is idle".into()); }
            let source_key = rows::source(&transaction, key, source.native_provider().map_err(failure)?, &source.id)?;
            let sequence = Self::advance(&transaction)?;
            let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, source.to_string().as_bytes());
            let import = Import { source, source_key, namespace, write: rows::WriteScope { key, id: target, sequence: sequence.sequence, at: now, process: self.process } };
            let mut occurrences = HashMap::new();
            for native in response.thread.turns.iter().flatten() {
                let occurrence = occurrences.entry(native.id.clone()).or_insert(0);
                import_turn(&transaction, &import, native, *occurrence, &mut state)?;
                *occurrence += 1;
            }
            let previous=rows::header(&transaction,key)?.0.details.selection;
            let selection=decision::records::Selection {model:previous.model.or(response.model.clone()),effort:previous.effort.or(response.effort.clone()),service_tier:previous.service_tier.or(response.service_tier.clone())};
            let header = rows::write_header(&transaction, key, target, &state, sequence.sequence, now, Some((selection,None)))?;
            transaction.execute("UPDATE threads SET history_seq=?1 WHERE key=?2", params![integer(sequence.sequence)?,key]).map_err(failure)?;
            settle(&transaction,target,operation,claim,sequence,&Outcome::Accepted)?;
            transaction.commit().map_err(failure)?;
            Ok(Committed { state: Some(state), step: Step { outcome: Outcome::Accepted, ..Default::default() }, cursor: sequence, rows: vec![header], removed: vec![], reset: true })
        })
    }
}

struct Import<'a> {
    source: &'a SessionRef,
    source_key: i64,
    namespace: Uuid,
    write: rows::WriteScope<'a>,
}

fn import_turn(
    transaction: &Transaction<'_>,
    import: &Import<'_>,
    native: &agent_protocol::models::Turn,
    occurrence: usize,
    state: &mut State,
) -> Result<(), String> {
    let Import {
        source,
        source_key,
        namespace,
        write,
    } = import;
    let &rows::WriteScope { key, sequence, .. } = write;
    // A native echo of a Host-owned execution cannot replace canonical input,
    // tool output or another provider's history with a text-only import.
    let mapped = transaction.query_row("SELECT turn_id,process,attempt FROM executions WHERE thread=?1 AND source_id=?2 AND native_turn_id=?3 AND (native_position=?4 OR (?4=0 AND native_position IS NULL)) ORDER BY started_seq DESC LIMIT 1", params![key,source_key,native.id.as_str(),occurrence as i64], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))).optional().map_err(failure)?;
    if mapped
        .as_ref()
        .is_some_and(|(_, process, _)| process != &Uuid::nil().to_string())
    {
        return Ok(());
    }
    let existing = if let Some((turn, _, _)) = &mapped {
        rows::read::<Turn>(
            transaction,
            "turns",
            "thread=?1 AND id=?2",
            vec![key.into(), turn.clone().into()],
            rows::TURN,
            true,
        )?
        .into_iter()
        .next()
        .map(|(_, turn)| turn)
    } else if occurrence == 0 {
        rows::turn(transaction, key, &native.id)?
            .filter(|turn| Some(turn.provider) == source.provider)
    } else {
        None
    };
    if existing
        .as_ref()
        .is_some_and(|turn| turn.state == TurnState::RolledBack)
    {
        return Ok(());
    }
    let turn = if let Some(mut turn) = existing {
        turn.state = match native.status {
            TurnStatus::Completed => TurnState::Completed,
            TurnStatus::Failed => TurnState::Failed(native.error.clone().unwrap_or_default()),
            _ => TurnState::Interrupted,
        };
        turn
    } else {
        state.counters.turn += 1;
        Turn {
            id: Uuid::new_v5(
                namespace,
                format!("turn:{occurrence}:{}", native.id).as_bytes(),
            )
            .to_string()
            .into(),
            ordinal: state.counters.turn,
            provider: source.native_provider().map_err(failure)?,
            queued: false,
            observed: true,
            started_at: native
                .started_at_ms
                .map(|at| at as i64)
                .or_else(|| native.started_at.map(|at| (at * 1000.0) as i64)),
            completed_at: native.completed_at_ms.map(|at| at as i64),
            duration_ms: native.duration_ms,
            retry_count: 0,
            error: None,
            state: match native.status {
                TurnStatus::Completed => TurnState::Completed,
                TurnStatus::Failed => TurnState::Failed(native.error.clone().unwrap_or_default()),
                _ => TurnState::Interrupted,
            },
        }
    };
    state.turns.insert(turn.id.clone(), turn.clone());
    rows::write(
        transaction,
        write,
        &Change::Turn(turn.clone()),
        state,
        &mut Vec::new(),
        &mut Vec::new(),
    )?;
    let attempt = mapped
        .as_ref()
        .map(|(_, _, attempt)| Uuid::parse_str(attempt).map_err(failure))
        .transpose()?
        .unwrap_or_else(|| {
            Uuid::new_v5(
                namespace,
                format!("import:{occurrence}:{}", turn.id).as_bytes(),
            )
        });
    transaction.execute("INSERT INTO executions(thread,turn_id,attempt,source_id,native_turn_id,current,process,started_seq,native_position) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8) ON CONFLICT(thread,turn_id,attempt) DO NOTHING",params![key,turn.id.as_str(),attempt.to_string(),source_key,native.id.as_str(),Uuid::nil().to_string(),integer(sequence)?,occurrence as i64]).map_err(failure)?;
    let items = native.items.as_deref().unwrap_or_default();
    for (index, native) in items.iter().enumerate() {
        if items[index + 1..].iter().any(|later| later.id == native.id) {
            continue;
        }
        if !native.is_history_message() {
            continue;
        }
        let canonical = Uuid::new_v5(
            namespace,
            format!("item:{}:{}", turn.id, native.id).as_bytes(),
        )
        .to_string()
        .into();
        let mapped: Option<String> = transaction.query_row("SELECT item_id FROM item_natives WHERE thread=?1 AND turn_id=?2 AND attempt=?3 AND native_id=?4", params![key,turn.id.as_str(),attempt.to_string(),native.id.as_str()], |row| row.get(0)).optional().map_err(failure)?;
        let existing = mapped
            .map(|id| rows::item(transaction, key, &id.into()))
            .transpose()?
            .flatten()
            .or(rows::item(transaction, key, &native.id)?)
            .filter(|item| item.turn == turn.id)
            .or(rows::item(transaction, key, &canonical)?.filter(|item| item.turn == turn.id));
        let (id, position) = if let Some(item) = existing {
            (item.id, item.position)
        } else {
            state.counters.item += 1;
            (canonical, state.counters.item)
        };
        let item = Item {
            id,
            turn: turn.id.clone(),
            position,
            status: native.status,
            deferred: native.is_deferred(),
            input: native.client_input_id.clone(),
            body: native.body().clone(),
        };
        rows::write(
            transaction,
            write,
            &Change::Item(item.clone()),
            state,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
        transaction.execute("INSERT INTO item_natives(thread,turn_id,attempt,native_id,item_id) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(thread,turn_id,attempt,native_id) DO NOTHING", params![key,turn.id.as_str(),attempt.to_string(),native.id.as_str(),item.id.as_str()]).map_err(failure)?;
    }
    Ok(())
}

fn settle(
    transaction: &Transaction<'_>,
    target: &SessionRef,
    operation: Uuid,
    claim: u64,
    cursor: Cursor,
    outcome: &Outcome,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO receipts VALUES (?1,?2,?3,?4,?5)",
            params![
                decision::OperationId::Job(operation).receipt_key(),
                target.provider.map(ProviderKind::key),
                target.id,
                integer(cursor.sequence)?,
                protocol::encode(outcome).map_err(failure)?
            ],
        )
        .map_err(failure)?;
    let deleted = transaction
        .execute(
            "DELETE FROM jobs WHERE id=?1 AND state='running' AND attempts=?2",
            params![operation.to_string(), integer(claim)?],
        )
        .map_err(failure)?;
    if deleted != 1 {
        return Err("import claim is obsolete".into());
    }
    Ok(())
}
