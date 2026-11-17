//! Fixture access to the production native identity decisions and observations.
use super::*;

fn read<T>(
    database: &Path,
    load: impl FnOnce(&Connection) -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    if !database.exists() {
        return Ok(None);
    }
    let connection =
        Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(failure)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(10))
        .map_err(failure)?;
    load(&connection)
}

pub fn native_owner(database: &Path, native: &SessionRef) -> Result<Option<SessionRef>, String> {
    read(database, |connection| {
        native_bindings::owner(connection, native)
    })
}

pub fn active_native_source(
    database: &Path,
    thread: &SessionRef,
) -> Result<Option<SessionRef>, String> {
    read(database, |connection| {
        native_bindings::active_source(connection, thread)
    })
}

impl Storage {
    /// Publish fixture metadata and observed archive records atomically.
    pub(in crate::host_rpc) fn seed_native_records(
        &self,
        thread: &agent_protocol::orchestration::Thread,
        native: &agent_protocol::orchestration::ProviderThread,
        records: Vec<agent_protocol::orchestration::Change>,
    ) -> Result<(), String> {
        self.with(|db| {
            let tx = db.transaction().map_err(failure)?;
            let source = native_bindings::header_source(thread, native)?;
            if let Some(owner) = native_bindings::owner(&tx, &source)? {
                if owner.application_id() != thread.id {
                    return Err("native source is already owned".into());
                }
                if !records.is_empty() {
                    return Ok(());
                }
            }
            native_threads::write_header(&tx, thread, native, &source)?;
            if !records.is_empty() {
                let state = projections::load(&tx, &thread.id, &[])?;
                commands::append_decision(
                    &tx,
                    &state,
                    agent_protocol::orchestration::Decision {
                        changes: records,
                        effects: vec![],
                    },
                    None,
                    &thread.updated_at,
                )?;
            }
            tx.commit().map_err(failure)
        })
    }
}
