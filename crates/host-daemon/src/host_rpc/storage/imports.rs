//! Explicit native imports append text without replacing Host-owned executions.
use super::*;
use agent_protocol::{
    conversation::{Item, Turn, TurnState},
    models::ThreadResponse,
};
use std::collections::HashMap;

fn source_matches(
    connection: &Connection,
    key: i64,
    active: ProviderKind,
    source: &SessionRef,
) -> Result<bool, String> {
    if Some(active) != source.provider {
        return Ok(false);
    }
    connection.query_row("SELECT loaded!=1 OR EXISTS(SELECT 1 FROM bindings b JOIN sources s ON s.id=b.source_id WHERE b.thread=?1 AND b.provider=?2 AND b.state='ready' AND s.native_id=?3) FROM threads WHERE key=?1",
        params![key,active.key(),source.id],|row|row.get(0)).map_err(failure)
}

pub(super) fn queue(
    transaction: &Transaction<'_>,
    target: &SessionRef,
    source: &SessionRef,
    now: i64,
) -> Result<(), String> {
    let lane = format!("import:{target}");
    let (pending, obsolete) = {
        let mut statement = transaction
            .prepare("SELECT id,attempts,kind,body,state FROM jobs WHERE lane=?1")
            .map_err(failure)?;
        let mut rows = statement.query([&lane]).map_err(failure)?;
        let mut same_source = false;
        let mut obsolete = Vec::new();
        while let Some(row) = rows.next().map_err(failure)? {
            let id: String = row.get(0).map_err(failure)?;
            let attempt: i64 = row.get(1).map_err(failure)?;
            let kind: String = row.get(2).map_err(failure)?;
            let body: Vec<u8> = row.get(3).map_err(failure)?;
            let state: String = row.get(4).map_err(failure)?;
            if let jobs::Work::Import { source: saved, .. } = codec::enum_record(&kind, &body)? {
                if saved == *source {
                    same_source |= state == "pending";
                } else {
                    obsolete.push((
                        Uuid::parse_str(&id).map_err(failure)?,
                        u64::try_from(attempt).map_err(failure)?,
                    ));
                }
            }
        }
        (same_source, obsolete)
    };
    for (id, attempt) in obsolete {
        settle(
            transaction,
            target,
            id,
            attempt,
            cursor(transaction)?,
            &Outcome::Rejected("native import source changed".into()),
        )?;
    }
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
    pub(in crate::host_rpc) fn import_failed(
        &self,
        target: &SessionRef,
        operation: Uuid,
        claim: u64,
        error: String,
    ) -> Result<Committed, String> {
        use agent_protocol::session::{HistoryReadKind, HistoryReadState};
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let saved: Option<(String, Vec<u8>)> = transaction
                .query_row(
                    "SELECT kind,body FROM jobs WHERE id=?1 AND state='running' AND attempts=?2",
                    params![operation.to_string(), integer(claim)?],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(failure)?;
            let Some((kind, body)) = saved else {
                return Ok(Committed {
                    step: Step::default(),
                    cursor: cursor(&transaction)?,
                });
            };
            let jobs::Work::Import {
                target: owner,
                source,
            } = codec::enum_record(&kind, &body)?
            else {
                return Err("job is not a native import".into());
            };
            if owner != *target {
                return Err("native import belongs to another conversation".into());
            }
            let key = thread_key(&transaction, target)?;
            let (mut header, counters) = rows::header(&transaction, key)?;
            let current = source_matches(&transaction, key, header.state.provider, &source)?;
            let read = HistoryReadState::new(HistoryReadKind::Unavailable, vec![error.clone()]);
            let mut sequence = cursor(&transaction)?;
            let mut rows = Vec::new();
            if current && header.details.history_read_state.as_ref() != Some(&read) {
                sequence = Self::next_cursor(&transaction)?;
                header.details.history_read_state = Some(read);
                transaction
                    .execute(
                        "UPDATE threads SET body=?1 WHERE key=?2",
                        params![codec::header(&header, &counters)?, key],
                    )
                    .map_err(failure)?;
                rows.push(Row {
                    key: Key::Thread(target.clone()),
                    version: 0,
                    position: Position::Unordered,
                    content: Content::Full(Arc::new(Record::Thread(header))),
                });
            }
            let outcome = Outcome::Rejected(error);
            settle(&transaction, target, operation, claim, sequence, &outcome)?;
            Self::commit(
                transaction,
                Some(target),
                Step {
                    outcome,
                    ..Default::default()
                },
                rows,
                Vec::new(),
                false,
            )
        })
    }

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
            let sequence = Self::next_cursor(&transaction)?;
            let key = match existing_key(&transaction, target)? {
                Some(key) => key,
                None => {
                    history::insert_header(&transaction, target, observed, &Counters::default())?
                }
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
            history::link_parent(&transaction, key, observed.details.parent.as_ref())?;
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
                    &state.turns,
                    &mut updated,
                    &mut Vec::new(),
                )?;
            }
            updated.extend(rows::write_header(
                &transaction,
                &rows::WriteScope {
                    key,
                    id: target,
                    sequence: sequence.sequence,
                    at: header.details.updated_at,
                    process: self.process,
                },
                &state,
                None,
                None,
                None,
            )?);
            Self::commit(
                transaction,
                Some(target),
                Step {
                    outcome: Outcome::Accepted,
                    ..Default::default()
                },
                updated,
                Vec::new(),
                false,
            )
        })
    }

    pub(in crate::host_rpc) fn import_status(
        &self,
        target: &SessionRef,
        now: i64,
    ) -> Result<Committed, String> {
        use agent_protocol::session::{HistoryReadKind, HistoryReadState};
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let existing = existing_key(&transaction, target)?;
            if existing.is_none()
                && super::titles::native_owner(&transaction, target)?
                    .is_some_and(|owner| owner != *target)
            {
                return Err("native source belongs to another conversation".into());
            }

            if let Some(key) = existing
                && transaction
                    .query_row("SELECT loaded=1 FROM threads WHERE key=?1", [key], |row| {
                        row.get::<_, bool>(0)
                    })
                    .map_err(failure)?
            {
                return Ok(Committed {
                    step: Step::default(),
                    cursor: cursor(&transaction)?,
                });
            }
            let (mut header, counters) = if let Some(key) = existing {
                rows::header(&transaction, key)?
            } else {
                (
                    Header {
                        state: decision::Thread {
                            context_start: 0,
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
            queue(&transaction, target, target, now)?;
            let read = HistoryReadState::new(HistoryReadKind::Loading, vec![]);
            if header.details.history_read_state.as_ref() == Some(&read) {
                let current = cursor(&transaction)?;
                transaction.commit().map_err(failure)?;
                return Ok(Committed {
                    step: Step::default(),
                    cursor: current,
                });
            }
            header.details.history_read_state = Some(read);
            let key = match existing {
                Some(key) => key,
                None => history::insert_header(&transaction, target, &header, &counters)?,
            };
            if existing.is_none() {
                transaction
                    .execute("UPDATE threads SET loaded=0 WHERE key=?1", [key])
                    .map_err(failure)?;
            }
            transaction
                .execute(
                    "UPDATE threads SET body=?1 WHERE key=?2",
                    params![codec::header(&header, &counters)?, key],
                )
                .map_err(failure)?;
            let updated = vec![Row {
                key: Key::Thread(target.clone()),
                version: 0,
                position: Position::Unordered,
                content: Content::Full(Arc::new(Record::Thread(header))),
            }];
            Self::commit(
                transaction,
                Some(target),
                Step::default(),
                updated,
                Vec::new(),
                false,
            )
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
            if accepted { return Ok(Committed { step: Step::default(), cursor: cursor(&transaction)? }); }
            let current_claim: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state='running' AND attempts=?2)", params![operation.to_string(),integer(claim)?], |row| row.get(0)).map_err(failure)?;
            if !current_claim { return Err("import claim is obsolete".into()); }
            let loaded: bool = transaction.query_row("SELECT loaded=1 FROM threads WHERE key=?1", [key], |row| row.get(0)).map_err(failure)?;
            if !loaded {
                let sequence = Self::next_cursor(&transaction)?;
                history::install(&transaction, target, response, Some(source), sequence.sequence, self.process, now)?;
                settle(&transaction, target, operation, claim, sequence, &Outcome::Accepted)?;
                return Self::commit(transaction,Some(target),Step {outcome:Outcome::Accepted,..Default::default()},Vec::new(),Vec::new(),true);
            }
            let mut state = rows::load(&transaction, key, self.process)?;
            if state.thread.name.is_empty() { state.thread.name = response.thread.name.clone().unwrap_or_default(); }
            if !source_matches(&transaction,key,state.thread.provider,source)? {
                let sequence = Self::next_cursor(&transaction)?;
                let outcome = Outcome::Rejected("native import source changed".into());
                let (mut header, counters) = rows::header(&transaction, key)?;
                let mut rows = Vec::new();
                if header.details.history_read_state.as_ref().is_some_and(|read| read.kind == agent_protocol::session::HistoryReadKind::Loading) {
                    header.details.history_read_state = Some(agent_protocol::session::HistoryReadState::new(agent_protocol::session::HistoryReadKind::Unavailable, vec!["native import source changed".into()]));
                    transaction.execute("UPDATE threads SET body=?1 WHERE key=?2",params![codec::header(&header, &counters)?,key]).map_err(failure)?;
                    rows.push(Row { key: Key::Thread(target.clone()), version: 0, position: Position::Unordered, content: Content::Full(Arc::new(Record::Thread(header))) });
                }
                let receipt_cursor=if rows.is_empty() {cursor(&transaction)?} else {sequence};
                settle(&transaction,target,operation,claim,receipt_cursor,&outcome)?;
                return Self::commit(transaction,Some(target),Step {outcome,..Default::default()},rows,Vec::new(),false);
            }
            if state.active_turn().is_some_and(|turn| !turn.observed || state.attempts.contains_key(&turn.id) || response.thread.status==agent_protocol::models::SessionStatus::Running) || state.inputs.values().any(|input| !matches!(input.state, decision::InputState::Queued { .. } | decision::InputState::Failed { .. })) { return Err("conversation is busy; native import will resume when it is idle".into()); }
            let caught_up = matches!(state.bindings.get(&state.thread.provider), Some(decision::Binding::Ready { covered, .. }) if *covered == state.history_end());
            let source_key = rows::source(&transaction, key, source.native_provider().map_err(failure)?, &source.id)?;
            let sequence = Self::next_cursor(&transaction)?;
            let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, source.to_string().as_bytes());
            let import = Import { source, source_key, namespace, write: rows::WriteScope { key, id: target, sequence: sequence.sequence, at: now, process: self.process } };
            let previous_order: Vec<TurnId> = rows::read::<Turn>(&transaction, "turns", "thread=?1 ORDER BY ordinal", vec![key.into()], rows::TURN, true)?.into_iter().map(|(_, turn)| turn.id).collect();
            let mut imported_order = Vec::new();
            let mut occurrences = HashMap::new();
            for native in response.thread.turns.iter().flatten() {
                let occurrence = occurrences.entry(native.id.clone()).or_insert(0);
                if let Some(turn) = import_turn(&transaction, &import, native, *occurrence, &mut state)? { imported_order.push(turn); }
                *occurrence += 1;
            }
            reorder_imported(&transaction, &import.write, &mut state, previous_order, &imported_order)?;
            if caught_up {
                let binding = Change::Binding { provider: state.thread.provider, value: Some(decision::Binding::Ready {
                    source: source.id.clone(), covered: state.history_end(),
                }) };
                state = state.apply(std::slice::from_ref(&binding));
                rows::write(&transaction, &import.write, &binding, &state.turns, &mut Vec::new(), &mut Vec::new())?;
            }
            let previous=rows::header(&transaction,key)?.0.details.selection;
            let selection=decision::records::Selection {model:previous.model.or(response.model.clone()),effort:previous.effort.or(response.effort.clone()),service_tier:previous.service_tier.or(response.service_tier.clone())};
            rows::write_header(&transaction, &rows::WriteScope { key, id: target, sequence: sequence.sequence, at: now, process: self.process }, &state, Some((selection,None)), None, None)?;
            // Reimport publishes a reset, including the native reader's final completeness.
            let (mut header, _) = rows::header(&transaction, key)?;
            header.details.history_read_state = response.thread.history_read_state.clone();
            transaction.execute("UPDATE threads SET body=?1 WHERE key=?2", params![codec::header(&header, &state.counters)?,key]).map_err(failure)?;
            settle(&transaction,target,operation,claim,sequence,&Outcome::Accepted)?;
            Self::commit(transaction,Some(target),Step {outcome:Outcome::Accepted,..Default::default()},Vec::new(),Vec::new(),true)
        })
    }
}

struct Import<'a> {
    source: &'a SessionRef,
    source_key: i64,
    namespace: Uuid,
    write: rows::WriteScope<'a>,
}

fn reorder_imported(
    transaction: &Transaction<'_>,
    scope: &rows::WriteScope<'_>,
    state: &mut State,
    previous: Vec<TurnId>,
    imported: &[TurnId],
) -> Result<(), String> {
    let order = merged_order(previous, imported);
    let turns: Vec<Turn> = order
        .iter()
        .map(|id| {
            rows::read::<Turn>(
                transaction,
                "turns",
                "thread=?1 AND id=?2",
                vec![scope.key.into(), id.to_string().into()],
                rows::TURN,
                true,
            )
            .and_then(|mut rows| {
                rows.pop()
                    .map(|(_, turn)| turn)
                    .ok_or_else(|| "imported turn is missing".into())
            })
        })
        .collect::<Result<_, _>>()?;
    if turns
        .iter()
        .enumerate()
        .all(|(index, turn)| turn.ordinal == index as u64 + 1)
    {
        return Ok(());
    }
    let remapped: std::collections::BTreeMap<_, _> = turns
        .iter()
        .enumerate()
        .map(|(index, turn)| (turn.ordinal, index as u64 + 1))
        .collect();
    let bindings: Vec<_> = state
        .bindings
        .iter()
        .filter_map(|(provider, binding)| {
            if let decision::Binding::Ready { source, covered } = binding {
                let next = remapped.get(covered).copied().unwrap_or(*covered);
                (next != *covered).then(|| Change::Binding {
                    provider: *provider,
                    value: Some(decision::Binding::Ready {
                        source: source.clone(),
                        covered: next,
                    }),
                })
            } else {
                None
            }
        })
        .collect();
    // Move indexed ordinals out of the destination range before assigning the
    // new order; bodies and public identities stay unchanged in this transaction.
    transaction
        .execute(
            "UPDATE turns SET ordinal=-ordinal WHERE thread=?1",
            [scope.key],
        )
        .map_err(failure)?;
    for (index, mut turn) in turns.into_iter().enumerate() {
        turn.ordinal = index as u64 + 1;
        state.turns.insert(turn.id.clone(), turn.clone());
        rows::write(
            transaction,
            scope,
            &Change::Turn(turn),
            &state.turns,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
    }
    for change in bindings {
        *state = state.apply(std::slice::from_ref(&change));
        rows::write(
            transaction,
            scope,
            &change,
            &state.turns,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
    }
    state.counters.turn = state.counters.turn.max(order.len() as u64);
    Ok(())
}

// A missing prefix precedes the next native turn already known to the Host;
// existing canonical turns keep their relative order, including other providers.
fn merged_order(mut previous: Vec<TurnId>, imported: &[TurnId]) -> Vec<TurnId> {
    for (index, id) in imported.iter().enumerate() {
        if previous.contains(id) {
            continue;
        }
        let before = imported[index + 1..]
            .iter()
            .find_map(|next| previous.iter().position(|id| id == next))
            .unwrap_or(previous.len());
        previous.insert(before, id.clone());
    }
    previous
}

fn import_turn(
    transaction: &Transaction<'_>,
    import: &Import<'_>,
    native: &agent_protocol::models::Turn,
    occurrence: usize,
    state: &mut State,
) -> Result<Option<TurnId>, String> {
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
        return Ok(mapped.map(|(turn, _, _)| turn.into()));
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
        return Ok(None);
    }
    let refreshing = existing.as_ref().is_some_and(|turn| {
        matches!(
            turn.state,
            TurnState::Running { .. } | TurnState::Waiting(decision::Wait::Background { .. })
        )
    });
    let turn = if let Some(mut turn) = existing {
        turn.state = history::imported_state(native.status, native.error.as_ref());
        turn
    } else {
        state.counters.turn += 1;
        Turn {
            attempts: Vec::new(),
            fresh_context: false,
            id: if *source == write.id && !state.turns.contains_key(&native.id) {
                native.id.clone()
            } else {
                Uuid::new_v5(
                    namespace,
                    format!("turn:{occurrence}:{}", native.id).as_bytes(),
                )
                .to_string()
                .into()
            },
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
            compaction: false,
            error: None,
            state: history::imported_state(native.status, native.error.as_ref()),
        }
    };
    state.turns.insert(turn.id.clone(), turn.clone());
    rows::write(
        transaction,
        write,
        &Change::Turn(turn.clone()),
        &state.turns,
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
        let canonical =
            if *source == write.id && rows::item(transaction, key, &native.id)?.is_none() {
                native.id.clone()
            } else {
                Uuid::new_v5(
                    namespace,
                    format!("item:{}:{}", turn.id, native.id).as_bytes(),
                )
                .to_string()
                .into()
            };
        let mapped: Option<String> = transaction.query_row("SELECT item_id FROM item_natives WHERE thread=?1 AND turn_id=?2 AND attempt=?3 AND native_id=?4", params![key,turn.id.as_str(),attempt.to_string(),native.id.as_str()], |row| row.get(0)).optional().map_err(failure)?;
        let existing = mapped
            .map(|id| rows::item(transaction, key, &id.into()))
            .transpose()?
            .flatten()
            .or(rows::item(transaction, key, &native.id)?)
            .filter(|item| item.turn == turn.id)
            .or(rows::item(transaction, key, &canonical)?.filter(|item| item.turn == turn.id));
        let item = if let Some(mut item) = existing {
            // Settled messages and composed user input are authoritative. A
            // foreign running turn may still supply its final assistant text;
            // its original canonical input linkage always stays unchanged.
            if refreshing
                && matches!(
                    item.body,
                    agent_protocol::items::ItemBody::AssistantText { .. }
                )
            {
                item.body = native.body().clone();
                item.status = native.status;
                item.deferred = native.is_deferred();
                rows::write(
                    transaction,
                    write,
                    &Change::Item(item.clone()),
                    &state.turns,
                    &mut Vec::new(),
                    &mut Vec::new(),
                )?;
            }
            item
        } else {
            state.counters.item += 1;
            let item = Item {
                id: canonical,
                turn: turn.id.clone(),
                position: state.counters.item,
                status: native.status,
                deferred: native.is_deferred(),
                input: native.client_input_id.clone(),
                author: native.author.clone(),
                attempt: native.attempt,
                body: native.body().clone(),
            };
            rows::write(
                transaction,
                write,
                &Change::Item(item.clone()),
                &state.turns,
                &mut Vec::new(),
                &mut Vec::new(),
            )?;
            item
        };
        transaction.execute("INSERT INTO item_natives(thread,turn_id,attempt,native_id,item_id) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(thread,turn_id,attempt,native_id) DO NOTHING", params![key,turn.id.as_str(),attempt.to_string(),native.id.as_str(),item.id.as_str()]).map_err(failure)?;
    }
    Ok(Some(turn.id))
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
            "DELETE FROM jobs WHERE id=?1 AND attempts=?2",
            params![operation.to_string(), integer(claim)?],
        )
        .map_err(failure)?;
    if deleted != 1 {
        return Err("import claim is obsolete".into());
    }
    Ok(())
}
