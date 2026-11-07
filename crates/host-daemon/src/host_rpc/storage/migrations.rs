use super::*;
use agent_protocol::{models::Turn, protocol};
use rusqlite_migration::{HookError, M, Migrations};
use std::path::PathBuf;

pub(super) const VERSION: usize = 2;

// Version 1 is the already-used transcript layout. Conversion is confined to
// this migration; normal reads and writes only use the normalized schema.
pub(super) fn definitions(path: Option<PathBuf>) -> Migrations<'static> {
    Migrations::new(vec![
        M::up(include_str!("001_transcripts.sql")),
        M::up_with_hook(
            include_str!("schema.sql"),
            move |transaction: &Transaction| {
                convert(transaction, path.as_deref()).map_err(HookError::Hook)
            },
        ),
    ])
}

pub(super) fn upgrade(connection: &mut Connection, path: &Path) -> Result<(), String> {
    let migrations = definitions(Some(path.to_owned()));
    let pending = migrations.pending_migrations(connection).map_err(failure)?;
    if pending > 0 {
        let existing: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table')",
                [],
                |row| row.get(0),
            )
            .map_err(failure)?;
        if existing {
            let backup = path.with_extension(format!("before-v{VERSION}.sqlite"));
            // VACUUM INTO takes a consistent SQLite snapshot, including WAL;
            // copying the main file alone could lose committed conversations.
            if !backup.exists() {
                connection
                    .execute("VACUUM INTO ?1", [backup.to_string_lossy().as_ref()])
                    .map_err(failure)?;
            }
            let saved =
                Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(failure)?;
            let check: String = saved
                .query_row("PRAGMA quick_check", [], |row| row.get(0))
                .map_err(failure)?;
            if check != "ok" {
                return Err("The database recovery copy failed its integrity check".into());
            }
        }
    }
    // All pending migrations share one transaction. On error the original
    // schema/data/version remain in place; never restore over another writer.
    migrations.to_latest(connection).map_err(failure)
}

fn convert(connection: &Connection, path: Option<&Path>) -> Result<(), String> {
    let mut query = connection
        .prepare("SELECT provider, id, metadata FROM conversations")
        .map_err(failure)?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
            ))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for (provider, id, bytes) in rows {
        let session = SessionRef::new(decode(&encode(provider)?)?, id).map_err(failure)?;
        ensure_thread(connection, &session)?;
        if let Some(bytes) = bytes {
            let mut response: ThreadResponse = protocol::decode(&bytes).map_err(failure)?;
            response.thread = crate::projects::titles::summary(response.thread);
            response.thread.id = Some(session.clone());
            response.thread.status = agent_protocol::models::SessionStatus::Unknown;
            response.thread.list_stale = None;
            connection
                .execute(
                    "UPDATE threads SET metadata_json = ?2, imported = 1 WHERE thread_id = ?1",
                    params![key(&session), encode(response)?],
                )
                .map_err(failure)?;
        }
    }
    let mut query = connection
        .prepare("SELECT provider, id, version FROM versions ORDER BY version")
        .map_err(failure)?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for (provider, id, sequence) in rows {
        let session = SessionRef::new(decode(&encode(provider)?)?, id).map_err(failure)?;
        ensure_thread(connection, &session)?;
        connection.execute("INSERT INTO changes (sequence, thread_id, entity_type, entity_id) VALUES (?1, ?2, 'thread', ?2)", params![sequence, key(&session)]).map_err(failure)?;
    }
    let mut query = connection
        .prepare("SELECT provider, id, position, body FROM turns ORDER BY provider, id, position")
        .map_err(failure)?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, usize>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    for (provider, id, position, body) in rows {
        let session = SessionRef::new(decode(&encode(provider)?)?, id).map_err(failure)?;
        let turn: Turn = protocol::decode(&body).map_err(failure)?;
        super::history::write_turn(connection, &session, position, &turn, None)?;
    }
    if let Some(path) = path {
        let registry = path.with_file_name("bex-conversations.json");
        match std::fs::read(registry) {
            Ok(bytes) => {
                let rows: Vec<crate::host_rpc::conversations::Conversation> =
                    serde_json::from_slice(&bytes).map_err(failure)?;
                crate::host_rpc::conversations::validate(&rows)?;
                for row in rows {
                    super::registry::save(connection, &row)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failure(error)),
        }
        match std::fs::read(path.with_file_name("bex-model-selections.json")) {
            Ok(bytes) => {
                let selections: Vec<crate::host_rpc::service::model_selections::Selection> =
                    serde_json::from_slice(&bytes).map_err(failure)?;
                for selection in selections {
                    connection.execute("INSERT INTO model_selections (native_session_id, options_json) VALUES (?1, ?2)", params![key(&selection.session), encode(selection.options)?]).map_err(failure)?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failure(error)),
        }
    }
    connection
        .execute_batch("DROP TABLE turns; DROP TABLE conversations; DROP TABLE versions;")
        .map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        models::{Thread, Turn},
        session::ProviderKind,
    };

    fn session() -> SessionRef {
        SessionRef::new(ProviderKind::Codex, "native".into()).unwrap()
    }

    fn legacy(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(include_str!("001_transcripts.sql"))
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(session()),
                cwd: Some("/workspace".into()),
                name: Some("original title".into()),
                ..Default::default()
            },
            model: None,
            effort: None,
            service_tier: None,
        };
        connection.execute("INSERT INTO conversations (provider, id, cwd, metadata) VALUES ('codex', 'native', '/workspace', ?1)", [protocol::encode(response).unwrap()]).unwrap();
        connection.execute("INSERT INTO turns (provider, id, position, turn_id, running, body) VALUES ('codex', 'native', 0, 'turn', 0, ?1)", [protocol::encode(Turn { id: "turn".into(), ..Default::default() }).unwrap()]).unwrap();
        connection
            .execute("INSERT INTO versions VALUES ('codex', 'native', 120)", [])
            .unwrap();
        connection
    }

    #[test]
    fn migrations_are_valid() {
        definitions(None).validate().unwrap();
    }

    #[test]
    fn upgrade_preserves_transcripts_bindings_and_an_untouched_recovery_copy() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bex-transcripts.sqlite");
        drop(legacy(&path));
        let mut row = crate::host_rpc::conversations::Conversation::new(&Thread {
            id: Some(session()),
            cwd: Some("/workspace".into()),
            ..Default::default()
        })
        .unwrap();
        row.name = Some("user title".into());
        row.cover(1);
        let registry = serde_json::to_vec(&[row]).unwrap();
        std::fs::write(root.path().join("bex-conversations.json"), &registry).unwrap();
        let database = Database::open(&path);
        let history = database.history(&session()).unwrap().unwrap();
        assert_eq!(history.thread.name.as_deref(), Some("user title"));
        assert_eq!(history.thread.turns.unwrap()[0].id.as_str(), "turn");
        assert_eq!(
            database.get(&session()).unwrap().unwrap().bindings[0].covered,
            Some(1)
        );
        let snapshot = database.window(&session(), None, 10).unwrap().unwrap();
        assert!(snapshot.response.thread.transcript_version.unwrap() >= 120);
        assert_eq!(
            std::fs::read(root.path().join("bex-conversations.json")).unwrap(),
            registry
        );
        let backup = Connection::open(path.with_extension("before-v2.sqlite")).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, usize>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            backup
                .query_row("SELECT version FROM versions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            120
        );
        database
            .with(|connection| {
                assert!(connection.prepare("SELECT * FROM conversations").is_err());
                assert_eq!(
                    connection
                        .pragma_query_value(None, "user_version", |row| row.get::<_, usize>(0))
                        .unwrap(),
                    VERSION
                );
                Ok::<(), String>(())
            })
            .unwrap();
    }

    #[test]
    fn failed_upgrade_restores_schema_data_and_version_and_retries_after_repair() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bex-transcripts.sqlite");
        drop(legacy(&path));
        let registry = root.path().join("bex-conversations.json");
        std::fs::write(&registry, "broken").unwrap();
        let database = Database::open(&path);
        assert!(database.history(&session()).is_err());
        drop(database);
        let unchanged = Connection::open(&path).unwrap();
        assert_eq!(
            unchanged
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            unchanged
                .query_row("SELECT version FROM versions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            120
        );
        assert!(unchanged.prepare("SELECT * FROM threads").is_err());
        drop(unchanged);
        std::fs::write(&registry, "[]").unwrap();
        let recovered = Database::open(&path);
        assert_eq!(
            recovered
                .history(&session())
                .unwrap()
                .unwrap()
                .thread
                .turns
                .unwrap()[0]
                .id
                .as_str(),
            "turn"
        );
        let backup = Connection::open(path.with_extension("before-v2.sqlite")).unwrap();
        assert_eq!(
            backup
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn newer_database_is_rejected_without_rewriting_it() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.sqlite");
        let connection = legacy(&path);
        connection
            .pragma_update(None, "user_version", VERSION + 1)
            .unwrap();
        drop(connection);
        let database = Database::open(&path);
        assert!(database.history(&session()).is_err());
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, usize>(0))
                .unwrap(),
            VERSION + 1
        );
        assert_eq!(
            connection
                .query_row("SELECT version FROM versions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            120
        );
        assert!(!path.with_extension("before-v2.sqlite").exists());
    }
}
