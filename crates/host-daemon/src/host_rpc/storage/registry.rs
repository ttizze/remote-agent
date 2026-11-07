use super::*;
use crate::host_rpc::conversations::{
    Binding, Conversation, Segment, StartupChange, change_startup, namespace_change,
    namespace_request, validate,
};
use crate::providers::AgentChange;
use agent_protocol::{models::*, session::*};
use rusqlite::OptionalExtension;

fn storage_error(error: impl std::fmt::Display) -> Failure {
    Failure::new("conversation_storage_failed", error)
}

pub(in crate::host_rpc) fn rows(
    connection: &Connection,
    selected: Option<&SessionRef>,
    include_handoffs: bool,
) -> Result<Vec<Conversation>, String> {
    let sql = if selected.is_some() {
        "SELECT thread_id, active_provider_thread_id, metadata_json, host_title, host_preview FROM threads WHERE managed = 1 AND thread_id = ?1"
    } else {
        "SELECT thread_id, active_provider_thread_id, metadata_json, host_title, host_preview FROM threads WHERE managed = 1 ORDER BY thread_id"
    };
    let mut query = connection.prepare_cached(sql).map_err(failure)?;
    let selected = selected.map(key);
    let mut result = Vec::new();
    let mut found = query
        .query(rusqlite::params_from_iter(selected.as_deref()))
        .map_err(failure)?;
    while let Some(row) = found.next().map_err(failure)? {
        let id: String = row.get(0).map_err(failure)?;
        let response: ThreadResponse = decode(&row.get::<_, String>(2).map_err(failure)?)?;
        let mut bindings = connection.prepare("SELECT provider_thread_id, covered_runs FROM provider_threads WHERE thread_id = ?1 ORDER BY ordinal").map_err(failure)?;
        let bindings = bindings
            .query_map([&id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<usize>>(1)?))
            })
            .map_err(failure)?
            .map(|row| {
                let (session, covered) = row.map_err(failure)?;
                Ok(Binding {
                    session: decode(&session)?,
                    covered,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut segments = connection.prepare("SELECT provider_thread_id, start_run, end_run FROM history_segments WHERE thread_id = ?1 ORDER BY ordinal").map_err(failure)?;
        let segments = segments
            .query_map([&id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, usize>(1)?,
                    row.get::<_, Option<usize>>(2)?,
                ))
            })
            .map_err(failure)?
            .map(|row| {
                let (session, from, end) = row.map_err(failure)?;
                Ok(Segment {
                    session: decode(&session)?,
                    from,
                    end,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let handoffs = if include_handoffs {
            let mut handoffs = connection
            .prepare(
                "SELECT payload_json FROM context_handoffs WHERE thread_id = ?1 ORDER BY ordinal",
            )
            .map_err(failure)?;
            handoffs
                .query_map([&id], |row| row.get::<_, String>(0))
                .map_err(failure)?
                .map(|row| decode(&row.map_err(failure)?))
                .collect::<Result<Vec<_>, String>>()?
        } else {
            Vec::new()
        };
        let startup = connection
            .query_row(
                "SELECT payload_json FROM thread_startups WHERE thread_id = ?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(failure)?
            .map(|value| decode(&value))
            .transpose()?;
        result.push(Conversation {
            id: decode(&id)?,
            active: decode(
                &row.get::<_, Option<String>>(1)
                    .map_err(failure)?
                    .unwrap_or_else(|| id.clone()),
            )?,
            cwd: response
                .thread
                .cwd
                .ok_or("managed conversation has no workspace")?,
            name: row.get(3).map_err(failure)?,
            preview: row.get(4).map_err(failure)?,
            bindings,
            segments,
            handoffs,
            startup,
        });
    }
    Ok(result)
}

pub(in crate::host_rpc) fn save(connection: &Connection, row: &Conversation) -> Result<(), String> {
    validate(std::slice::from_ref(row))?;
    if native_owner(connection, &row.id)?.is_some_and(|owner| owner != row.id) {
        return Err("The public conversation is already a native binding".into());
    }
    for binding in &row.bindings {
        let reserved: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM threads WHERE managed = 1 AND thread_id = ?1 AND thread_id <> ?2)", params![key(&binding.session), key(&row.id)], |row| row.get(0)).map_err(failure)?;
        if reserved
            || native_owner(connection, &binding.session)?.is_some_and(|owner| owner != row.id)
        {
            return Err("Duplicate native conversation owner".into());
        }
    }
    ensure_thread(connection, &row.id)?;
    let id = key(&row.id);
    let mut metadata: ThreadResponse = decode(
        &connection
            .query_row(
                "SELECT metadata_json FROM threads WHERE thread_id = ?1",
                [&id],
                |row| row.get::<_, String>(0),
            )
            .map_err(failure)?,
    )?;
    metadata.thread.id = Some(row.id.clone());
    metadata.thread.cwd = Some(row.cwd.clone());
    metadata.thread.active_provider = Some(row.active.provider);
    connection.execute("UPDATE threads SET metadata_json = ?2, managed = 1, active_provider_thread_id = ?3, host_title = ?4, host_preview = ?5 WHERE thread_id = ?1", params![id, encode(metadata)?, (!row.bindings.is_empty()).then(|| key(&row.active)), row.name, row.preview]).map_err(failure)?;
    for (ordinal, binding) in row.bindings.iter().enumerate() {
        connection.execute("INSERT INTO provider_threads (provider_thread_id, thread_id, ordinal, covered_runs) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(provider_thread_id) DO UPDATE SET covered_runs = excluded.covered_runs", params![key(&binding.session), id, ordinal, binding.covered]).map_err(failure)?;
    }
    connection
        .execute("DELETE FROM history_segments WHERE thread_id = ?1", [&id])
        .map_err(failure)?;
    for (ordinal, segment) in row.segments.iter().enumerate() {
        connection.execute("INSERT INTO history_segments (thread_id, ordinal, provider_thread_id, start_run, end_run) VALUES (?1, ?2, ?3, ?4, ?5)", params![id, ordinal, key(&segment.session), segment.from, segment.end]).map_err(failure)?;
    }
    connection
        .execute("DELETE FROM context_handoffs WHERE thread_id = ?1", [&id])
        .map_err(failure)?;
    for (ordinal, handoff) in row.handoffs.iter().enumerate() {
        connection.execute("INSERT INTO context_handoffs (thread_id, ordinal, payload_json) VALUES (?1, ?2, ?3)", params![id, ordinal, encode(handoff)?]).map_err(failure)?;
    }
    connection
        .execute("DELETE FROM thread_startups WHERE thread_id = ?1", [&id])
        .map_err(failure)?;
    if let Some(startup) = &row.startup {
        connection
            .execute(
                "INSERT INTO thread_startups (thread_id, payload_json) VALUES (?1, ?2)",
                params![id, encode(startup)?],
            )
            .map_err(failure)?;
    }
    changed(connection, &row.id, "binding", &id)
}

impl Database {
    pub(in crate::host_rpc) fn recover_conversations(&self) -> Result<(), Failure> {
        self.write(|connection| {
            let mut recovery = connection.prepare("UPDATE command_receipts SET delivery_json = '\"unknown\"' WHERE delivery_json = '\"sending\"' RETURNING thread_id, command_id").map_err(storage_error)?;
            let recovered = recovery.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(storage_error)?.collect::<Result<Vec<_>, _>>().map_err(storage_error)?;
            for (session, command) in recovered { changed(connection, &decode(&session).map_err(storage_error)?, "command", &command).map_err(storage_error)?; }
            for mut row in rows(connection, None, true).map_err(storage_error)? {
                let mut changed = false;
                if let Some(startup) = &mut row.startup {
                    match startup.preparation.status {
                        PreparationStatus::Preparing => {
                            startup.preparation.status = PreparationStatus::Failed {
                                error: ExecutionError {
                                    message:
                                        "ホストの再起動で準備が中断されました。再試行してください。"
                                            .into(),
                                    ..Default::default()
                                },
                            };
                            changed = true;
                        }
                        PreparationStatus::Sending => {
                            startup.preparation.status = PreparationStatus::Unknown;
                            changed = true;
                        }
                        _ => {}
                    }
                }
                for handoff in &mut row.handoffs {
                    if handoff.submission == SubmissionDelivery::Sending {
                        handoff.submission = SubmissionDelivery::Unknown;
                        changed = true;
                    }
                }
                if changed {
                    save(connection, &row).map_err(storage_error)?;
                }
            }
            Ok(())
        })
    }

    pub(in crate::host_rpc) fn save(&self, conversation: Conversation) -> Result<(), Failure> {
        self.write(|connection| save(connection, &conversation).map_err(storage_error))
    }

    pub(in crate::host_rpc) fn admit(
        &self,
        conversation: Conversation,
    ) -> Result<(Conversation, bool), Failure> {
        self.write(|connection| {
            if let Some(row) = rows(connection, Some(&conversation.id), true)
                .map_err(storage_error)?
                .pop()
            {
                return Ok((row, false));
            }
            save(connection, &conversation).map_err(storage_error)?;
            // Admission creates a new, empty app conversation. Its full history
            // is already Host-owned before the provider starts its first run.
            connection
                .execute(
                    "UPDATE threads SET imported = 1 WHERE thread_id = ?1",
                    [key(&conversation.id)],
                )
                .map_err(storage_error)?;
            Ok((conversation, true))
        })
    }

    pub(in crate::host_rpc) fn update_startup(
        &self,
        id: &SessionRef,
        change: StartupChange,
    ) -> Result<(Conversation, bool), Failure> {
        self.write(|connection| {
            let previous = rows(connection, Some(id), true)
                .map_err(storage_error)?
                .pop()
                .ok_or_else(|| Failure::new("invalid_thread", "task is unavailable"))?;
            let Some(row) = change_startup(&previous, change)? else {
                return Ok((previous, false));
            };
            save(connection, &row).map_err(storage_error)?;
            Ok((row, true))
        })
    }
}

impl Database {
    pub(in crate::host_rpc) fn get(
        &self,
        id: &SessionRef,
    ) -> Result<Option<Conversation>, Failure> {
        self.read(|connection| {
            if let Some(row) = rows(connection, Some(id), true)
                .map_err(storage_error)?
                .pop()
            {
                return Ok(Some(row));
            }
            if let Some(owner) = native_owner(connection, id).map_err(storage_error)? {
                return Err(Failure::new(
                    "native_session_bound",
                    format!("Use the public conversation {owner} instead of its provider session."),
                ));
            }
            Ok(None)
        })
    }

    pub(in crate::host_rpc) fn summary(
        &self,
        mut thread: Thread,
    ) -> Result<Option<Thread>, Failure> {
        self.read(|connection| {
            if let Some(id) = &thread.id
                && let Some(owner) = native_owner(connection, id).map_err(storage_error)?
                && let Some(row) = rows(connection, Some(&owner), false)
                    .map_err(storage_error)?
                    .pop()
            {
                if *id != row.active {
                    return Ok(None);
                }
                row.describe(&mut thread);
            }
            if let Some(parent) = &thread.parent_id
                && let Some(owner) = native_owner(connection, parent).map_err(storage_error)?
            {
                thread.parent_id = Some(owner);
            }
            Ok(Some(thread))
        })
    }

    pub(in crate::host_rpc) fn active_bindings(
        &self,
    ) -> Result<Vec<(SessionRef, ProviderKind)>, Failure> {
        self.read(|connection| {
            let mut query = connection.prepare("SELECT thread_id, active_provider_thread_id FROM threads WHERE managed = 1 AND active_provider_thread_id IS NOT NULL").map_err(storage_error)?;
            query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).map_err(storage_error)?
                .map(|row| { let (public, native) = row.map_err(storage_error)?; let native: SessionRef = decode(&native).map_err(storage_error)?; Ok((decode(&public).map_err(storage_error)?, native.provider)) }).collect()
        })
    }

    pub(in crate::host_rpc) fn route(
        &self,
        event: AgentChange,
        supports_fork: bool,
    ) -> Result<Option<AgentChange>, Failure> {
        let native = match &event {
            AgentChange::Session { session, .. }
            | AgentChange::Request { session, .. }
            | AgentChange::Renamed { session, .. } => session,
            _ => return Ok(Some(event)),
        };
        let context = matches!(
            &event,
            AgentChange::Session {
                change: SessionChange::Turn { .. }
                    | SessionChange::Item { .. }
                    | SessionChange::TurnItems { .. },
                ..
            }
        );
        let row = self.read(|connection| -> Result<_, Failure> {
            let Some(owner) = native_owner(connection, native).map_err(storage_error)? else {
                return Ok(None);
            };
            Ok(rows(connection, Some(&owner), context)
                .map_err(storage_error)?
                .pop())
        })?;
        let event = match event {
            AgentChange::Session {
                session,
                mut change,
            } => {
                let Some(row) = row.as_ref() else {
                    return Ok(Some(AgentChange::Session { session, change }));
                };
                // Closed ranges are read from native history. Late callbacks
                // from a released runtime cannot change the active execution.
                if session != row.active {
                    return Ok(None);
                }
                namespace_change(row, &session, &mut change, supports_fork);
                AgentChange::Session {
                    session: row.id.clone(),
                    change,
                }
            }
            AgentChange::Request {
                session,
                origin,
                mut request,
            } => {
                let root = if let Some(row) = row.as_ref() {
                    if session != row.active {
                        return Err(Failure::new(
                            "inactive_provider_session",
                            "The provider session has been released.",
                        ));
                    }
                    namespace_request(row, &session, &mut request);
                    row.id.clone()
                } else {
                    session
                };
                AgentChange::Request {
                    session: root,
                    origin,
                    request,
                }
            }
            AgentChange::Renamed { session, name } => {
                if let Some(row) = row.as_ref() {
                    if row.active != session || row.name.is_some() {
                        return Ok(None);
                    }
                    return Ok(Some(AgentChange::Renamed {
                        session: row.id.clone(),
                        name,
                    }));
                }
                AgentChange::Renamed { session, name }
            }
            other => other,
        };
        Ok(Some(event))
    }
}

fn native_owner(
    connection: &Connection,
    native: &SessionRef,
) -> Result<Option<SessionRef>, String> {
    connection
        .query_row(
            "SELECT thread_id FROM provider_threads WHERE provider_thread_id = ?1",
            [key(native)],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(failure)?
        .map(|value| decode(&value))
        .transpose()
}
