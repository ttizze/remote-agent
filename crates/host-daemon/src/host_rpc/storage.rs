//! One transactional writer for conversation rows, receipts and durable jobs.
use agent_protocol::{
    conversation::{
        self as decision, Change, Context, Counters, Event, Outcome, State, Step,
        records::{Header, Message, Record, Row},
    },
    ids::{ClientInputId, TurnId},
    protocol,
    session::{ProviderKind, SessionRef},
    sync::{Content, Cursor, Key, Position, Window},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

mod activity;
mod bodies;
mod catalog;
mod codec;
mod executions;
pub(in crate::host_rpc) mod history;
mod imports;
pub(super) mod jobs;
mod journal;
mod list;
mod projects;
pub(super) mod pull_requests;
mod rows;
#[cfg(test)]
mod tests;
mod titles;
pub(in crate::host_rpc) mod workspace;

#[derive(Debug)]
pub(crate) struct Storage {
    connection: Mutex<Result<Connection, String>>,
    process: Uuid,
    changed: tokio::sync::watch::Sender<Option<Cursor>>,
}

#[derive(Debug)]
pub(super) struct Committed {
    pub step: Step,
    pub cursor: Cursor,
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn integer(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "conversation sequence exhausted".into())
}

fn provider(value: &str) -> Result<ProviderKind, String> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(failure)
}

fn cursor(connection: &Connection) -> Result<Cursor, String> {
    connection
        .query_row("SELECT id, seq FROM store", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(failure)
        .and_then(|(id, sequence)| {
            Ok(Cursor {
                store: Uuid::parse_str(&id).map_err(failure)?,
                sequence: u64::try_from(sequence).map_err(failure)?,
            })
        })
}

fn thread_key(connection: &Connection, id: &SessionRef) -> Result<i64, String> {
    existing_key(connection, id)?.ok_or_else(|| "conversation does not exist".into())
}

fn existing_key(connection: &Connection, id: &SessionRef) -> Result<Option<i64>, String> {
    connection
        .query_row(
            "SELECT key FROM threads WHERE ifnull(provider,'')=ifnull(?1,'') AND id=?2",
            params![id.provider.map(ProviderKind::key), id.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)
}

impl Storage {
    pub(super) fn request_owner(
        &self,
        id: &agent_protocol::ids::RequestId,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| connection.query_row("SELECT threads.provider,threads.id FROM requests JOIN threads ON threads.key=requests.thread WHERE requests.id=?1", [id.as_str()], |row| Ok((row.get::<_,Option<String>>(0)?,row.get::<_,String>(1)?))).optional().map_err(failure)?.map(|(kind,id)| Ok(SessionRef { provider: kind.as_deref().map(provider).transpose()?, id })).transpose())
    }
    pub(super) fn receipts(
        &self,
        id: &SessionRef,
        operations: &[Uuid],
    ) -> Result<decision::records::Receipts, String> {
        if operations.len() > decision::records::CheckCommands::MAX_OPERATIONS {
            return Err("too many command receipts requested".into());
        }
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let current = cursor(&transaction)?;
            let mut receipts = Vec::new();
            for operation in operations {
                let receipt = transaction.query_row("SELECT seq,outcome FROM receipts WHERE operation_id=?1 AND provider IS ?2 AND thread_id=?3", params![operation.to_string(), id.provider.map(ProviderKind::key), id.id], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,Vec<u8>>(1)?))).optional().map_err(failure)?;
                if let Some((sequence, outcome)) = receipt {
                    let sequence = u64::try_from(sequence).map_err(failure)?;
                    if sequence > current.sequence { return Err("receipt exceeds storage cursor".into()); }
                    receipts.push((*operation, decision::records::Acknowledged { cursor: Cursor { sequence, ..current }, outcome: protocol::decode(&outcome).map_err(failure)? }));
                }
            }
            transaction.commit().map_err(failure)?;
            Ok(receipts)
        })
    }

    pub(super) fn updating(&self) -> Result<bool, String> {
        self.with(|connection| {
            connection
                .query_row("SELECT updating FROM store", [], |row| row.get(0))
                .map_err(failure)
        })
    }

    pub(super) fn set_updating(&self, updating: bool) -> Result<(), String> {
        self.with(|connection| {
            connection
                .execute("UPDATE store SET updating=?1", [updating])
                .map(|_| ())
                .map_err(failure)
        })
    }

    pub(super) fn save_source(
        &self,
        id: &SessionRef,
        provider: ProviderKind,
        source: &str,
        context: Option<&agent_protocol::handoff::Context>,
        selection: &decision::records::Selection,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let source = rows::source(&transaction, key, provider, source)?;
            transaction
                .execute(
                    "UPDATE sources SET context=?1,options=?2 WHERE id=?3",
                    params![
                        context.map(protocol::encode).transpose().map_err(failure)?,
                        protocol::encode(selection).map_err(failure)?,
                        source
                    ],
                )
                .map_err(failure)?;
            transaction.commit().map_err(failure)
        })
    }

    pub(super) fn source_selection(
        &self,
        id: &SessionRef,
        provider: ProviderKind,
        source: &str,
    ) -> Result<decision::records::Selection, String> {
        self.with(|connection| {
            let key = thread_key(connection, id)?;
            saved_selection(connection, key, provider, source)?.map_or_else(
                || rows::header(connection, key).map(|(header, _)| header.details.selection),
                Ok,
            )
        })
    }

    pub(super) fn source_context(
        &self,
        id: &SessionRef,
        provider: ProviderKind,
        source: &str,
    ) -> Result<Option<agent_protocol::handoff::Context>, String> {
        self.with(|connection| {
            let body: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT context FROM sources WHERE thread=?1 AND provider=?2 AND native_id=?3",
                    params![thread_key(connection, id)?, provider.key(), source],
                    |row| row.get(0),
                )
                .optional()
                .map_err(failure)?
                .flatten();
            body.map(|body| protocol::decode(&body).map_err(failure))
                .transpose()
        })
    }

    pub(super) fn check_ready(&self) -> Result<(), String> {
        self.with(|_| Ok(()))
    }

    pub(super) fn header(&self, id: &SessionRef) -> Result<Header, String> {
        self.lookup(id)?.ok_or("conversation does not exist".into())
    }

    pub(super) fn lookup(&self, id: &SessionRef) -> Result<Option<Header>, String> {
        self.with(|connection| {
            existing_key(connection, id)?
                .map(|key| rows::header(connection, key).map(|(header, _)| header))
                .transpose()
        })
    }

    pub(in crate::host_rpc) fn input(
        &self,
        target: &SessionRef,
        input: &ClientInputId,
    ) -> Result<Option<decision::PendingInput>, String> {
        self.with(|connection| {
            rows::input(connection, thread_key(connection, target)?, input.clone())
        })
    }
    pub(super) fn item(
        &self,
        id: &SessionRef,
        item: &agent_protocol::ids::ItemId,
    ) -> Result<Option<decision::Item>, String> {
        self.with(|connection| rows::item(connection, thread_key(connection, id)?, item))
    }

    pub(super) fn turn(
        &self,
        id: &SessionRef,
        turn: &TurnId,
    ) -> Result<Option<decision::Turn>, String> {
        self.with(|connection| rows::turn(connection, thread_key(connection, id)?, turn))
    }

    pub(super) fn identities(&self) -> Result<Vec<SessionRef>, String> {
        self.with(|connection| {
            let mut statement = connection
                .prepare("SELECT provider,id FROM threads WHERE loaded=1 ORDER BY key")
                .map_err(failure)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(failure)?;
            rows.map(|row| {
                let (kind, id) = row.map_err(failure)?;
                SessionRef::new(kind.as_deref().map(provider).transpose()?, id)
                    .map_err(str::to_owned)
            })
            .collect()
        })
    }

    pub(crate) fn open(path: &Path) -> Self {
        let process = Uuid::new_v4();
        let connection = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(failure)?;
            }
            let mut connection = Connection::open(path).map_err(failure)?;
            connection
                .busy_timeout(std::time::Duration::from_secs(10))
                .map_err(failure)?;
            connection
                .pragma_update(None, "foreign_keys", true)
                .map_err(failure)?;
            let version: u32 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(failure)?;
            match version {
                0 => Self::initialize(&mut connection)?,
                3 => {}
                _ => return Err(format!("conversation storage needs upgrade from {version}")),
            }
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                .map_err(failure)?;
            cursor(&connection)?;
            // The Host lease is already held. Running work belonged to the
            // previous process; a live process never reclaims its own claims.
            connection
                .execute("UPDATE jobs SET state='pending' WHERE state='running'", [])
                .map_err(failure)?;
            Ok(connection)
        })();
        let initial = connection
            .as_ref()
            .ok()
            .and_then(|connection| cursor(connection).ok());
        Self {
            connection: Mutex::new(connection),
            process,
            changed: tokio::sync::watch::channel(initial).0,
        }
    }

    fn initialize(connection: &mut Connection) -> Result<(), String> {
        let populated: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table')",
                [],
                |row| row.get(0),
            )
            .map_err(failure)?;
        if populated {
            return Err("unversioned conversation database requires upgrade".into());
        }
        let transaction = connection.transaction().map_err(failure)?;
        transaction
            .execute_batch(include_str!("storage/schema.sql"))
            .map_err(failure)?;
        transaction
            .execute(
                "INSERT INTO store VALUES (?1, 0, 0, 0)",
                [Uuid::new_v4().to_string()],
            )
            .map_err(failure)?;
        transaction
            .pragma_update(None, "user_version", 3)
            .map_err(failure)?;
        transaction.commit().map_err(failure)?;
        Ok(())
    }

    fn with<T>(
        &self,
        read: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut owned = self
            .connection
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let connection = owned.as_mut().map_err(|error| error.clone())?;
        let before = connection.total_changes();
        let result = read(connection);
        let written = result.is_ok() && connection.total_changes() != before;
        // Wake private resource waiters as well as streams after a successful
        // write. Private changes keep the same public cursor; read transactions
        // never notify themselves. Streams catch up from the journal.
        if let Ok(current) = cursor(connection) {
            self.changed.send_if_modified(|last| {
                if *last == Some(current) && !written {
                    return false;
                }
                *last = Some(current);
                true
            });
        }
        result
    }

    #[cfg(test)]
    pub(super) fn create(&self, id: &SessionRef, header: &Header) -> Result<bool, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            if existing_key(&transaction, id)?.is_some() {
                return Ok(false);
            }
            history::insert_header(&transaction, id, header, &Counters::default())?;
            Self::commit(
                transaction,
                Some(id),
                Step::default(),
                Vec::new(),
                Vec::new(),
                true,
            )?;
            Ok(true)
        })
    }

    pub(super) fn decide(
        &self,
        id: &SessionRef,
        event: Event,
        context: Context,
        create: Option<&Header>,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let event = self.receipted(&transaction, id, event)?;
            let (key, created) = match existing_key(&transaction, id)? {
                Some(key) => (key, false),
                None => {
                    if matches!(&event, Event::Command { receipt: Some(_), .. } | Event::RejectedCommand { receipt: Some(_), .. }) { return self.missing(transaction, id, event); }
                    let Some(header) = create else { return self.missing(transaction, id, event); };
                    (history::insert_header(&transaction,id,header,&Counters::default())?, true)
                }
            };
            if let Event::Provider {attempt,..}=&event
                && !executions::read(&transaction,"e.thread=?2 AND e.attempt=?3",params![self.process.to_string(),key,attempt.to_string()])?.first().is_some_and(|execution|execution.accepting)
            {
                return Ok(Committed {step:Step::default(),cursor:cursor(&transaction)?});
            }
            let state = rows::load(&transaction, key, self.process)?;
            let mut state = rows::hydrate(&transaction, key, self.process, state, &event)?;
            let imported_turns = if matches!(&event, Event::Restart { .. }) {
                let mut statement = transaction.prepare("SELECT t.id FROM turns t WHERE t.thread=?1 AND t.state='running' AND EXISTS(SELECT 1 FROM executions e WHERE e.thread=t.thread AND e.turn_id=t.id AND e.process=?2) AND NOT EXISTS(SELECT 1 FROM executions e WHERE e.thread=t.thread AND e.turn_id=t.id AND e.process!=?2)").map_err(failure)?;
                statement.query_map(params![key, Uuid::nil().to_string()], |row| row.get::<_, String>(0).map(TurnId::from)).map_err(failure)?.collect::<Result<Vec<_>, _>>().map_err(failure)?
            } else { Vec::new() };
            let native_turn = decision::observed_native_turn(&state.inputs, &event)
                .filter(|native| state.attempts.values().find(|attempt| attempt.id == native.execution)
                    .and_then(|attempt| state.turns.get(&attempt.turn))
                    .is_none_or(|turn| decision::current_native_turn(&turn.attempts, native.execution, &native.native)))
                .map(|native| (native.execution, native.native));
            let native_items = match &event { Event::Provider { attempt, native_items, .. } => Some((*attempt, native_items.clone())), _ => None };
            let (mut header,counters)=rows::header(&transaction,key)?;
            let previous_header = header.clone();
            let status_changed = if let Event::NativeStatus { provider, source, status } = &event
                && decision::current_source(state.thread.provider, state.bindings.get(provider), *provider, source) {
                let active = state.active_turn().map(|turn| (turn.observed, state.attempts.contains_key(&turn.id)));
                let changed = activity::native_status(&transaction, id, (*provider, source, *status), active, &mut header.details.activity, context.now)?;
                if changed { transaction.execute("UPDATE threads SET body=?1 WHERE key=?2", params![codec::header(&header,&counters)?,key]).map_err(failure)?; }
                changed
            } else { false };
            let steer_ready = executions::steering_ready(&transaction, key, self.process,
                state.active_turn().and_then(|turn| state.attempts.get(&turn.id)).map(|attempt| attempt.id),
                native_turn.as_ref().map(|(attempt, _)| *attempt))?;
            let rewind_binding = if let Event::Command { command: decision::Command::Rewind(turn), receipt: None, .. } = &event {
                let retained: Option<String> = transaction.query_row("SELECT id FROM turns WHERE thread=?1 AND state!='rolled_back' AND ordinal<(SELECT ordinal FROM turns WHERE thread=?1 AND id=?2) ORDER BY ordinal DESC LIMIT 1", params![key, turn.as_str()], |row| row.get(0)).optional().map_err(failure)?;
                retained.map(|turn| history::fork_hint(&transaction, key, &TurnId::from(turn), state.thread.provider)).transpose()?.flatten().map(|(source, through)| {
                    transaction.query_row("SELECT native_id FROM sources WHERE id=?1", [source], |row| row.get(0)).map(|source| decision::Binding::Fork { source, through: through.into() }).map_err(failure)
                }).transpose()?
            } else { None };
            let rewind_context_start = if let Event::Command {command:decision::Command::Rewind(turn),receipt:None,..} = &event {
                state.turns.get(turn).map(|turn| history::context_start_before(&transaction,key,turn.ordinal)).transpose()?
            } else {None};
            let context=Context {rewind_context_start, preparation: header.details.workspace.as_deref(), rewind_binding: rewind_binding.as_ref(), imported_turns: &imported_turns, steer_ready, external_busy:context.external_busy || state.active_turn().is_none() && header.details.activity.unobserved_running(),..context};
            let submitted = matches!(&event, Event::Command {
                id: decision::OperationId::Client(_),
                command: decision::Command::Submit { .. },
                receipt: None,
            });
            let mut step = decision::step(&state, event, context);
            let last_input_at = (submitted && step.outcome == Outcome::Accepted).then_some(context.now);
            let from = cursor(&transaction)?;
            let changed = status_changed || !step.changes.is_empty()
                || !step.continuations.is_empty();
            let through = if changed || created { Self::next_cursor(&transaction)? } else { from };
            let admitted = step.changes.iter().find_map(|change| {
                let Change::Item(item) = change else { return None; };
                if !matches!(item.body, agent_protocol::items::ItemBody::UserMessage { .. }) { return None; }
                let input = state.inputs.get(&ClientInputId::from(item.id.to_string()))?;
                let delivery = match &input.state { decision::InputState::Delivering(delivery) | decision::InputState::Unknown { delivery, .. } => delivery, _ => return None };
                Some((&input.content, &delivery.attempt))
            });
            let admitted = admitted.map(|(content, attempt)| {
                let selected = saved_selection(&transaction, key, attempt.provider, &attempt.source)?
                    .unwrap_or(header.details.selection.clone());
                let selected = selected.resolve(
                    content.model.as_ref(),
                    content.effort.as_deref(),
                    content.service_tier.as_deref(),
                );
                transaction.execute("UPDATE sources SET context=CASE WHEN ?5 THEN context ELSE NULL END,options=?1 WHERE thread=?2 AND provider=?3 AND native_id=?4", params![protocol::encode(&selected).map_err(failure)?, key, attempt.provider.key(), attempt.source, agent_protocol::operations::Input::is_compaction(&content.parts)]).map_err(failure)?;
                let preview = content.parts.iter().find_map(|part| match part {
                    agent_protocol::operations::Input::Text { text } => Some(text.chars().take(200).collect()),
                    _ => None,
                });
                Ok::<_, String>((selected, preview))
            }).transpose()?;
            state = state.apply(&step.changes);
            let mut updated = Vec::new();
            let mut removed = Vec::new();
            let scope = rows::WriteScope { key, id, sequence:through.sequence, at:context.now, process:self.process };
            for change in &step.changes {
                rows::write(&transaction,
&scope,
change,
&state.turns,
&mut updated,
&mut removed)?;
            }
            if let Some(binding) = native_turn {
                let interrupt = executions::bind_turn(&transaction, key, self.process, binding, &state.turns)?
                    .filter(|interrupt| !step.actions.contains(interrupt));
                step.actions.extend(interrupt);
            }
            if let Some((attempt, items)) = native_items {
                rows::bind_items(&transaction, key, attempt, self.process, &items)?;
            }
            if created || status_changed || !step.changes.is_empty() {
                updated.extend(rows::write_header(&transaction, &rows::WriteScope { key, id, sequence: through.sequence, at: if status_changed && step.changes.is_empty() {header.details.updated_at} else {context.now}, process: self.process }, &state, admitted, Some(&previous_header), Some(context.steers), last_input_at)?);
            }
            for continuation in &step.continuations {
                jobs::enqueue(
                    &transaction,
                    continuation.id,
                    &format!("continue:{id}"),
                    context.now,
                    &jobs::Work::Continue {
                        target: id.clone(),
                        source: continuation.source.clone(),
                    },
                )?;
            }
            Self::commit(transaction, Some(id), step, updated, removed, false)
        })
    }

    fn missing(
        &self,
        transaction: Transaction<'_>,
        id: &SessionRef,
        event: Event,
    ) -> Result<Committed, String> {
        let (Event::Command {
            id: operation,
            receipt,
            ..
        }
        | Event::RejectedCommand {
            id: operation,
            receipt,
            ..
        }) = event
        else {
            return Err("conversation does not exist".into());
        };
        let mut step = Step::default();
        if let Some(outcome) = receipt {
            step.outcome = outcome;
        } else {
            step.outcome = Outcome::Rejected("conversation does not exist".into());
            step.receipt = Some((operation, step.outcome.clone()));
        }
        Self::commit(transaction, Some(id), step, Vec::new(), Vec::new(), false)
    }

    fn receipted(
        &self,
        transaction: &Transaction<'_>,
        id: &SessionRef,
        event: Event,
    ) -> Result<Event, String> {
        let mut event = event;
        let operation = match &event {
            Event::Command { id, .. } | Event::RejectedCommand { id, .. } => *id,
            _ => return Ok(event),
        };
        let receipt = transaction
            .query_row(
                "SELECT provider,thread_id,outcome FROM receipts WHERE operation_id=?1",
                [operation.receipt_key()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(failure)?;
        let receipt = receipt
            .map(|(owner, target, body)| {
                if owner.as_deref() != id.provider.map(ProviderKind::key) || target != id.id {
                    return Err("operation belongs to another conversation".into());
                }
                protocol::decode::<Outcome>(&body).map_err(failure)
            })
            .transpose()?;
        match &mut event {
            Event::Command {
                receipt: stored, ..
            }
            | Event::RejectedCommand {
                receipt: stored, ..
            } => *stored = receipt,
            _ => unreachable!(),
        }
        Ok(event)
    }

    pub(super) fn state(&self, id: &SessionRef) -> Result<State, String> {
        self.with(|connection| rows::load(connection, thread_key(connection, id)?, self.process))
    }

    pub(super) fn snapshot(
        &self,
        id: &SessionRef,
        cached: Option<Cursor>,
        window: Option<Window>,
    ) -> Result<Message, String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let message = Self::view(&transaction, id, cached, window)?;
            transaction.commit().map_err(failure)?;
            Ok(message)
        })
    }
    fn view(
        connection: &Connection,
        id: &SessionRef,
        cached: Option<Cursor>,
        window: Option<Window>,
    ) -> Result<Message, String> {
        let Some(key) = existing_key(connection, id)? else {
            return Ok(Message::Missing);
        };
        let current = cursor(connection)?;
        if let Some(requested) = window
            && rows::window(connection, key, Some(requested))? == requested
            && let Some(message) = journal::replay(connection, Some(key), cached, current)?
        {
            return Ok(message);
        }
        let floor = rows::window(connection, key, None)?;
        let mut saved = rows::snapshot(connection, key, id, floor)?;
        decision::records::summarize(&mut saved, decision::records::WORK_DETAIL_BYTES)?;
        let message = Message::Snapshot {
            cursor: current,
            window: Some(floor),
            rows: saved,
        };
        Ok(message)
    }
}

fn saved_selection(
    connection: &Connection,
    key: i64,
    provider: ProviderKind,
    source: &str,
) -> Result<Option<decision::records::Selection>, String> {
    let body: Option<Vec<u8>> = connection
        .query_row(
            "SELECT options FROM sources WHERE thread=?1 AND provider=?2 AND native_id=?3",
            params![key, provider.key(), source],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?
        .flatten();
    body.map(|body| protocol::decode(&body).map_err(failure))
        .transpose()
}
