mod load;
mod writer;

pub use load::*;
pub use writer::*;

use crate::{ShellRow, StoreError};
use agent_domain::{CommandId, Effect, Fact, FactBody, Receipt, Reply, ThreadId, Timestamp};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// Cached projections older than this marker are rebuilt from facts.
pub const SNAPSHOT_FORMAT: &str = "state-json-1";
pub const SNAPSHOT_INTERVAL: u64 = 256;
pub const FACT_PAGE: usize = 500;
const SCHEMA_VERSION: &str = "1";
const IDLE_READERS: usize = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct StoredFact {
    pub global_seq: u64,
    pub thread_seq: u64,
    pub fact: Fact,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadHead {
    pub thread_seq: u64,
    pub input_seq: u64,
    pub global_seq: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredReceipt {
    pub thread: ThreadId,
    pub receipt: Receipt,
    pub thread_seq: u64,
    pub global_seq: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactGap {
    pub facts: u64,
    pub bytes: u64,
    pub contains_created: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub effect: Effect,
    pub thread: ThreadId,
    pub lane: String,
    pub kind: String,
    pub status: String,
    pub attempts: u32,
    pub available_at: i64,
}

/// The SQLite fact log. One writer thread owns the write connection; reads use a pool.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}
struct Inner {
    path: PathBuf,
    writer: std::sync::mpsc::Sender<Job>,
    readers: Mutex<Vec<Connection>>,
    listeners: Arc<RwLock<Vec<Arc<dyn CommitListener>>>>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let mut connection = Connection::open(&path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL; PRAGMA busy_timeout = 5000;",
        )?;
        migrate(&mut connection)?;
        let listeners = Arc::new(RwLock::new(Vec::new()));
        let writer = writer::spawn(connection, listeners.clone())?;
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                writer,
                readers: Mutex::new(Vec::new()),
                listeners,
            }),
        })
    }

    /// Listeners run on the writer thread right after each commit, in global order.
    pub fn add_listener(&self, listener: Arc<dyn CommitListener>) {
        self.inner
            .listeners
            .write()
            .expect("listener list")
            .push(listener);
    }

    /// Runs `read` in one read transaction on a pooled connection.
    pub fn read<T>(
        &self,
        read: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let connection = match self.inner.readers.lock().expect("reader pool").pop() {
            Some(connection) => connection,
            None => {
                let connection = Connection::open_with_flags(
                    &self.inner.path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                connection.busy_timeout(std::time::Duration::from_secs(5))?;
                connection
            }
        };
        let result = (|| {
            let transaction = connection.unchecked_transaction()?;
            let value = read(&transaction)?;
            transaction.finish()?;
            Ok(value)
        })();
        let mut idle = self.inner.readers.lock().expect("reader pool");
        if idle.len() < IDLE_READERS {
            idle.push(connection);
        }
        result
    }

    /// Runs blocking store work off the async executor.
    pub async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(|error| StoreError::Corrupt(format!("store task failed: {error}")))?
    }

    pub fn receipt(&self, command: &CommandId) -> Result<Option<StoredReceipt>, StoreError> {
        self.read(|c| {
            c.query_row(
                "SELECT thread_id, fingerprint, reply, thread_seq, global_seq
                 FROM receipts WHERE command_id = ?1",
                [command.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()?
            .map(|(thread, fingerprint, reply, thread_seq, global_seq)| {
                Ok(StoredReceipt {
                    thread: thread_id(thread)?,
                    receipt: Receipt {
                        command: command.clone(),
                        fingerprint,
                        reply: serde_json::from_str::<Reply>(&reply)?,
                    },
                    thread_seq: thread_seq as u64,
                    global_seq: global_seq as u64,
                })
            })
            .transpose()
        })
    }

    pub fn thread_head(&self, thread: &ThreadId) -> Result<ThreadHead, StoreError> {
        self.read(|c| head(c, thread))
    }

    /// Facts in global order, optionally for one thread.
    pub fn facts_page(
        &self,
        thread: Option<&ThreadId>,
        after_global_seq: u64,
        limit: usize,
    ) -> Result<Vec<StoredFact>, StoreError> {
        self.read(|c| facts_page(c, thread, after_global_seq, limit))
    }

    pub fn facts_after(
        &self,
        thread: Option<&ThreadId>,
        after_global_seq: u64,
    ) -> Result<Vec<StoredFact>, StoreError> {
        self.read(|c| {
            let mut facts = Vec::new();
            let mut after = after_global_seq;
            loop {
                let page = facts_page(c, thread, after, FACT_PAGE)?;
                let full = page.len() == FACT_PAGE;
                if let Some(last) = page.last() {
                    after = last.global_seq;
                }
                facts.extend(page);
                if !full {
                    return Ok(facts);
                }
            }
        })
    }

    pub fn fact_gap(
        &self,
        thread: &ThreadId,
        after_global_seq: u64,
    ) -> Result<FactGap, StoreError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0),
                        COALESCE(MAX(kind = 'ThreadCreated'), 0)
                 FROM facts WHERE thread_id = ?1 AND global_seq > ?2",
                params![thread.as_str(), after_global_seq as i64],
                |row| {
                    Ok(FactGap {
                        facts: row.get::<_, i64>(0)? as u64,
                        bytes: row.get::<_, i64>(1)? as u64,
                        contains_created: row.get::<_, i64>(2)? != 0,
                    })
                },
            )?)
        })
    }

    pub fn latest_global_seq(&self) -> Result<u64, StoreError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(global_seq), 0) FROM facts",
                [],
                |row| row.get::<_, i64>(0),
            )? as u64)
        })
    }

    /// Pending or running outbox rows keep their thread resident.
    pub fn has_open_effects(&self, thread: &ThreadId) -> Result<bool, StoreError> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM outbox
                 WHERE thread_id = ?1 AND status IN ('pending', 'running'))",
                [thread.as_str()],
                |row| row.get::<_, bool>(0),
            )?)
        })
    }

    pub fn outbox(&self, thread: &ThreadId) -> Result<Vec<OutboxRow>, StoreError> {
        self.read(|c| {
            let mut statement = c.prepare_cached(
                "SELECT payload, lane, kind, status, attempts, available_at
                 FROM outbox WHERE thread_id = ?1 ORDER BY rowid",
            )?;
            let rows = statement.query_map([thread.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            rows.map(|row| {
                let (payload, lane, kind, status, attempts, available_at) = row?;
                Ok(OutboxRow {
                    effect: serde_json::from_str(&payload)?,
                    thread: thread.clone(),
                    lane,
                    kind,
                    status,
                    attempts: attempts as u32,
                    available_at,
                })
            })
            .collect()
        })
    }

    pub fn threads_needing_recovery(&self) -> Result<Vec<ThreadId>, StoreError> {
        self.read(|c| {
            let mut statement = c.prepare_cached(
                "SELECT thread_id FROM threads WHERE needs_recovery = 1 ORDER BY thread_id",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| thread_id(row?)).collect()
        })
    }

    pub fn shell(&self, thread: &ThreadId) -> Result<Option<(u64, ShellRow)>, StoreError> {
        self.read(|c| {
            c.query_row(
                "SELECT global_seq, project, archived, deleted, needs_recovery, payload
                 FROM thread_shells WHERE thread_id = ?1",
                [thread.as_str()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(seq, project, archived, deleted, needs_recovery, payload)| {
                    Ok((
                        seq as u64,
                        ShellRow {
                            project,
                            archived,
                            deleted,
                            needs_recovery,
                            payload: serde_json::from_str(&payload)?,
                        },
                    ))
                },
            )
            .transpose()
        })
    }
}

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'runtime_meta')",
        [],
        |row| row.get(0),
    )?;
    if exists {
        let version: Option<String> = transaction
            .query_row(
                "SELECT value FROM runtime_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if version.as_deref() != Some(SCHEMA_VERSION) {
            return Err(StoreError::Schema(version.unwrap_or_default()));
        }
    } else {
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.execute(
            "INSERT INTO runtime_meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

fn thread_id(value: String) -> Result<ThreadId, StoreError> {
    ThreadId::new(value).map_err(|error| StoreError::Corrupt(error.to_string()))
}

pub(crate) fn head(c: &Connection, thread: &ThreadId) -> Result<ThreadHead, StoreError> {
    Ok(c.query_row(
        "SELECT thread_seq, input_seq, last_global_seq FROM threads WHERE thread_id = ?1",
        [thread.as_str()],
        |row| {
            Ok(ThreadHead {
                thread_seq: row.get::<_, i64>(0)? as u64,
                input_seq: row.get::<_, i64>(1)? as u64,
                global_seq: row.get::<_, i64>(2)? as u64,
            })
        },
    )
    .optional()?
    .unwrap_or_default())
}

pub(crate) fn decode_fact(at: &str, payload: &str) -> Result<Fact, StoreError> {
    Ok(Fact {
        at: Timestamp::parse(at).map_err(|error| StoreError::Corrupt(error.to_string()))?,
        body: serde_json::from_str::<FactBody>(payload)?,
    })
}

fn facts_page(
    c: &Connection,
    thread: Option<&ThreadId>,
    after: u64,
    limit: usize,
) -> Result<Vec<StoredFact>, StoreError> {
    let mut statement = c.prepare_cached(match thread {
        Some(_) => {
            "SELECT global_seq, thread_seq, at, payload FROM facts
             WHERE thread_id = ?1 AND global_seq > ?2 ORDER BY global_seq LIMIT ?3"
        }
        None => {
            "SELECT global_seq, thread_seq, at, payload FROM facts
             WHERE ?1 IS NULL AND global_seq > ?2 ORDER BY global_seq LIMIT ?3"
        }
    })?;
    let rows = statement.query_map(
        params![thread.map(ThreadId::as_str), after as i64, limit as i64],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (global_seq, thread_seq, at, payload) = row?;
        Ok(StoredFact {
            global_seq: global_seq as u64,
            thread_seq: thread_seq as u64,
            fact: decode_fact(&at, &payload)?,
        })
    })
    .collect()
}

#[cfg(test)]
pub(crate) mod tests;
