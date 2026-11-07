//! Runs and display items are stored separately; bodies never enter changes.
use super::*;
use agent_protocol::{
    execution::TurnStatus,
    ids::{ItemId, TurnId},
    models::{Item, SessionStatus, Turn},
};
use rusqlite::OptionalExtension;
use std::sync::Arc;

const WINDOW_BYTES: usize = 1 << 20;
pub(crate) struct Window {
    pub response: ThreadResponse,
    pub start: usize,
}

fn metadata(
    connection: &Connection,
    session: &SessionRef,
    imported_only: bool,
) -> Result<Option<ThreadResponse>, String> {
    connection.query_row("SELECT metadata_json, host_title, host_preview FROM threads WHERE thread_id = ?1 AND (?2 = 0 OR imported = 1)", params![key(session), imported_only], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)))
        .optional().map_err(failure)?.map(|(json, title, preview)| {
            let mut value: ThreadResponse = decode(&json)?;
            value.thread.id = Some(session.clone());
            if title.is_some() { value.thread.name = title; }
            if preview.is_some() { value.thread.preview = preview; }
            Ok(value)
        }).transpose()
}

fn read_turn(
    connection: &Connection,
    run_id: &str,
    json: &str,
    items_known: bool,
) -> Result<Turn, String> {
    let mut turn: Turn = decode(json)?;
    if items_known {
        let mut query = connection
            .prepare_cached(
                "SELECT payload_json FROM turn_items WHERE run_id = ?1 ORDER BY ordinal",
            )
            .map_err(failure)?;
        turn.items = Some(
            query
                .query_map([run_id], |row| row.get::<_, String>(0))
                .map_err(failure)?
                .map(|value| decode(&value.map_err(failure)?).map(Arc::new))
                .collect::<Result<_, String>>()?,
        );
    }
    Ok(turn)
}

fn runs(connection: &Connection, session: &SessionRef) -> Result<Vec<(String, Turn)>, String> {
    let mut query = connection.prepare("SELECT run_id, payload_json, items_known FROM runs WHERE thread_id = ?1 ORDER BY ordinal").map_err(failure)?;
    query
        .query_map([key(session)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .map_err(failure)?
        .map(|row| {
            let (id, json, known) = row.map_err(failure)?;
            let turn = read_turn(connection, &id, &json, known)?;
            Ok((id, turn))
        })
        .collect()
}

fn count(connection: &Connection, session: &SessionRef) -> Result<usize, String> {
    connection
        .query_row(
            "SELECT coalesce(max(ordinal) + 1, 0) FROM runs WHERE thread_id = ?1",
            [key(session)],
            |row| row.get(0),
        )
        .map_err(failure)
}

fn find_turn<T>(
    connection: &Connection,
    session: &SessionRef,
    turn_id: &TurnId,
    imported_only: bool,
    select: impl Fn(Turn) -> Option<T>,
) -> Result<Option<(usize, T)>, String> {
    let mut query = connection.prepare_cached("SELECT run_id, ordinal, payload_json, items_known FROM runs JOIN threads USING (thread_id) WHERE thread_id = ?1 AND native_turn_id = ?2 AND (?3 = 0 OR imported = 1) ORDER BY ordinal DESC").map_err(failure)?;
    let mut rows = query
        .query(params![key(session), turn_id.as_str(), imported_only])
        .map_err(failure)?;
    while let Some(row) = rows.next().map_err(failure)? {
        let turn = read_turn(
            connection,
            &row.get::<_, String>(0).map_err(failure)?,
            &row.get::<_, String>(2).map_err(failure)?,
            row.get(3).map_err(failure)?,
        )?;
        if let Some(value) = select(turn) {
            return Ok(Some((row.get(1).map_err(failure)?, value)));
        }
    }
    Ok(None)
}

pub(in crate::host_rpc) fn write_turn(
    connection: &Connection,
    session: &SessionRef,
    ordinal: usize,
    turn: &Turn,
    identity: Option<&str>,
) -> Result<(), String> {
    ensure_thread(connection, session)?;
    let existing: Option<String> = connection
        .query_row(
            "SELECT run_id FROM runs WHERE thread_id = ?1 AND ordinal = ?2",
            params![key(session), ordinal],
            |row| row.get(0),
        )
        .optional()
        .map_err(failure)?;
    let run_id = identity
        .map(str::to_owned)
        .or(existing)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut header = turn.clone();
    header.items = None;
    connection.execute("INSERT INTO runs (run_id, thread_id, ordinal, payload_json, items_known) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(run_id) DO UPDATE SET ordinal = excluded.ordinal, payload_json = excluded.payload_json, items_known = max(runs.items_known, excluded.items_known)", params![run_id, key(session), ordinal, encode(header)?, turn.items.is_some()]).map_err(failure)?;
    if let Some(items) = &turn.items {
        let mut query = connection
            .prepare_cached("SELECT item_id FROM turn_items WHERE run_id = ?1 ORDER BY ordinal")
            .map_err(failure)?;
        let old = query
            .query_map([&run_id], |row| row.get::<_, String>(0))
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?;
        for removed in old
            .iter()
            .filter(|id| !items.iter().any(|item| item.id.as_str() == id.as_str()))
        {
            let entity = format!("{run_id}:{removed}");
            deleted(connection, session, "item", &entity)?;
        }
        if !old
            .iter()
            .map(String::as_str)
            .eq(items.iter().map(|item| item.id.as_str()))
        {
            connection
                .execute("DELETE FROM turn_items WHERE run_id = ?1", [&run_id])
                .map_err(failure)?;
        }
        for (position, item) in items.iter().enumerate() {
            let updated = connection.execute("INSERT INTO turn_items (run_id, ordinal, payload_json) VALUES (?1, ?2, ?3)
                ON CONFLICT(run_id, ordinal) DO UPDATE SET payload_json = excluded.payload_json WHERE turn_items.payload_json <> excluded.payload_json", params![run_id, position, encode(item)?]).map_err(failure)?;
            if updated > 0 {
                changed(
                    connection,
                    session,
                    "item",
                    &format!("{run_id}:{}", item.id),
                )?;
            }
        }
    }
    changed(connection, session, "run", &run_id)
}

impl Database {
    pub(crate) fn import(
        &self,
        session: &SessionRef,
        mut response: ThreadResponse,
    ) -> Result<(), String> {
        let native = response.thread.turns.take().unwrap_or_default();
        response.thread.id = Some(session.clone());
        response.thread.status = SessionStatus::Unknown;
        response.thread.history_cursor = None;
        response.thread.history_has_more = None;
        response.thread.history_read_state = None;
        response.thread.preparation = None;
        response.thread.requests.clear();
        response.thread.submissions.clear();
        response.thread = crate::projects::titles::summary(response.thread);
        self.write(|connection| {
            ensure_thread(connection, session)?;
            let recorded = runs(connection, session)?;
            let mut consumed = std::collections::HashSet::new();
            let mut merged = Vec::new();
            for turn in &native {
                let stored = recorded
                    .iter()
                    .find(|(id, stored)| !consumed.contains(id) && turn.matches_occurrence(stored));
                if let Some((id, _)) = stored {
                    consumed.insert(id.clone());
                }
                merged.push(match stored {
                    Some((id, stored)) => (
                        Some(id.as_str()),
                        Turn {
                            provider: stored.provider.or(turn.provider),
                            supports_fork: stored.supports_fork.or(turn.supports_fork),
                            ..stored.clone()
                        },
                    ),
                    None => (None, turn.as_ref().clone()),
                });
            }
            merged.extend(
                recorded
                    .iter()
                    .filter(|(_, stored)| {
                        !native.iter().any(|turn| turn.matches_occurrence(stored))
                    })
                    .map(|(id, turn)| (Some(id.as_str()), turn.clone())),
            );
            for (id, turn) in &recorded {
                if merged
                    .iter()
                    .any(|(identity, _)| *identity == Some(id.as_str()))
                {
                    continue;
                }
                deleted(connection, session, "run", id)?;
                for item in turn.items.iter().flatten() {
                    deleted(connection, session, "item", &format!("{id}:{}", item.id))?;
                }
            }
            connection
                .execute("DELETE FROM runs WHERE thread_id = ?1", [key(session)])
                .map_err(failure)?;
            for (position, (identity, turn)) in merged.iter().enumerate() {
                write_turn(connection, session, position, turn, *identity)?;
            }
            connection
                .execute(
                    "UPDATE threads SET imported = 1, metadata_json = ?2 WHERE thread_id = ?1",
                    params![key(session), encode(&response)?],
                )
                .map_err(failure)?;
            changed(connection, session, "thread", &key(session))
        })
    }

    pub(crate) fn record(
        &self,
        session: &SessionRef,
        turn: &Turn,
        removed_item: Option<&ItemId>,
    ) -> Result<(), String> {
        self.write(|connection| {
            let stored = find_turn(connection, session, &turn.id, false, |stored| {
                removed_item
                    .map_or_else(
                        || stored.shares_items(turn),
                        |id| stored.items.iter().flatten().any(|item| &item.id == id),
                    )
                    .then_some(stored)
            })?;
            let stored = match stored {
                None if removed_item.is_none() => {
                    find_turn(connection, session, &turn.id, false, |stored| {
                        stored.matches_occurrence(turn).then_some(stored)
                    })?
                }
                stored => stored,
            };
            let mut turn = turn.clone();
            let position = match stored {
                Some((position, stored)) => {
                    if turn.items.is_none() {
                        turn.items = stored.items;
                    }
                    position
                }
                None => count(connection, session)?,
            };
            write_turn(connection, session, position, &turn, None)
        })
    }

    pub(crate) fn update_turn(
        &self,
        session: &SessionRef,
        turn_id: &TurnId,
        item_id: Option<&ItemId>,
        change: impl FnOnce(Turn) -> Option<Turn>,
    ) -> Result<(), String> {
        self.write(|connection| {
            let Some((position, turn)) = find_turn(connection, session, turn_id, false, |turn| {
                item_id
                    .is_none_or(|id| turn.items.iter().flatten().any(|item| &item.id == id))
                    .then_some(turn)
            })?
            else {
                return Ok(());
            };
            let Some(turn) = change(turn) else {
                return Ok(());
            };
            write_turn(connection, session, position, &turn, None)
        })
    }

    pub(crate) fn interrupt_running(&self) -> Result<(), String> {
        self.write(|connection| {
            let mut query = connection.prepare("SELECT thread_id, ordinal, run_id, payload_json, items_known FROM runs WHERE status = 'running'").map_err(failure)?;
            let stopped = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, bool>(4)?))).map_err(failure)?.collect::<Result<Vec<_>, _>>().map_err(failure)?;
            for (session, position, id, json, known) in stopped {
                let mut turn = read_turn(connection, &id, &json, known)?;
                turn.status = TurnStatus::Interrupted;
                write_turn(connection, &decode(&session)?, position, &turn, None)?;
            }
            Ok(())
        })
    }

    pub(crate) fn history(&self, session: &SessionRef) -> Result<Option<ThreadResponse>, String> {
        self.read(|connection| {
            let Some(mut response) = metadata(connection, session, true)? else {
                return Ok(None);
            };
            response.thread.turns = Some(
                runs(connection, session)?
                    .into_iter()
                    .map(|(_, turn)| Arc::new(turn))
                    .collect(),
            );
            Ok(Some(response))
        })
    }

    pub(crate) fn window(
        &self,
        session: &SessionRef,
        end: Option<usize>,
        limit: usize,
    ) -> Result<Option<Window>, String> {
        self.read(|connection| {
            let Some(mut response) = metadata(connection, session, true)? else { return Ok(None); };
            let count = count(connection, session)?;
            let end = end.unwrap_or(count);
            if end > count { return Err("conversation history cursor is no longer valid".into()); }
            let mut query = connection.prepare("SELECT run_id, payload_json, items_known FROM runs WHERE thread_id = ?1 AND ordinal < ?2 ORDER BY ordinal DESC LIMIT ?3").map_err(failure)?;
            let mut rows = query.query(params![key(session), end, i64::try_from(limit).unwrap_or(i64::MAX)]).map_err(failure)?;
            let (mut turns, mut bytes) = (Vec::new(), 0);
            while let Some(row) = rows.next().map_err(failure)? {
                let turn = read_turn(connection, &row.get::<_, String>(0).map_err(failure)?, &row.get::<_, String>(1).map_err(failure)?, row.get(2).map_err(failure)?)?;
                let size = agent_protocol::protocol::encode(&turn).map_err(failure)?.len();
                if !turns.is_empty() && bytes + size > WINDOW_BYTES { break; }
                bytes += size; turns.push(Arc::new(turn));
            }
            turns.reverse();
            let start = end - turns.len();
            response.thread.turns = Some(turns);
            response.thread.transcript_version = connection.query_row("SELECT max(sequence) FROM changes WHERE thread_id = ?1", [key(session)], |row| row.get(0)).map_err(failure)?;
            Ok(Some(Window { response, start }))
        })
    }

    pub(crate) fn imported(&self) -> Result<Vec<(SessionRef, Option<String>)>, String> {
        self.read(|connection| {
            let mut query = connection
                .prepare("SELECT thread_id, cwd FROM public_threads WHERE imported = 1")
                .map_err(failure)?;
            query
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))
                .map_err(failure)?
                .map(|row| {
                    let (id, cwd) = row.map_err(failure)?;
                    Ok((decode(&id)?, cwd))
                })
                .collect()
        })
    }

    pub(crate) fn turn(
        &self,
        session: &SessionRef,
        turn_id: &TurnId,
    ) -> Result<Option<Turn>, String> {
        self.read(|connection| {
            find_turn(connection, session, turn_id, true, Some)
                .map(|found| found.map(|(_, turn)| turn))
        })
    }

    pub(crate) fn item(
        &self,
        session: &SessionRef,
        turn_id: &TurnId,
        item_id: &ItemId,
        imported_only: bool,
    ) -> Result<Option<Arc<Item>>, String> {
        self.read(|connection| {
            find_turn(connection, session, turn_id, imported_only, |turn| {
                turn.items?.into_iter().find(|item| &item.id == item_id)
            })
            .map(|found| found.map(|(_, item)| item))
        })
    }

    pub(crate) fn rename(&self, session: &SessionRef, name: Option<&str>) -> Result<(), String> {
        self.write(|connection| {
            let Some(mut response) = metadata(connection, session, false)? else {
                return Ok(());
            };
            response.thread.name = name.map(str::to_owned);
            connection
                .execute(
                    "UPDATE threads SET metadata_json = ?2 WHERE thread_id = ?1",
                    params![key(session), encode(response)?],
                )
                .map_err(failure)?;
            changed(connection, session, "thread", &key(session))
        })
    }

    pub(in crate::host_rpc) fn observe(&self, thread: &Thread) -> Result<(), String> {
        let Some(session) = &thread.id else {
            return Ok(());
        };
        self.write(|connection| {
            ensure_thread(connection, session)?;
            let mut saved = metadata(connection, session, false)?.expect("thread exists");
            saved.thread = crate::projects::titles::summary(thread.clone());
            saved.thread.status = SessionStatus::Unknown;
            saved.thread.list_stale = None;
            connection
                .execute(
                    "UPDATE threads SET listed = 1, metadata_json = ?2 WHERE thread_id = ?1",
                    params![key(session), encode(saved)?],
                )
                .map_err(failure)?;
            changed(connection, session, "thread", &key(session))
        })
    }

    pub(in crate::host_rpc) fn titles(&self, search: &str) -> Result<Vec<StoredTitle>, String> {
        let search = search.trim().to_lowercase();
        self.read(|connection| {
            let mut query = connection.prepare("SELECT thread_id, managed FROM public_threads WHERE listed = 1 OR managed = 1 OR imported = 1 ORDER BY updated_at_ms DESC, thread_id").map_err(failure)?;
            let ids = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))).map_err(failure)?.collect::<Result<Vec<_>, _>>().map_err(failure)?;
            let mut titles = Vec::new();
            for (id, managed) in ids {
                let mut thread = metadata(connection, &decode(&id)?, false)?.expect("listed thread exists").thread;
                if !format!("{} {}", thread.name.as_deref().unwrap_or_default(), thread.preview.as_deref().unwrap_or_default()).to_lowercase().contains(&search) { continue; }
                let startup: Option<String> = connection.query_row("SELECT payload_json FROM thread_startups WHERE thread_id = ?1", [&id], |row| row.get(0)).optional().map_err(failure)?;
                if let Some(startup) = startup { let startup: crate::host_rpc::conversations::Startup = decode(&startup)?; startup.describe(&mut thread); }
                let mut sources = Vec::new();
                if managed {
                    let mut query = connection.prepare_cached("SELECT provider_thread_id FROM provider_threads WHERE thread_id = ?1 UNION SELECT provider_thread_id FROM history_segments WHERE thread_id = ?1").map_err(failure)?;
                    for source in query.query_map([&id], |row| row.get::<_, String>(0)).map_err(failure)? {
                        let source: SessionRef = decode(&source.map_err(failure)?)?;
                        if !sources.contains(&source.provider) { sources.push(source.provider); }
                    }
                }
                titles.push(StoredTitle { thread, managed, sources });
            }
            Ok(titles)
        })
    }
}

pub(in crate::host_rpc) struct StoredTitle {
    pub thread: Thread,
    pub managed: bool,
    pub sources: Vec<agent_protocol::session::ProviderKind>,
}
