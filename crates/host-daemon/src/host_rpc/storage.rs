//! Canonical events, projections and effect receipts share one SQLite writer.
use agent_protocol::{
    conversation::records,
    session::{ProviderKind, SessionRef},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;
mod activity;
pub(in crate::host_rpc) mod checkpoints;
pub(in crate::host_rpc) mod commands;
mod event_store;
#[cfg(any(test, feature = "test-support"))]
pub mod fixture;
mod forks;
mod handoffs;
pub(in crate::host_rpc) use handoffs::NativeAllocation;
mod lifecycle;
mod message_search;
mod native_bindings;
mod native_import;
mod native_threads;
pub(in crate::host_rpc) mod outbox;
mod projections;
mod projects;
mod shell;
mod stream;
#[cfg(test)]
pub(in crate::host_rpc) mod test_support;
#[cfg(test)]
mod tests;
mod timeline;
mod titles;
pub(in crate::host_rpc) mod workspace;

/// Host storage and offline fixture consumers address the same canonical file.
pub const CONVERSATION_DATABASE_FILE: &str = "bex-orchestration-v2.sqlite";

#[derive(Debug)]
pub(crate) struct Storage {
    connection: Mutex<Result<Connection, String>>,
    scope: Result<String, String>,
    changed: tokio::sync::watch::Sender<()>,
}
fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn integer(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| "conversation sequence exhausted".into())
}

impl Storage {
    pub(super) fn request_owner(
        &self,
        id: &agent_protocol::ids::RequestId,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| connection.query_row("SELECT r.thread_id FROM orchestration_v2_projection_runtime_requests r WHERE r.runtime_request_id=?1 AND (r.status='pending' OR (r.status='resolved' AND EXISTS(SELECT 1 FROM orchestration_v2_effect_outbox e WHERE e.thread_id=r.thread_id AND e.effect_type='runtime-request.respond' AND e.status IN ('pending','running') AND json_extract(e.payload_json,'$.requestId')=r.runtime_request_id)))",[id.as_str()],|row|row.get::<_,String>(0)).optional().map_err(failure)?
            .map(|id|SessionRef::new(None,id).map_err(str::to_owned)).transpose())
    }
    pub(super) fn receipts(
        &self,
        id: &SessionRef,
        operations: &[Uuid],
    ) -> Result<records::Receipts, String> {
        if operations.len() > records::CheckCommands::MAX_OPERATIONS {
            return Err("too many command receipts requested".into());
        }
        self.with(|connection| {
            let transaction=connection.transaction().map_err(failure)?;
            let mut receipts=Vec::new();
            for operation in operations {
                let receipt=transaction.query_row("SELECT result_sequence,accepted_at,error FROM orchestration_command_receipts WHERE command_id=?1 AND aggregate_kind='thread' AND aggregate_id=?2",params![operation.to_string(),id.application_id()],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?))).optional().map_err(failure)?;
                if let Some((sequence,accepted_at,error))=receipt {
                    receipts.push((*operation,records::Acknowledged {sequence:sequence.try_into().map_err(failure)?,accepted_at,error}));
                }
            }
            transaction.commit().map_err(failure)?;
            Ok(receipts)
        })
    }

    pub(super) fn check_ready(&self) -> Result<(), String> {
        self.with(|_| Ok(()))
    }
    pub(in crate::host_rpc) fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }
    pub(crate) fn open(path: &Path) -> Self {
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
                6 => {}
                _ => {
                    return Err(format!(
                        "unsupported conversation storage format: {version}"
                    ));
                }
            }
            connection.prepare("SELECT input_intent,created_by FROM orchestration_v2_projection_turn_items LIMIT 0").map_err(failure)?;
            connection
                .prepare(
                    "SELECT sequence,application_event_version FROM orchestration_events LIMIT 0",
                )
                .map_err(failure)?;
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                .map_err(failure)?;
            Ok(connection)
        })();
        // The DB is the authority after initial import. Resolve the created
        // file itself so symlink aliases share pending commands and cursors.
        // A new filename starts a new namespace without reading the old DB.
        let scope = dunce::canonicalize(path).map_err(failure).and_then(|path| {
            let bytes = serde_json::to_vec(&("orchestration-v2", path)).map_err(failure)?;
            let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
            Ok(digest
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect())
        });
        Self {
            connection: Mutex::new(connection),
            scope,
            changed: tokio::sync::watch::channel(()).0,
        }
    }
    pub(in crate::host_rpc) fn scope(&self) -> Result<String, String> {
        self.scope.clone()
    }
    fn initialize(connection: &mut Connection) -> Result<(), String> {
        let populated: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table')",
                [],
                |row| row.get(0),
            )
            .map_err(failure)?;
        if populated {
            return Err("unversioned conversation database is unsupported".into());
        }
        let tx = connection.transaction().map_err(failure)?;
        tx.execute_batch(include_str!("storage/orchestration.sql"))
            .map_err(failure)?;
        message_search::initialize(&tx)?;
        tx.pragma_update(None, "user_version", 6).map_err(failure)?;
        tx.commit().map_err(failure)
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
        if result.is_ok() && connection.total_changes() != before {
            self.changed.send_replace(());
        }
        result
    }
}
