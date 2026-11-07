//! One transactional owner for conversation identity, content and revisions.
mod commands;
mod history;
#[cfg(test)]
pub(super) use history::Window;
mod migrations;
pub(super) mod registry;

use crate::providers::Failure;
use agent_protocol::{
    models::{Thread, ThreadResponse},
    session::SessionRef,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{path::Path, sync::Mutex};

pub(crate) struct Database {
    // An open/migration failure stays a failure, never an empty conversation.
    state: Mutex<Result<Connection, String>>,
}

pub(super) struct StorageError(String);
impl From<StorageError> for String {
    fn from(error: StorageError) -> Self {
        error.0
    }
}
impl From<StorageError> for Failure {
    fn from(error: StorageError) -> Self {
        Self::new("conversation_storage_failed", error.0)
    }
}

pub(super) fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}
pub(super) fn encode(value: impl Serialize) -> Result<String, String> {
    serde_json::to_string(&value).map_err(failure)
}
pub(super) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, String> {
    serde_json::from_str(value).map_err(failure)
}
pub(super) fn key(session: &SessionRef) -> String {
    serde_json::to_string(session).expect("session identity serializes")
}

impl Database {
    pub(crate) fn open(path: &Path) -> Self {
        let state = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(failure)?;
            }
            let mut connection = Connection::open(path).map_err(failure)?;
            let fresh: bool = connection
                .query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table')",
                    [],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            if fresh {
                connection
                    .execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")
                    .map_err(failure)?;
            }
            connection
                .busy_timeout(std::time::Duration::from_secs(10))
                .map_err(failure)?;
            connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").map_err(failure)?;
            migrations::upgrade(&mut connection, path)?;
            Ok(connection)
        })();
        let database = Self {
            state: Mutex::new(state),
        };
        if let Err(error) = database.recover_conversations() {
            *database
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Err(error.to_string());
        }
        database
    }

    pub(super) fn with<T, E: From<StorageError>>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        work(
            state
                .as_mut()
                .map_err(|error| E::from(StorageError(error.clone())))?,
        )
    }

    pub(super) fn write<T, E: From<StorageError>>(
        &self,
        work: impl FnOnce(&Transaction) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with(|connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| E::from(StorageError(failure(error))))?;
            let value = work(&transaction)?;
            transaction
                .commit()
                .map_err(|error| E::from(StorageError(failure(error))))?;
            Ok(value)
        })
    }

    pub(super) fn read<T, E: From<StorageError>>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        self.with(|connection| {
            // Several indexed reads form one snapshot even if another Host
            // connection commits while this window is being assembled.
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Deferred)
                .map_err(|error| E::from(StorageError(failure(error))))?;
            let value = work(&transaction)?;
            transaction
                .commit()
                .map_err(|error| E::from(StorageError(failure(error))))?;
            Ok(value)
        })
    }
}

pub(super) fn ensure_thread(connection: &Connection, session: &SessionRef) -> Result<(), String> {
    let response = ThreadResponse {
        thread: Thread {
            id: Some(session.clone()),
            ..Default::default()
        },
        model: None,
        effort: None,
        service_tier: None,
    };
    connection.execute("INSERT INTO threads (thread_id, metadata_json) VALUES (?1, ?2) ON CONFLICT(thread_id) DO NOTHING", params![key(session), encode(response)?]).map_err(failure)?;
    Ok(())
}

pub(super) fn changed(
    connection: &Connection,
    session: &SessionRef,
    kind: &str,
    id: &str,
) -> Result<(), String> {
    connection.execute("INSERT INTO changes (thread_id, entity_type, entity_id) VALUES (?1, ?2, ?3)
        ON CONFLICT(thread_id, entity_type, entity_id) DO UPDATE SET sequence = excluded.sequence, deleted = 0", params![key(session), kind, id]).map_err(failure)?;
    Ok(())
}

pub(super) fn deleted(
    connection: &Connection,
    session: &SessionRef,
    kind: &str,
    id: &str,
) -> Result<(), String> {
    changed(connection, session, kind, id)?;
    connection.execute("UPDATE changes SET deleted = 1 WHERE thread_id = ?1 AND entity_type = ?2 AND entity_id = ?3", params![key(session), kind, id]).map_err(failure)?;
    Ok(())
}
