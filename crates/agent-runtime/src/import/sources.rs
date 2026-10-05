//! Per-transcript import records and the native sessions threads already own.
use super::ImportSource;
use super::paths::comparison_key;
use crate::{Store, StoreError};
use agent_domain::{ThreadId, Timestamp};
use rusqlite::{OptionalExtension, params};
use std::path::Path;

/// `runtime_meta` key set once the first-run import has finished.
pub const FIRST_RUN_IMPORT_KEY: &str = "import:first-run";

/// Transcripts already imported into threads of the project at `root`.
pub(crate) fn completed_sources(
    store: &Store,
    root: &Path,
) -> Result<Vec<ImportSource>, StoreError> {
    let root = comparison_key(root);
    store.read(|c| {
        let mut statement = c.prepare_cached(
            "SELECT source FROM imported_sources WHERE project_root = ?1 ORDER BY rowid",
        )?;
        let rows = statement.query_map([root], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    })
}

pub(crate) async fn record_source(
    store: &Store,
    thread: &ThreadId,
    root: &Path,
    source: &ImportSource,
) -> Result<(), StoreError> {
    let (thread, root, source) = (thread.to_string(), comparison_key(root), source.clone());
    store
        .write(move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO imported_sources
                     (instance, path, fingerprint, project_root, native_session, thread_id, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    source.instance,
                    source.path.to_string_lossy(),
                    source.fingerprint(),
                    root,
                    source.session,
                    thread,
                    serde_json::to_string(&source)?,
                ],
            )?;
            Ok(())
        })
        .await
}

/// Matches the `facts_native_session` index expression.
pub(crate) const NATIVE_OWNER_QUERY: &str = "SELECT thread_id FROM facts
     WHERE kind IN ('SessionBound', 'NativeSessionBound', 'NativeChildBound')
       AND COALESCE(
               json_extract(payload, '$.SessionBound.native_thread'),
               json_extract(payload, '$.NativeSessionBound.native_thread'),
               json_extract(payload, '$.NativeChildBound.native_thread')) = ?1
       AND thread_id <> ?2
     LIMIT 1";

/// Another thread bound to this native session: one the Host ran, forked or adopted as a native child.
pub(crate) fn native_session_owner(
    store: &Store,
    native: &str,
    except: &ThreadId,
) -> Result<Option<String>, StoreError> {
    store.read(|c| {
        Ok(c.query_row(
            NATIVE_OWNER_QUERY,
            params![native, except.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
    })
}

pub(crate) fn first_run_done(store: &Store) -> Result<bool, StoreError> {
    store.read(|c| {
        Ok(c.query_row(
            "SELECT EXISTS (SELECT 1 FROM runtime_meta WHERE key = ?1)",
            [FIRST_RUN_IMPORT_KEY],
            |row| row.get::<_, bool>(0),
        )?)
    })
}

pub(crate) async fn mark_first_run(store: &Store, at: Timestamp) -> Result<(), StoreError> {
    store
        .write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO runtime_meta (key, value) VALUES (?1, ?2)",
                params![FIRST_RUN_IMPORT_KEY, at.as_str()],
            )?;
            Ok(())
        })
        .await
}
