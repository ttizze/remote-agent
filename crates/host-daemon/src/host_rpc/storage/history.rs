//! Import is a single write, never an overlay over a second mutable transcript.
use super::*;
use agent_protocol::{
    conversation::{Item, Turn, TurnState, Workspace},
    execution::TurnStatus,
    models::ThreadResponse,
    sync::Scope,
};
use std::collections::BTreeSet;

pub(in crate::host_rpc) fn header(response: &ThreadResponse, now: i64) -> Header {
    let thread = &response.thread;
    Header {
        state: decision::Thread {
            name: thread.name.clone().unwrap_or_default(),
            workspace: thread
                .cwd
                .clone()
                .map(Workspace::Ready)
                .unwrap_or(Workspace::Missing),
            provider: thread
                .active_provider
                .or_else(|| thread.id.as_ref().and_then(|id| id.provider))
                .expect("validated history identity"),
            queue_held: thread.queue.held,
        },
        details: decision::records::Details {
            parent: thread.parent_id.clone(),
            selection: decision::records::Selection {
                model: response.model.clone(),
                effort: response.effort.clone(),
                service_tier: response.service_tier.clone(),
            },
            capabilities: decision::records::capabilities(thread.capabilities.unwrap_or_default()),
            preview: thread
                .preview
                .as_deref()
                .and_then(decision::records::preview),
            updated_at: thread.updated_at.map_or(now, |at| (at * 1000.0) as i64),
            project: thread.project_id.clone(),
            branch: thread.git_branch.clone(),
            has_changes: thread.has_changes,
            agent: thread.agent_id.clone(),
            can_accept_direct_input: thread.can_accept_direct_input,
            history_read_state: thread.history_read_state.clone(),
            activity: native_activity(thread),
            ..Default::default()
        },
    }
}

fn native_activity(thread: &agent_protocol::models::Thread) -> decision::records::Activity {
    use agent_protocol::{requests::RequestBody, session::RequestDelivery};
    let awaiting = thread
        .requests
        .values()
        .filter(|request| request.delivery == RequestDelivery::Awaiting)
        .collect::<Vec<_>>();
    let latest = thread.turns.iter().flatten().last();
    decision::records::Activity {
        stale: false,
        status: thread.status,
        waiting: !awaiting.is_empty(),
        approval: awaiting.iter().any(|request| {
            matches!(
                request.body,
                RequestBody::Approval { .. } | RequestBody::Permission { .. }
            )
        }),
        latest: latest.map(|turn| turn.status),
        completion: latest
            .filter(|turn| turn.status != TurnStatus::Running)
            .map(|turn| turn.id.clone()),
        background: thread.background.clone(),
    }
}

fn unique(id: &str, seen: &mut BTreeSet<String>, namespace: Uuid) -> String {
    let mut candidate = id.to_owned();
    let mut occurrence = 0;
    while candidate.is_empty() || !seen.insert(candidate.clone()) {
        occurrence += 1;
        candidate = Uuid::new_v5(&namespace, format!("{id}:{occurrence}").as_bytes()).to_string();
    }
    candidate
}

pub(super) fn insert_header(
    transaction: &Transaction<'_>,
    id: &SessionRef,
    header: &Header,
    counters: &Counters,
    sequence: u64,
) -> Result<i64, String> {
    let parent = header
        .details
        .parent
        .as_ref()
        .map(|id| thread_key(transaction, id))
        .transpose()?;
    transaction.execute(
        "INSERT INTO threads(provider,id,seq,parent,cwd,updated_at,history_seq,body) VALUES (?1,?2,?3,?4,?5,?6,?3,?7)",
        params![id.provider.map(ProviderKind::key), id.id, integer(sequence)?, parent, header.state.workspace.cwd(), header.details.updated_at, codec::header(header, counters)?],
    ).map_err(failure)?;
    Ok(transaction.last_insert_rowid())
}

/// Native children may precede their parent in discovery. Keep that edge and
/// resolve an already-owned native source to its stable public conversation.
pub(super) fn link_parent(
    transaction: &Transaction<'_>,
    child: i64,
    parent: Option<&SessionRef>,
    sequence: u64,
) -> Result<(), String> {
    let parent = if let Some(parent) = parent {
        let owner = transaction
            .query_row(
                "SELECT thread FROM sources WHERE provider=?1 AND native_id=?2",
                params![parent.provider.map(ProviderKind::key), parent.id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(failure)?;
        let key = if let Some(key) = owner.or(existing_key(transaction, parent)?) {
            key
        } else {
            let header = Header {
                state: decision::Thread {
                    name: String::new(),
                    workspace: decision::Workspace::Missing,
                    provider: parent.native_provider().map_err(failure)?,
                    queue_held: false,
                },
                details: decision::records::Details {
                    activity: decision::records::Activity {
                        status: agent_protocol::models::SessionStatus::Unknown,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            };
            let key = insert_header(transaction, parent, &header, &Counters::default(), sequence)?;
            transaction
                .execute("UPDATE threads SET loaded=-1 WHERE key=?1", [key])
                .map_err(failure)?;
            key
        };
        let cycle:bool=transaction.query_row("WITH RECURSIVE ancestors(key) AS (SELECT ?1 UNION SELECT t.parent FROM threads t JOIN ancestors a ON t.key=a.key WHERE t.parent IS NOT NULL) SELECT EXISTS(SELECT 1 FROM ancestors WHERE key=?2)",params![key,child],|row|row.get(0)).map_err(failure)?;
        (!cycle).then_some(key)
    } else {
        None
    };
    transaction
        .execute(
            "UPDATE threads SET parent=?1 WHERE key=?2",
            params![parent, child],
        )
        .map_err(failure)?;
    Ok(())
}

pub(super) struct NativeTurn {
    pub source: SessionRef,
    pub id: TurnId,
    pub items: std::collections::BTreeMap<agent_protocol::ids::ItemId, agent_protocol::ids::ItemId>,
}

/// Used by both the upgrade and explicit external imports. Existing unique IDs
/// survive; repeated native IDs get stable occurrence IDs within this root.
pub(super) fn write_history(
    transaction: &Transaction<'_>,
    id: &SessionRef,
    response: &ThreadResponse,
    sources: &std::collections::BTreeMap<TurnId, NativeTurn>,
    sequence: u64,
    process: Uuid,
    now: i64,
) -> Result<(), String> {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_URL, id.to_string().as_bytes());
    let key = thread_key(transaction, id)?;
    let (saved, counters) = rows::header(transaction, key)?;
    let mut state = State::new(saved.state);
    state.counters.input = counters.input;
    let mut seen_turns = BTreeSet::new();
    let mut seen_items = BTreeSet::new();
    let scope = rows::WriteScope {
        key,
        id,
        sequence,
        at: now,
        process,
    };
    let mut occurrences = std::collections::HashMap::<_, i64>::new();
    for native in response.thread.turns.iter().flatten() {
        state.counters.turn += 1;
        let turn = Turn {
            id: unique(native.id.as_str(), &mut seen_turns, namespace).into(),
            ordinal: state.counters.turn,
            provider: sources
                .get(&native.id)
                .and_then(|address| address.source.provider)
                .or(native.provider)
                .unwrap_or(state.thread.provider),
            queued: native.queued,
            observed: true,
            started_at: native
                .started_at_ms
                .and_then(|at| at.try_into().ok())
                .or_else(|| native.started_at.map(|at| (at * 1000.0) as i64)),
            completed_at: native.completed_at_ms.and_then(|at| at.try_into().ok()),
            duration_ms: native.duration_ms,
            retry_count: 0,
            compaction: false,
            error: native
                .error
                .clone()
                .filter(|_| native.status == TurnStatus::Running),
            state: match native.status {
                TurnStatus::Running => TurnState::Running { interrupt: None },
                TurnStatus::Completed => TurnState::Completed,
                TurnStatus::Failed => TurnState::Failed(native.error.clone().unwrap_or_default()),
                TurnStatus::Interrupted | TurnStatus::Unknown => TurnState::Interrupted,
            },
        };
        let address = sources.get(&native.id);
        let source_key = address
            .map(|address| {
                rows::source(
                    transaction,
                    key,
                    address.source.native_provider().map_err(failure)?,
                    &address.source.id,
                )
            })
            .transpose()?;
        state.turns.insert(turn.id.clone(), turn.clone());
        rows::write(
            transaction,
            &scope,
            &Change::Turn(turn.clone()),
            &state.turns,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
        let attempt = Uuid::new_v5(&namespace, format!("import:{}", turn.id).as_bytes());
        if let Some(source_key) = source_key {
            let native_id = &address.expect("mapped source").id;
            let occurrence = occurrences
                .entry((source_key, native_id.clone()))
                .or_default();
            transaction.execute("INSERT INTO executions(thread,turn_id,attempt,source_id,native_turn_id,current,process,started_seq,native_position) VALUES (?1,?2,?3,?4,?5,0,?6,?7,?8)", params![key,turn.id.as_str(),attempt.to_string(),source_key,native_id.as_str(),Uuid::nil().to_string(),integer(sequence)?,*occurrence]).map_err(failure)?;
            *occurrence += 1;
        }
        let native_items = native.items.as_deref().unwrap_or_default();
        for (index, native) in native_items.iter().enumerate() {
            if native_items[index + 1..]
                .iter()
                .any(|later| later.id == native.id)
            {
                continue;
            }
            state.counters.item += 1;
            let item = Item {
                id: unique(native.id.as_str(), &mut seen_items, namespace).into(),
                turn: turn.id.clone(),
                position: state.counters.item,
                status: native.status,
                deferred: native.is_deferred(),
                input: native.client_input_id.clone(),
                body: native.body().clone(),
            };
            rows::write(
                transaction,
                &scope,
                &Change::Item(item.clone()),
                &state.turns,
                &mut Vec::new(),
                &mut Vec::new(),
            )?;
            if let Some(address) = address {
                let native_id = address.items.get(&native.id).unwrap_or(&native.id);
                transaction.execute("INSERT INTO item_natives(thread,turn_id,attempt,native_id,item_id) VALUES (?1,?2,?3,?4,?5)", params![key,turn.id.as_str(),attempt.to_string(),native_id.as_str(),item.id.as_str()]).map_err(failure)?;
            }
        }
    }
    state.counters.turn = state.counters.turn.max(counters.turn);
    state.counters.item = state.counters.item.max(counters.item);
    let updated_at = header(response, now).details.updated_at;
    rows::write_header(
        transaction,
        &rows::WriteScope {
            key,
            id,
            sequence,
            at: updated_at,
            process,
        },
        &state,
        None,
        None,
    )?;
    Ok(())
}

pub(super) fn fork_hint(
    transaction: &Connection,
    key: i64,
    through: &TurnId,
    provider: ProviderKind,
) -> Result<Option<(i64, String)>, String> {
    transaction.query_row(
        "SELECT e.source_id,e.native_turn_id FROM executions e JOIN sources s ON s.id=e.source_id WHERE e.thread=?1 AND e.turn_id=?2 AND s.provider=?3 AND e.native_turn_id IS NOT NULL ORDER BY e.started_seq DESC LIMIT 1",
        params![key, through.as_str(), provider.key()], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional().map_err(failure)
}

/// A fork keeps the selected items' native addresses, including bodies which
/// have not been read yet. Its executions are historical and never accept callbacks.
fn copy_executions(transaction: &Transaction<'_>, source: i64, target: i64) -> Result<(), String> {
    let mut statement = transaction.prepare(
        "SELECT e.turn_id,e.attempt FROM executions e JOIN turns t ON t.thread=?2 AND t.id=e.turn_id WHERE e.thread=?1"
    ).map_err(failure)?;
    let executions = statement
        .query_map(params![source, target], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for (turn, previous) in executions {
        let attempt = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO executions(thread,turn_id,attempt,source_id,native_turn_id,submitted,callbacks,native_position,started_seq,current,process) SELECT ?1,turn_id,?2,source_id,native_turn_id,0,0,native_position,started_seq,0,?3 FROM executions WHERE thread=?4 AND turn_id=?5 AND attempt=?6",
            params![target,attempt,Uuid::nil().to_string(),source,turn,previous],
        ).map_err(failure)?;
        transaction.execute(
            "INSERT INTO item_natives(thread,turn_id,attempt,native_id,item_id) SELECT ?1,n.turn_id,?2,n.native_id,n.item_id FROM item_natives n JOIN items i ON i.thread=?1 AND i.id=n.item_id WHERE n.thread=?3 AND n.turn_id=?4 AND n.attempt=?5",
            params![target,attempt,source,turn,previous],
        ).map_err(failure)?;
    }
    Ok(())
}

/// Install initial history and its native binding in the caller's transaction.
pub(super) fn install(
    transaction: &Transaction<'_>,
    id: &SessionRef,
    response: &ThreadResponse,
    source: Option<&SessionRef>,
    sequence: u64,
    process: Uuid,
    now: i64,
) -> Result<(), String> {
    let mut header = header(response, now);
    let parent = header.details.parent.take();
    if let Some(key) = existing_key(transaction, id)? {
        let (previous_header, counters) = rows::header(transaction, key)?;
        header.state.queue_held = previous_header.state.queue_held;
        let previous = previous_header.details;
        header.details.read_through = previous.read_through;
        header.details.selection = decision::records::Selection {
            model: previous.selection.model.or(header.details.selection.model),
            effort: previous
                .selection
                .effort
                .or(header.details.selection.effort),
            service_tier: previous
                .selection
                .service_tier
                .or(header.details.selection.service_tier),
        };
        let parent: Option<i64> = None;
        transaction.execute("UPDATE threads SET seq=?1,loaded=1,parent=?2,cwd=?3,updated_at=?4,history_seq=?1,body=?5 WHERE key=?6", params![integer(sequence)?,parent,header.state.workspace.cwd(),header.details.updated_at,codec::header(&header,&counters)?,key]).map_err(failure)?;
    } else {
        insert_header(transaction, id, &header, &Counters::default(), sequence)?;
    }
    link_parent(
        transaction,
        thread_key(transaction, id)?,
        parent.as_ref(),
        sequence,
    )?;
    let sources = response
        .thread
        .turns
        .iter()
        .flatten()
        .filter_map(|turn| {
            source.map(|source| {
                (
                    turn.id.clone(),
                    NativeTurn {
                        source: source.clone(),
                        id: turn.id.clone(),
                        items: Default::default(),
                    },
                )
            })
        })
        .collect();
    write_history(transaction, id, response, &sources, sequence, process, now)?;
    if let Some(source) = source {
        let key = thread_key(transaction, id)?;
        let state = rows::load(transaction, key, process)?;
        rows::write(
            transaction,
            &rows::WriteScope {
                key,
                id,
                sequence,
                at: now,
                process,
            },
            &Change::Binding {
                provider: source.native_provider().map_err(failure)?,
                value: Some(decision::Binding::Ready {
                    covered: state.history_end(),
                    source: source.id.clone(),
                }),
            },
            &state.turns,
            &mut Vec::new(),
            &mut Vec::new(),
        )?;
    }
    Ok(())
}

impl Storage {
    pub(in crate::host_rpc) fn fork(
        &self,
        source: &SessionRef,
        target: &SessionRef,
        through: &TurnId,
        now: i64,
    ) -> Result<(), String> {
        if target.provider.is_some() || Uuid::parse_str(&target.id).is_err() {
            return Err("fork target must be a new UUID".into());
        }
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            // The target UUID is the operation ID: an uncertain reply is safe to retry.
            if let Some((kind, owner, outcome)) = transaction.query_row("SELECT provider,thread_id,outcome FROM receipts WHERE operation_id=?1", [target.id.as_str()], |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,String>(1)?,row.get::<_,Vec<u8>>(2)?))).optional().map_err(failure)? {
                if kind.as_deref() != target.provider.map(ProviderKind::key) || owner != target.id || protocol::decode::<Outcome>(&outcome).map_err(failure)? != Outcome::Accepted { return Err("fork UUID was already used".into()); }
                return Ok(());
            }
            if existing_key(&transaction, target)?.is_some() { return Err("fork target already exists".into()); }
            let source_key = thread_key(&transaction, source)?;
            let turn = rows::turn(&transaction, source_key, through)?.ok_or("fork turn does not exist")?;
            let busy: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE thread=?1 AND ordinal<=?2 AND state IN ('running','waiting'))", params![source_key, integer(turn.ordinal)?], |row| row.get(0)).map_err(failure)?;
            if busy { return Err("finish the selected turn before forking".into()); }
            let (mut header, _) = rows::header(&transaction, source_key)?;
            header.state.provider = turn.provider;
            header.state.queue_held = false;
            header.details.parent = None;
            header.details.read_through = None;
            header.details.agent = None;
            header.details.can_accept_direct_input = Some(true);
            header.details.activity = Default::default();
            header.details.updated_at = now;
            if header.details.selection.model.as_ref().is_some_and(|model| model.provider != turn.provider) { header.details.selection = Default::default(); }
            let sequence = Self::advance(&transaction)?;
            let target_key = insert_header(&transaction, target, &header, &Counters::default(), sequence.sequence)?;
            transaction.execute("INSERT INTO turns(id,thread,seq,ordinal,state,body) SELECT id,?1,?2,ordinal,state,body FROM turns WHERE thread=?3 AND ordinal<=?4 AND state!='rolled_back'", params![target_key, integer(sequence.sequence)?, source_key, integer(turn.ordinal)?]).map_err(failure)?;
            transaction.execute("INSERT INTO items(id,thread,turn_id,seq,position,status,body) SELECT items.id,?1,items.turn_id,?2,items.position,items.status,items.body FROM items JOIN turns ON turns.thread=items.thread AND turns.id=items.turn_id WHERE items.thread=?3 AND turns.ordinal<=?4 AND turns.state!='rolled_back'", params![target_key, integer(sequence.sequence)?, source_key, integer(turn.ordinal)?]).map_err(failure)?;
            copy_executions(&transaction, source_key, target_key)?;
            // Native history preserves tool context when the selected prefix
            // has a native turn. The provider creates a separate resource at
            // the next input; unsupported providers use the portable history.
            if let Some((source, through)) = fork_hint(&transaction, source_key, through, turn.provider)? {
                transaction.execute("INSERT INTO bindings(thread,provider,source_id,covered,state,body) VALUES (?1,?2,?3,?4,'fork',?5)", params![target_key,turn.provider.key(),source,integer(turn.ordinal)?,protocol::encode(TurnId::from(through)).map_err(failure)?]).map_err(failure)?;
            }
            let mut state = State::new(header.state);
            state.counters.turn = turn.ordinal;
            state.counters.item = transaction.query_row("SELECT coalesce(max(position),0) FROM items WHERE thread=?1", [target_key], |row| row.get::<_,i64>(0)).map_err(failure)?.try_into().map_err(failure)?;
            rows::write_header(&transaction, &rows::WriteScope { key: target_key, id: target, sequence: sequence.sequence, at: now, process: self.process }, &state, None, None)?;
            transaction.execute("INSERT INTO receipts(operation_id,provider,thread_id,seq,outcome) VALUES (?1,?2,?3,?4,?5)", params![target.id, target.provider.map(ProviderKind::key), target.id, integer(sequence.sequence)?, protocol::encode(Outcome::Accepted).map_err(failure)?]).map_err(failure)?;
            transaction.commit().map_err(failure)
        })
    }

    pub(in crate::host_rpc) fn read_page(
        &self,
        id: &SessionRef,
        cached: Cursor,
        window: Window,
        item: Option<&agent_protocol::ids::ItemId>,
    ) -> Result<(Vec<Message>, Window, Cursor), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let initial = Self::view(&transaction, id, Some(cached), Some(window))?;
            let (current, mut floor) = match &initial {
                Message::Snapshot {
                    cursor,
                    window: Some(window),
                    ..
                } => (*cursor, *window),
                Message::Changes { through, .. } => (*through, window),
                _ => return Err("conversation does not exist".into()),
            };
            let mut messages = vec![initial];
            if let Some(item) = item {
                if let Some(row) = rows::item_row(&transaction, key, id, item)?
                    && agent_protocol::sync::contains(
                        &Scope::Conversation(id.clone()),
                        Some(floor),
                        &row.key,
                        &row.position,
                    )
                {
                    messages.push(Message::Hydrate {
                        cursor: current,
                        row,
                    });
                }
            } else if floor.turn != 0 || floor.item != 0 {
                floor = rows::older_window(&transaction, key, floor)?;
                let mut rows = rows::snapshot(&transaction, key, id, floor)?;
                rows.retain(|row| {
                    !agent_protocol::sync::contains(
                        &Scope::Conversation(id.clone()),
                        Some(window),
                        &row.key,
                        &row.position,
                    )
                });
                decision::records::summarize(&mut rows, 128 * 1024)?;
                messages.push(Message::Extend {
                    cursor: current,
                    window: floor,
                    rows,
                });
            }
            transaction.commit().map_err(failure)?;
            Ok((messages, floor, current))
        })
    }

    #[cfg(test)]
    pub(in crate::host_rpc) fn import(
        &self,
        id: &SessionRef,
        response: &ThreadResponse,
        source: Option<&SessionRef>,
        now: i64,
    ) -> Result<bool, String> {
        agent_protocol::operations::validate_thread(response, Some(id)).map_err(str::to_owned)?;
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let existing = existing_key(&transaction, id)?;
            if existing.is_some_and(|key| {
                transaction
                    .query_row("SELECT loaded=1 FROM threads WHERE key=?1", [key], |row| {
                        row.get::<_, bool>(0)
                    })
                    .unwrap_or(true)
            }) {
                return Ok(false);
            }
            let sequence = Self::advance(&transaction)?;
            install(
                &transaction,
                id,
                response,
                source,
                sequence.sequence,
                self.process,
                now,
            )?;
            transaction.commit().map_err(failure)?;
            Ok(true)
        })
    }

    pub(in crate::host_rpc) fn history(
        &self,
        id: &SessionRef,
        after: u64,
    ) -> Result<ThreadResponse, String> {
        let message = self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let window = Window {
                turn: if after == 0 {
                    0
                } else {
                    after.saturating_add(1)
                },
                item: 0,
            };
            let message = Message::Snapshot {
                cursor: cursor(&transaction)?,
                window: Some(window),
                rows: rows::snapshot(&transaction, key, id, window)?,
            };
            transaction.commit().map_err(failure)?;
            Ok(message)
        })?;
        let replica = decision::records::Replica::new(Scope::Conversation(id.clone()), 1)
            .apply(1, message)
            .map_err(failure)?;
        decision::records::project(&replica).ok_or("conversation does not exist".into())
    }
}
