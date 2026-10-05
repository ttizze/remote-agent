use super::{FACT_PAGE, SNAPSHOT_FORMAT, SNAPSHOT_INTERVAL, Store, ThreadHead, decode_fact, head};
use crate::StoreError;
use agent_domain::{Fact, State, ThreadId, Timestamp, apply};
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone)]
pub struct LoadedThread {
    pub state: State,
    pub head: ThreadHead,
    pub last_at: Option<Timestamp>,
    /// The cached projection was missing, outdated or unreadable and should be rewritten.
    pub snapshot_stale: bool,
}

impl Store {
    /// The snapshot plus the facts after it, folded. A format mismatch folds every fact.
    pub fn load_thread(&self, thread: &ThreadId) -> Result<LoadedThread, StoreError> {
        self.read(|c| load(c, thread))
    }
}

fn load(c: &Connection, thread: &ThreadId) -> Result<LoadedThread, StoreError> {
    let head = head(c, thread)?;
    let snapshot: Option<(i64, String, Vec<u8>)> = c
        .query_row(
            "SELECT thread_seq, format, blob FROM thread_snapshots WHERE thread_id = ?1",
            [thread.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (base, base_seq, stale) = match snapshot {
        Some((seq, format, blob)) if format == SNAPSHOT_FORMAT => {
            match serde_json::from_slice::<State>(&blob) {
                Ok(state) => (state, seq as u64, false),
                Err(error) => {
                    tracing::warn!(%thread, %error, "rebuilding an unreadable snapshot");
                    (State::default(), 0, true)
                }
            }
        }
        Some(_) => (State::default(), 0, true),
        None => (State::default(), 0, head.thread_seq >= SNAPSHOT_INTERVAL),
    };
    let (state, folded) = fold_pages(base, base_seq, FACT_PAGE, |after, limit| {
        thread_facts(c, thread, after, limit)
    })?;
    if folded != head.thread_seq {
        return Err(StoreError::Corrupt(format!(
            "thread {thread} folded through {folded} but its head is {}",
            head.thread_seq
        )));
    }
    let last_at = c
        .query_row(
            "SELECT at FROM facts WHERE thread_id = ?1 ORDER BY thread_seq DESC LIMIT 1",
            [thread.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|at| Timestamp::parse(&at).map_err(|error| StoreError::Corrupt(error.to_string())))
        .transpose()?;
    Ok(LoadedThread {
        state,
        head,
        last_at,
        snapshot_stale: stale,
    })
}

/// Folds facts after `after` (a thread sequence) one bounded page at a time.
pub(crate) fn fold_pages(
    mut state: State,
    mut after: u64,
    page: usize,
    mut read: impl FnMut(u64, usize) -> Result<Vec<(u64, Fact)>, StoreError>,
) -> Result<(State, u64), StoreError> {
    loop {
        let rows = read(after, page)?;
        for (seq, fact) in &rows {
            apply(&mut state, fact).map_err(|error| {
                StoreError::Corrupt(format!("fact {seq} does not fold: {error}"))
            })?;
            after = *seq;
        }
        if rows.len() < page {
            return Ok((state, after));
        }
    }
}

fn thread_facts(
    c: &Connection,
    thread: &ThreadId,
    after: u64,
    limit: usize,
) -> Result<Vec<(u64, Fact)>, StoreError> {
    let mut statement = c.prepare_cached(
        "SELECT thread_seq, at, payload FROM facts
         WHERE thread_id = ?1 AND thread_seq > ?2 ORDER BY thread_seq LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![thread.as_str(), after as i64, limit as i64],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (seq, at, payload) = row?;
        Ok((seq as u64, decode_fact(&at, &payload)?))
    })
    .collect()
}

#[cfg(test)]
pub(crate) fn thread_facts_for_test(
    store: &Store,
    thread: &ThreadId,
    after: u64,
    limit: usize,
) -> Result<Vec<(u64, Fact)>, StoreError> {
    store.read(|c| thread_facts(c, thread, after, limit))
}
