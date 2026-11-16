//! Read-only fixture access to the production native identity decisions.
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
