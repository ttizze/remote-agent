//! First startup after storage unification. The process owns the Host lease
//! before opening this store; old files remain untouched for rollback.
use super::*;
use crate::host_rpc::{
    conversations::{Conversation, validate},
    routing::read_state::Completion,
    service::model_selections::Selection,
};
use serde::Deserialize;
use std::{fs, io::ErrorKind};

const FILES: [&str; 4] = [
    "bex-conversations.json",
    "bex-read-state.json",
    "bex-model-selections.json",
    "bex-conversations.rewind",
];

#[derive(Deserialize)]
struct Rewind {
    previous: Option<Conversation>,
    history: ThreadResponse,
}

pub(super) struct Previous {
    bindings: Vec<Conversation>,
    reads: Vec<Completion>,
    models: Vec<Selection>,
    history: Option<ThreadResponse>,
    backup: tempfile::TempDir,
}

fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failure(error)),
    }
}

pub(super) fn prepare(connection: &Connection, path: &Path) -> Result<Option<Previous>, String> {
    let populated: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table')",
            [],
            |row| row.get(0),
        )
        .map_err(failure)?;
    if !populated && !FILES.iter().any(|name| path.with_file_name(name).exists()) {
        return Ok(None);
    }
    let mut bindings: Vec<Conversation> = read(&path.with_file_name(FILES[0]))?.unwrap_or_default();
    let reads = read(&path.with_file_name(FILES[1]))?.unwrap_or_default();
    let models = read(&path.with_file_name(FILES[2]))?.unwrap_or_default();
    let rewind: Option<Rewind> = read(&path.with_file_name(FILES[3]))?;
    let history = match rewind {
        Some(rewind) => {
            let id = rewind
                .history
                .thread
                .id
                .as_ref()
                .ok_or("missing rewind identity")?;
            if rewind.previous.as_ref().is_some_and(|row| &row.id != id) {
                return Err("inconsistent rewind identity".into());
            }
            bindings.retain(|row| &row.id != id);
            if let Some(previous) = rewind.previous {
                bindings.push(previous);
            }
            Some(rewind.history)
        }
        None => None,
    };
    validate(&bindings)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(failure)?;
    if integrity != "ok" {
        return Err("conversation database integrity check failed".into());
    }
    let backup = backup(connection, path)?;
    Ok(Some(Previous {
        bindings,
        reads,
        models,
        history,
        backup,
    }))
}

fn backup(connection: &Connection, path: &Path) -> Result<tempfile::TempDir, String> {
    let parent = path
        .parent()
        .ok_or("missing conversation storage directory")?;
    let backup = tempfile::Builder::new()
        .prefix("before-storage-unification-")
        .tempdir_in(parent)
        .map_err(failure)?;
    for name in FILES {
        let source = path.with_file_name(name);
        match fs::copy(&source, backup.path().join(name)) {
            Ok(_) => fs::OpenOptions::new()
                .write(true)
                .open(backup.path().join(name))
                .and_then(|file| file.sync_all())
                .map_err(failure)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(failure(error)),
        }
    }
    let database = backup
        .path()
        .join(path.file_name().ok_or("missing database name")?);
    // SQLite copies the committed snapshot, including frames still in the WAL.
    connection
        .execute(
            "VACUUM INTO ?1",
            [database.to_str().ok_or("invalid database path")?],
        )
        .map_err(failure)?;
    fs::OpenOptions::new()
        .write(true)
        .open(&database)
        .and_then(|file| file.sync_all())
        .map_err(failure)?;
    #[cfg(unix)]
    for directory in [backup.path(), parent] {
        fs::File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(failure)?;
    }
    Ok(backup)
}

impl Previous {
    pub(super) fn write(self, transaction: &Transaction) -> Result<tempfile::TempDir, String> {
        if let Some(history) = self.history {
            let id = history
                .thread
                .id
                .as_ref()
                .ok_or("missing rewind identity")?
                .clone();
            store(transaction, &id, history, false)?;
        }
        for (name, bytes) in [
            ("bindings", serde_json::to_vec(&self.bindings)),
            ("reads", serde_json::to_vec(&self.reads)),
            ("models", serde_json::to_vec(&self.models)),
        ] {
            // An unversioned host_state is not a known source format. Never
            // replace an existing destination silently.
            transaction
                .execute(
                    "INSERT INTO host_state(name, body) VALUES (?1, ?2)",
                    params![name, bytes.map_err(failure)?],
                )
                .map_err(failure)?;
        }
        Ok(self.backup)
    }
}
