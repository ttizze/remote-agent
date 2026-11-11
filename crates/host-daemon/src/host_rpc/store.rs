//! Host-owned conversation storage: transcripts, bindings, receipts and options.
//! Native history is imported once; afterwards the Host records completed turns.
//! Stored items keep their bodies inline; readers summarize or defer them for transfer.
use agent_protocol::{
    execution::TurnStatus,
    ids::{ItemId, TurnId},
    models::{Item, SessionStatus, ThreadResponse, Turn},
    operations::InputQueue,
    protocol,
    session::SessionRef,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::HashMap, path::Path, sync::Mutex};

/// A window stops growing past this many encoded bytes, keeping at least one turn.
const WINDOW_BYTES: usize = 1 << 20;

pub(crate) struct ConversationStore {
    // A store that failed to open stays failed; reads must not fall back to
    // an empty transcript that would look like a conversation without turns.
    state: Mutex<Result<Connection, String>>,
}

pub(crate) struct Window {
    pub response: ThreadResponse,
    /// Position of the first returned turn. Earlier turns precede it.
    pub start: usize,
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn turns_by_id<'a>(turns: impl Iterator<Item = &'a Turn>) -> HashMap<&'a TurnId, Vec<&'a Turn>> {
    let mut by_id: HashMap<_, Vec<_>> = HashMap::new();
    for turn in turns {
        by_id.entry(&turn.id).or_default().push(turn);
    }
    by_id
}

fn decode_turn(body: &[u8]) -> Result<Turn, String> {
    protocol::decode(body).map_err(failure)
}

impl ConversationStore {
    pub(crate) fn open(path: &Path) -> Self {
        let state = (|| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(failure)?;
            }
            let connection = Connection::open(path).map_err(failure)?;
            // A restarting Host can briefly overlap its predecessor.
            connection
                .busy_timeout(std::time::Duration::from_secs(10))
                .map_err(failure)?;
            let version: u32 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .map_err(failure)?;
            if version > 1 {
                return Err(format!(
                    "unsupported conversation database version: {version}"
                ));
            }
            if version == 0 {
                let populated: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table')",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(failure)?;
                // A pre-unification Host may have metadata without a transcript
                // database yet. Detect it, but never read or convert it here.
                let metadata = [
                    "bex-conversations.json",
                    "bex-read-state.json",
                    "bex-model-selections.json",
                    "bex-conversations.rewind",
                ]
                .iter()
                .any(|name| path.with_file_name(name).exists());
                if populated || metadata {
                    return Err("conversation storage requires the one-time offline conversion; stop the old Host, back up its state directory, and run the conversion tool before starting this version".into());
                }
            }
            // A successful receipt or rewind must survive a Host restart.
            connection
                .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
                .map_err(failure)?;
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE IF NOT EXISTS host_state (
                         name TEXT PRIMARY KEY,
                         body BLOB NOT NULL
                     ) WITHOUT ROWID;
                     CREATE TABLE IF NOT EXISTS conversations (
                         provider TEXT NOT NULL,
                         id TEXT NOT NULL,
                         cwd TEXT,
                         metadata BLOB,
                         PRIMARY KEY (provider, id)
                     ) WITHOUT ROWID;
                     CREATE TABLE IF NOT EXISTS turns (
                         provider TEXT NOT NULL,
                         id TEXT NOT NULL,
                         position INTEGER NOT NULL,
                         turn_id TEXT NOT NULL,
                         running INTEGER NOT NULL,
                         body BLOB NOT NULL,
                         PRIMARY KEY (provider, id, position)
                     ) WITHOUT ROWID;
                     CREATE INDEX IF NOT EXISTS turn_ids ON turns (provider, id, turn_id, position);
                     -- Host-wide order of each conversation's latest change.
                     CREATE TABLE IF NOT EXISTS versions (
                         provider TEXT NOT NULL,
                         id TEXT NOT NULL,
                         version INTEGER NOT NULL,
                         PRIMARY KEY (provider, id)
                     ) WITHOUT ROWID;
                     CREATE INDEX IF NOT EXISTS version_order ON versions (version);
                     -- Input waiting for each conversation's running turn.
                     CREATE TABLE IF NOT EXISTS queues (
                         provider TEXT NOT NULL,
                         id TEXT NOT NULL,
                         body BLOB NOT NULL,
                         PRIMARY KEY (provider, id)
                     ) WITHOUT ROWID;
                     PRAGMA user_version = 1;
                     COMMIT;",
                )
                .map_err(failure)?;
            Ok(connection)
        })();
        Self {
            state: Mutex::new(state),
        }
    }

    pub(crate) fn check_ready(&self) -> Result<(), String> {
        self.with(|_| Ok(()))
    }

    fn with<T>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        work(state.as_mut().map_err(|error| error.clone())?)
    }

    fn write<T>(&self, work: impl FnOnce(&Transaction) -> Result<T, String>) -> Result<T, String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let value = work(&transaction)?;
            transaction.commit().map_err(failure)?;
            Ok(value)
        })
    }

    pub(super) fn load_state<T: DeserializeOwned + Default>(
        &self,
        name: &str,
    ) -> Result<T, String> {
        self.with(|connection| {
            let bytes: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT body FROM host_state WHERE name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .optional()
                .map_err(failure)?;
            bytes
                .map(|bytes| serde_json::from_slice(&bytes).map_err(failure))
                .unwrap_or_else(|| Ok(T::default()))
        })
    }

    pub(super) fn save_state(&self, name: &str, value: &impl Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(failure)?;
        self.write(|transaction| write_state(transaction, name, &bytes))
    }

    pub(super) fn replace_with_bindings(
        &self,
        conversation: &SessionRef,
        response: ThreadResponse,
        bindings: &[super::conversations::Conversation],
    ) -> Result<u64, String> {
        let bytes = serde_json::to_vec(bindings).map_err(failure)?;
        self.write(|transaction| {
            let version = store(transaction, conversation, response, false)?;
            write_state(transaction, "bindings", &bytes)?;
            Ok(version)
        })
    }

    #[cfg(test)]
    pub(super) fn reject_state_writes(&self, reject: bool) {
        self.with(|connection| {
            connection
                .execute_batch(if reject {
                    "CREATE TEMP TRIGGER reject_state BEFORE INSERT ON host_state
             BEGIN SELECT RAISE(ABORT, 'injected storage failure'); END;"
                } else {
                    "DROP TRIGGER reject_state;"
                })
                .map_err(failure)
        })
        .unwrap();
    }

    /// Native history fills in what the Host did not run. A turn the Host
    /// recorded keeps its recorded items, which an import does not copy in
    /// full; recorded turns missing from native history follow it in order.
    pub(crate) fn import(
        &self,
        conversation: &SessionRef,
        expected_version: Option<u64>,
        response: ThreadResponse,
    ) -> Result<(), String> {
        self.write(|transaction| {
            if transcript_version(transaction, conversation)? != expected_version {
                return Ok(());
            }
            store(transaction, conversation, response, true).map(|_| ())
        })
    }

    /// Capture before reading native history; import checks it in its transaction.
    pub(crate) fn version(&self, conversation: &SessionRef) -> Result<Option<u64>, String> {
        self.with(|connection| transcript_version(connection, conversation))
    }

    /// Replace the stored occurrence of a live turn, or append it. Conversations
    /// that are not imported yet keep these turns for their import.
    /// A turn observed without items keeps the items already stored for it.
    /// The removed item identifies the occurrence even after its last item is gone.
    pub(crate) fn record(
        &self,
        conversation: &SessionRef,
        turn: &Turn,
        removed_item: Option<&ItemId>,
    ) -> Result<(), String> {
        let key = params![conversation.provider.key(), conversation.id];
        self.write(|transaction| {
            let stored = find_turn(transaction, conversation, &turn.id, false, |stored| {
                removed_item
                    .map_or_else(
                        || stored.shares_items(turn),
                        |id| stored.items.iter().flatten().any(|item| &item.id == id),
                    )
                    .then_some(stored)
            })?;
            let stored = match stored {
                None if removed_item.is_none() => {
                    find_turn(transaction, conversation, &turn.id, false, |stored| {
                        stored.matches_occurrence(turn).then_some(stored)
                    })?
                }
                stored => stored,
            };
            let mut turn = std::borrow::Cow::Borrowed(turn);
            let position = match stored {
                Some((position, stored)) => {
                    if turn.items.is_none() {
                        turn.to_mut().items = stored.items;
                    }
                    position
                }
                None => turn_count(transaction, conversation.provider.key(), &conversation.id)?,
            };
            transaction
                .execute(
                    "INSERT OR IGNORE INTO conversations (provider, id) VALUES (?1, ?2)",
                    key,
                )
                .map_err(failure)?;
            write_turn(transaction, conversation, position, &turn)?;
            advance(transaction, conversation.provider.key(), &conversation.id)
        })
    }

    /// Change the stored occurrence containing the item, or the latest occurrence
    /// for changes that do not name an item.
    pub(crate) fn update_turn(
        &self,
        conversation: &SessionRef,
        turn_id: &TurnId,
        item_id: Option<&ItemId>,
        change: impl FnOnce(Turn) -> Option<Turn>,
    ) -> Result<(), String> {
        self.write(|transaction| {
            let Some((position, turn)) =
                find_turn(transaction, conversation, turn_id, false, |turn| {
                    item_id
                        .is_none_or(|id| turn.items.iter().flatten().any(|item| &item.id == id))
                        .then_some(turn)
                })?
            else {
                return Ok(());
            };
            let Some(turn) = change(turn) else {
                return Ok(());
            };
            write_turn(transaction, conversation, position, &turn)?;
            advance(transaction, conversation.provider.key(), &conversation.id)
        })
    }

    /// No turn runs across a Host restart.
    pub(crate) fn interrupt_running(&self) -> Result<(), String> {
        self.write(|transaction| {
            let mut stopped = Vec::new();
            {
                let mut rows = transaction
                    .prepare("SELECT provider, id, position, body FROM turns WHERE running = 1")
                    .map_err(failure)?;
                let mut rows = rows.query([]).map_err(failure)?;
                while let Some(row) = rows.next().map_err(failure)? {
                    let mut turn = decode_turn(&row.get::<_, Vec<u8>>(3).map_err(failure)?)?;
                    turn.status = TurnStatus::Interrupted;
                    stopped.push((
                        row.get::<_, String>(0).map_err(failure)?,
                        row.get::<_, String>(1).map_err(failure)?,
                        row.get::<_, i64>(2).map_err(failure)?,
                        protocol::encode(&turn).map_err(failure)?,
                    ));
                }
            }
            for (provider, id, position, body) in stopped {
                transaction
                    .execute(
                        "UPDATE turns SET running = 0, body = ?4
                         WHERE provider = ?1 AND id = ?2 AND position = ?3",
                        params![provider, id, position, body],
                    )
                    .map_err(failure)?;
                advance(transaction, &provider, &id)?;
            }
            Ok(())
        })
    }

    /// Handoffs and forks read the entire committed transcript, without the UI
    /// window's byte limit. Recorded turns alone do not prove a complete import.
    pub(crate) fn history(
        &self,
        conversation: &SessionRef,
    ) -> Result<Option<ThreadResponse>, String> {
        self.with(|connection| {
            let key = params![conversation.provider.key(), conversation.id];
            let Some(metadata) = connection
                .query_row(
                    "SELECT metadata FROM conversations WHERE provider = ?1 AND id = ?2",
                    key,
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map_err(failure)?
                .flatten()
            else {
                return Ok(None);
            };
            let mut response: ThreadResponse = protocol::decode(&metadata).map_err(failure)?;
            let mut statement = connection
                .prepare("SELECT body FROM turns WHERE provider = ?1 AND id = ?2 ORDER BY position")
                .map_err(failure)?;
            let mut rows = statement.query(key).map_err(failure)?;
            let mut turns = Vec::new();
            while let Some(row) = rows.next().map_err(failure)? {
                turns.push(std::sync::Arc::new(decode_turn(
                    &row.get::<_, Vec<u8>>(0).map_err(failure)?,
                )?));
            }
            response.thread.turns = Some(turns);
            Ok(Some(response))
        })
    }

    /// The newest turns before `end`, or None until the conversation is imported.
    pub(crate) fn window(
        &self,
        conversation: &SessionRef,
        end: Option<usize>,
        limit: usize,
    ) -> Result<Option<Window>, String> {
        let key = params![conversation.provider.key(), conversation.id];
        self.with(|connection| {
            let Some(metadata) = connection
                .query_row(
                    "SELECT metadata FROM conversations WHERE provider = ?1 AND id = ?2",
                    key,
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map_err(failure)?
                .flatten()
            else {
                return Ok(None);
            };
            let count =
                turn_count(connection, conversation.provider.key(), &conversation.id)? as usize;
            let end = end.unwrap_or(count);
            if end > count {
                return Err("conversation history cursor is no longer valid".into());
            }
            let mut rows = connection
                .prepare(
                    "SELECT body FROM turns WHERE provider = ?1 AND id = ?2 AND position < ?3
                     ORDER BY position DESC LIMIT ?4",
                )
                .map_err(failure)?;
            let mut rows = rows
                .query(params![
                    conversation.provider.key(),
                    conversation.id,
                    end as i64,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ])
                .map_err(failure)?;
            let (mut turns, mut bytes) = (Vec::new(), 0);
            while let Some(row) = rows.next().map_err(failure)? {
                let body = row.get::<_, Vec<u8>>(0).map_err(failure)?;
                if !turns.is_empty() && bytes + body.len() > WINDOW_BYTES {
                    break;
                }
                bytes += body.len();
                turns.push(std::sync::Arc::new(decode_turn(&body)?));
            }
            turns.reverse();
            let mut response: ThreadResponse = protocol::decode(&metadata).map_err(failure)?;
            let start = end - turns.len();
            response.thread.turns = Some(turns);
            response.thread.transcript_version = transcript_version(connection, conversation)?;
            Ok(Some(Window { response, start }))
        })
    }

    /// Records the input a conversation's queue holds, as T3 Code keeps queued
    /// runs; an empty queue leaves nothing behind.
    pub(crate) fn save_queue(
        &self,
        conversation: &SessionRef,
        queue: &InputQueue,
    ) -> Result<(), String> {
        let key = params![conversation.provider.key(), conversation.id];
        self.write(|transaction| {
            if queue.inputs.is_empty() {
                transaction
                    .execute("DELETE FROM queues WHERE provider = ?1 AND id = ?2", key)
                    .map_err(failure)?;
            } else {
                transaction
                    .execute(
                        "INSERT INTO queues (provider, id, body) VALUES (?1, ?2, ?3)
                         ON CONFLICT (provider, id) DO UPDATE SET body = excluded.body",
                        params![
                            conversation.provider.key(),
                            conversation.id,
                            protocol::encode(queue).map_err(failure)?
                        ],
                    )
                    .map_err(failure)?;
            }
            Ok(())
        })
    }

    /// Every conversation's saved queue.
    pub(crate) fn queues(&self) -> Result<Vec<(SessionRef, InputQueue)>, String> {
        self.with(|connection| {
            let mut rows = connection
                .prepare("SELECT provider, id, body FROM queues")
                .map_err(failure)?;
            let mut rows = rows.query([]).map_err(failure)?;
            let mut queues = Vec::new();
            while let Some(row) = rows.next().map_err(failure)? {
                let provider =
                    serde_json::from_value(serde_json::Value::String(row.get(0).map_err(failure)?))
                        .map_err(failure)?;
                queues.push((
                    SessionRef::new(provider, row.get(1).map_err(failure)?).map_err(failure)?,
                    protocol::decode(&row.get::<_, Vec<u8>>(2).map_err(failure)?)
                        .map_err(failure)?,
                ));
            }
            Ok(queues)
        })
    }

    /// Imported conversations and their working directories.
    pub(crate) fn imported(&self) -> Result<Vec<(SessionRef, Option<String>)>, String> {
        self.with(|connection| {
            let mut rows = connection
                .prepare("SELECT provider, id, cwd FROM conversations WHERE metadata IS NOT NULL")
                .map_err(failure)?;
            let mut rows = rows.query([]).map_err(failure)?;
            let mut imported = Vec::new();
            while let Some(row) = rows.next().map_err(failure)? {
                let provider =
                    serde_json::from_value(serde_json::Value::String(row.get(0).map_err(failure)?))
                        .map_err(failure)?;
                imported.push((
                    SessionRef::new(provider, row.get(1).map_err(failure)?).map_err(failure)?,
                    row.get(2).map_err(failure)?,
                ));
            }
            Ok(imported)
        })
    }

    /// The latest stored occurrence of a turn of an imported conversation.
    pub(crate) fn turn(
        &self,
        conversation: &SessionRef,
        turn_id: &TurnId,
    ) -> Result<Option<Turn>, String> {
        self.with(|connection| {
            find_turn(connection, conversation, turn_id, true, Some)
                .map(|found| found.map(|(_, turn)| turn))
        })
    }

    pub(crate) fn item(
        &self,
        conversation: &SessionRef,
        turn_id: &TurnId,
        item_id: &ItemId,
        imported_only: bool,
    ) -> Result<Option<std::sync::Arc<Item>>, String> {
        self.with(|connection| {
            find_turn(connection, conversation, turn_id, imported_only, |turn| {
                turn.items?.into_iter().find(|item| &item.id == item_id)
            })
            .map(|found| found.map(|(_, item)| item))
        })
    }

    /// Names change on the Host; the imported snapshot must follow them.
    pub(crate) fn rename(
        &self,
        conversation: &SessionRef,
        name: Option<&str>,
    ) -> Result<(), String> {
        let key = params![conversation.provider.key(), conversation.id];
        self.write(|transaction| {
            let Some(metadata) = transaction
                .query_row(
                    "SELECT metadata FROM conversations WHERE provider = ?1 AND id = ?2",
                    key,
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map_err(failure)?
                .flatten()
            else {
                return Ok(());
            };
            let mut response: ThreadResponse = protocol::decode(&metadata).map_err(failure)?;
            response.thread.name = name.map(Into::into);
            transaction
                .execute(
                    "UPDATE conversations SET metadata = ?3 WHERE provider = ?1 AND id = ?2",
                    params![
                        conversation.provider.key(),
                        conversation.id,
                        protocol::encode(&response).map_err(failure)?
                    ],
                )
                .map_err(failure)?;
            advance(transaction, conversation.provider.key(), &conversation.id)
        })
    }
}

fn write_state(transaction: &Transaction, name: &str, bytes: &[u8]) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO host_state(name, body) VALUES (?1, ?2)
        ON CONFLICT(name) DO UPDATE SET body = excluded.body",
            params![name, bytes],
        )
        .map_err(failure)?;
    Ok(())
}

fn find_turn<T>(
    connection: &Connection,
    conversation: &SessionRef,
    turn_id: &TurnId,
    imported_only: bool,
    select: impl Fn(Turn) -> Option<T>,
) -> Result<Option<(i64, T)>, String> {
    let mut statement = connection
        .prepare_cached(
            "SELECT position, body FROM turns JOIN conversations USING (provider, id)
         WHERE provider = ?1 AND id = ?2 AND turn_id = ?3 AND (?4 = 0 OR metadata IS NOT NULL)
         ORDER BY position DESC",
        )
        .map_err(failure)?;
    let mut rows = statement
        .query(params![
            conversation.provider.key(),
            conversation.id,
            turn_id.as_str(),
            imported_only
        ])
        .map_err(failure)?;
    while let Some(row) = rows.next().map_err(failure)? {
        let turn = decode_turn(&row.get::<_, Vec<u8>>(1).map_err(failure)?)?;
        if let Some(selected) = select(turn) {
            return Ok(Some((row.get(0).map_err(failure)?, selected)));
        }
    }
    Ok(None)
}

fn store(
    transaction: &Transaction,
    conversation: &SessionRef,
    mut response: ThreadResponse,
    keep_recorded: bool,
) -> Result<u64, String> {
    let native = response.thread.turns.take().unwrap_or_default();
    // Live execution supplies status; a stored observation would go stale.
    response.thread.status = SessionStatus::Unknown;
    response.thread.history_cursor = None;
    response.thread.history_has_more = None;
    response.thread.history_read_state = None;
    let metadata = protocol::encode(&response).map_err(failure)?;
    let key = params![conversation.provider.key(), conversation.id];
    let mut recorded = Vec::new();
    if keep_recorded {
        let mut rows = transaction
            .prepare("SELECT body FROM turns WHERE provider = ?1 AND id = ?2 ORDER BY position")
            .map_err(failure)?;
        let mut rows = rows.query(key).map_err(failure)?;
        while let Some(row) = rows.next().map_err(failure)? {
            recorded.push(decode_turn(&row.get::<_, Vec<u8>>(0).map_err(failure)?)?);
        }
    }
    // A turn recorded before its conversation was saved lacks the
    // provider attribution native reads add.
    let recorded_by_id = turns_by_id(recorded.iter());
    let native_by_id = turns_by_id(native.iter().map(AsRef::as_ref));
    let turns: Vec<std::borrow::Cow<Turn>> = native
        .iter()
        .map(|native| {
            let stored = recorded_by_id
                .get(&native.id)
                .into_iter()
                .flatten()
                .copied()
                .find(|turn| native.matches_occurrence(turn));
            match stored {
                Some(turn) => std::borrow::Cow::Owned(Turn {
                    provider: turn.provider.or(native.provider),
                    supports_fork: turn.supports_fork.or(native.supports_fork),
                    ..turn.clone()
                }),
                None => std::borrow::Cow::Borrowed(native.as_ref()),
            }
        })
        .chain(
            recorded
                .iter()
                .filter(|turn| {
                    !native_by_id
                        .get(&turn.id)
                        .into_iter()
                        .flatten()
                        .any(|native| native.matches_occurrence(turn))
                })
                .map(std::borrow::Cow::Borrowed),
        )
        .collect();
    transaction
        .execute("DELETE FROM turns WHERE provider = ?1 AND id = ?2", key)
        .map_err(failure)?;
    for (position, turn) in turns.iter().enumerate() {
        write_turn(transaction, conversation, position as i64, turn)?;
    }
    transaction
                .execute(
                    "INSERT INTO conversations (provider, id, cwd, metadata) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (provider, id) DO UPDATE SET cwd = excluded.cwd, metadata = excluded.metadata",
                    params![conversation.provider.key(), conversation.id, response.thread.cwd, metadata],
                )
                .map_err(failure)?;
    advance(transaction, conversation.provider.key(), &conversation.id)?;
    transcript_version(transaction, conversation)?
        .ok_or_else(|| "stored transcript has no version".into())
}

/// Imports enumerate positions, and live writes only replace or append. The
/// final indexed position therefore gives the count without scanning the history.
fn turn_count(connection: &Connection, provider: &str, id: &str) -> Result<i64, String> {
    connection
        .prepare_cached(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM turns WHERE provider = ?1 AND id = ?2",
        )
        .map_err(failure)?
        .query_row(params![provider, id], |row| row.get(0))
        .map_err(failure)
}

fn transcript_version(
    connection: &Connection,
    conversation: &SessionRef,
) -> Result<Option<u64>, String> {
    connection
        .query_row(
            "SELECT version FROM versions WHERE provider = ?1 AND id = ?2",
            params![conversation.provider.key(), conversation.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(failure)
        .map(|version| version.map(|version| version as u64))
}

/// Give the conversation the next Host-wide version, using the version index.
fn advance(transaction: &Transaction, provider: &str, id: &str) -> Result<(), String> {
    transaction
        .prepare_cached(
            "INSERT INTO versions (provider, id, version)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(version), 0) + 1 FROM versions))
             ON CONFLICT (provider, id) DO UPDATE SET version = excluded.version",
        )
        .map_err(failure)?
        .execute(params![provider, id])
        .map_err(failure)?;
    Ok(())
}

fn write_turn(
    transaction: &Transaction,
    conversation: &SessionRef,
    position: i64,
    turn: &Turn,
) -> Result<(), String> {
    transaction
        .prepare_cached(
            "INSERT INTO turns (provider, id, position, turn_id, running, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (provider, id, position) DO UPDATE SET
                 turn_id = excluded.turn_id, running = excluded.running, body = excluded.body",
        )
        .map_err(failure)?
        .execute(params![
            conversation.provider.key(),
            conversation.id,
            position,
            turn.id.as_str(),
            turn.status == TurnStatus::Running,
            protocol::encode(turn).map_err(failure)?
        ])
        .map_err(failure)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        items::{AssistantPhase, ItemBody},
        models::{Item, Thread},
        session::ProviderKind,
    };
    use std::sync::Arc;

    fn conversation() -> SessionRef {
        SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap()
    }

    fn turn(id: &str, items: &[&str], status: TurnStatus) -> Turn {
        Turn {
            id: id.into(),
            status,
            items: Some(
                items
                    .iter()
                    .map(|item| {
                        Arc::new(Item::new(
                            (*item).into(),
                            Default::default(),
                            ItemBody::AssistantText {
                                text: format!("{id}/{item}"),
                                phase: AssistantPhase::Final,
                            },
                        ))
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn history(turns: Vec<Turn>) -> ThreadResponse {
        ThreadResponse {
            thread: Thread {
                id: Some(conversation()),
                name: Some("native".into()),
                status: SessionStatus::Running,
                turns: Some(turns.into_iter().map(Arc::new).collect()),
                ..Default::default()
            },
            model: None,
            effort: None,
            service_tier: None,
        }
    }

    fn store() -> (tempfile::TempDir, ConversationStore) {
        let root = tempfile::tempdir().unwrap();
        let store = ConversationStore::open(&root.path().join("state/transcripts.sqlite"));
        (root, store)
    }

    #[test]
    fn existing_unversioned_and_future_databases_are_rejected_without_changes() {
        for version in [0, 2] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("bex-transcripts.sqlite");
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(&format!(
                "CREATE TABLE saved (body TEXT); INSERT INTO saved VALUES ('keep'); PRAGMA user_version = {version};"
            )).unwrap();
            drop(connection);
            let before = std::fs::read(&path).unwrap();
            let store = ConversationStore::open(&path);
            assert!(store.check_ready().is_err());
            assert!(store.load_state::<Vec<String>>("bindings").is_err());
            assert!(store.save_state("bindings", &Vec::<String>::new()).is_err());
            assert_eq!(std::fs::read(path).unwrap(), before);
        }
    }

    #[test]
    fn metadata_without_a_database_requires_offline_conversion() {
        let root = tempfile::tempdir().unwrap();
        let metadata = root.path().join("bex-conversations.json");
        std::fs::write(&metadata, b"[]").unwrap();
        let store = ConversationStore::open(&root.path().join("bex-transcripts.sqlite"));
        assert!(store.check_ready().is_err());
        assert_eq!(std::fs::read(metadata).unwrap(), b"[]");
    }

    #[test]
    fn a_new_database_can_reopen_with_its_saved_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bex-transcripts.sqlite");
        let store = ConversationStore::open(&path);
        store.save_state("models", &vec!["selected"]).unwrap();
        drop(store);
        let store = ConversationStore::open(&path);
        store.check_ready().unwrap();
        assert_eq!(
            store.load_state::<Vec<String>>("models").unwrap(),
            ["selected"]
        );
    }

    #[test]
    fn offline_conversion_without_old_transcripts_preserves_metadata_and_accepts_history() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bex-transcripts.sqlite");
        let connection = Connection::open(&path).unwrap();
        // The offline converter only creates host_state when there was no
        // transcript DB; the Host owns initialization of its transcript tables.
        connection
            .execute_batch(
                "CREATE TABLE host_state (name TEXT PRIMARY KEY, body BLOB NOT NULL) WITHOUT ROWID;
             PRAGMA user_version = 1;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO host_state VALUES ('bindings', ?)",
                [br#"["retained"]"#.as_slice()],
            )
            .unwrap();
        drop(connection);
        let store = ConversationStore::open(&path);
        store.check_ready().unwrap();
        assert_eq!(
            store.load_state::<Vec<String>>("bindings").unwrap(),
            ["retained"]
        );
        store
            .import(
                &conversation(),
                None,
                history(vec![turn("t", &["i"], TurnStatus::Completed)]),
            )
            .unwrap();
        assert_eq!(
            texts(&store.window(&conversation(), None, 10).unwrap().unwrap()),
            ["t/i"]
        );
    }

    #[test]
    fn successful_commits_are_durable() {
        let (_root, store) = store();
        store
            .with(|connection| {
                let mode: String = connection
                    .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                    .map_err(failure)?;
                let synchronous: u32 = connection
                    .query_row("PRAGMA synchronous", [], |row| row.get(0))
                    .map_err(failure)?;
                assert_eq!(mode, "wal");
                assert_eq!(synchronous, 2); // PRAGMA synchronous=FULL
                Ok(())
            })
            .unwrap();
    }

    fn texts(window: &Window) -> Vec<String> {
        window
            .response
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .map(|item| match item.body() {
                ItemBody::AssistantText { text, .. } => text.clone(),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn imports_match_repeated_turn_ids_by_items_and_keep_unmatched_occurrences() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![
                    turn("reused", &["a"], TurnStatus::Completed),
                    turn("reused", &["b"], TurnStatus::Failed),
                    turn("other", &["tail"], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        let mut shared = turn("reused", &["b", "native-only"], TurnStatus::Running);
        shared.provider = Some(ProviderKind::Codex);
        shared.supports_fork = Some(true);
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![
                    shared,
                    turn("reused", &["new"], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        let saved = store
            .history(&target)
            .unwrap()
            .unwrap()
            .thread
            .turns
            .unwrap();
        assert_eq!(
            saved
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["reused", "reused", "reused", "other"]
        );
        assert_eq!(saved[0].status, TurnStatus::Failed);
        assert_eq!(saved[0].provider, Some(ProviderKind::Codex));
        assert_eq!(saved[0].supports_fork, Some(true));
        assert_eq!(
            saved
                .iter()
                .flat_map(|turn| turn.items.iter().flatten())
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "new", "a", "tail"]
        );

        // An itemless native observation matches the first stored occurrence
        // and suppresses all stored occurrences covered by that observation.
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("reused", &[], TurnStatus::Running)]),
            )
            .unwrap();
        let saved = store
            .history(&target)
            .unwrap()
            .unwrap()
            .thread
            .turns
            .unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].items.as_ref().unwrap()[0].id.as_str(), "b");
        assert_eq!(saved[1].id.as_str(), "other");
    }

    #[test]
    fn recorded_turns_stay_hidden_until_import_and_keep_their_items() {
        let (_root, store) = store();
        let target = conversation();
        store
            .record(
                &target,
                &turn("shared", &["live"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("later", &["after"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        assert!(store.window(&target, None, 10).unwrap().is_none());
        assert!(store.turn(&target, &"later".into()).unwrap().is_none());
        assert!(
            store
                .item(&target, &"later".into(), &"after".into(), true)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .item(&target, &"later".into(), &"after".into(), false)
                .unwrap()
                .is_some()
        );

        let mut native = turn("shared", &["live"], TurnStatus::Completed);
        Arc::make_mut(&mut native.items.as_mut().unwrap()[0]).body =
            agent_protocol::items::ItemContent::Inline {
                body: Box::new(ItemBody::AssistantText {
                    text: "native".into(),
                    phase: AssistantPhase::Final,
                }),
            };
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("first", &["a"], TurnStatus::Completed), native]),
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["first/a", "shared/live", "later/after"]);

        // A turn that finished while its native history was read keeps the
        // recorded result instead of the unfinished read.
        let running = SessionRef::new(ProviderKind::Codex, "running".into()).unwrap();
        store
            .record(
                &running,
                &turn("racing", &["start", "end"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        store
            .import(
                &running,
                store.version(&running).unwrap(),
                history(vec![turn("racing", &["start"], TurnStatus::Running)]),
            )
            .unwrap();
        let window = store.window(&running, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["racing/start", "racing/end"]);
        assert_eq!(
            window.response.thread.turns.unwrap()[0].status,
            TurnStatus::Completed
        );
        assert_eq!(window.start, 0);
        assert_eq!(window.response.thread.name.as_deref(), Some("native"));
        assert_eq!(window.response.thread.status, SessionStatus::Unknown);
    }

    #[test]
    fn failed_cached_writes_roll_back_and_recover_without_gaps_after_reopening() {
        let (root, store) = store();
        let target = conversation();
        store
            .import(&target, store.version(&target).unwrap(), history(vec![]))
            .unwrap();
        assert_eq!(store.window(&target, None, 10).unwrap().unwrap().start, 0);
        store
            .record(&target, &turn("first", &["a"], TurnStatus::Completed), None)
            .unwrap();
        let version = store
            .window(&target, None, 10)
            .unwrap()
            .unwrap()
            .response
            .thread
            .transcript_version;
        store
            .with(|connection| {
                connection
                    .execute_batch(
                        "CREATE TRIGGER reject_turn BEFORE INSERT ON turns
                         WHEN NEW.turn_id = 'rejected' OR NEW.running = 1
                         BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
                    )
                    .map_err(failure)
            })
            .unwrap();
        assert!(
            store
                .record(
                    &target,
                    &turn("rejected", &["b"], TurnStatus::Completed),
                    None
                )
                .is_err()
        );
        assert!(
            store
                .record(&target, &turn("first", &["a"], TurnStatus::Running), None)
                .is_err()
        );
        assert!(
            store
                .update_turn(&target, &"first".into(), None, |mut turn| {
                    turn.status = TurnStatus::Running;
                    Some(turn)
                })
                .is_err()
        );
        let unchanged = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&unchanged), ["first/a"]);
        assert_eq!(
            unchanged.response.thread.turns.as_ref().unwrap()[0].status,
            TurnStatus::Completed
        );
        assert_eq!(unchanged.response.thread.transcript_version, version);
        store
            .record(
                &target,
                &turn("second", &["b"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        drop(store);
        let store = ConversationStore::open(&root.path().join("state/transcripts.sqlite"));
        let latest = store.window(&target, None, 1).unwrap().unwrap();
        assert_eq!(latest.start, 1);
        assert_eq!(texts(&latest), ["second/b"]);
        // Replacing an earlier occurrence must not move the next append position.
        store
            .record(&target, &turn("first", &["a"], TurnStatus::Failed), None)
            .unwrap();
        store
            .record(&target, &turn("third", &["c"], TurnStatus::Completed), None)
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.start, 0);
        assert_eq!(texts(&window), ["first/a", "second/b", "third/c"]);
        store
            .with(|connection| {
                let counted: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM turns WHERE provider = ?1 AND id = ?2",
                        params![target.provider.key(), target.id],
                        |row| row.get(0),
                    )
                    .map_err(failure)?;
                assert_eq!(
                    turn_count(connection, target.provider.key(), &target.id)?,
                    counted
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn windows_page_backwards_and_stop_at_the_byte_budget() {
        let (_root, store) = store();
        let target = conversation();
        let turns = (0..12)
            .map(|index| turn(&index.to_string(), &["x"], TurnStatus::Completed))
            .collect();
        store
            .import(&target, store.version(&target).unwrap(), history(turns))
            .unwrap();
        let newest = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(
            (
                newest.start,
                newest.response.thread.turns.as_ref().unwrap().len()
            ),
            (2, 10)
        );
        let older = store
            .window(&target, Some(newest.start), 10)
            .unwrap()
            .unwrap();
        assert_eq!(texts(&older), ["0/x", "1/x"]);
        assert!(store.window(&target, Some(13), 10).is_err());

        let large = |id: &str| {
            let mut turn = turn(id, &["x"], TurnStatus::Completed);
            Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]).body =
                agent_protocol::items::ItemContent::Inline {
                    body: Box::new(ItemBody::AssistantText {
                        text: "x".repeat(WINDOW_BYTES / 2 + 1),
                        phase: AssistantPhase::Final,
                    }),
                };
            turn
        };
        let target = SessionRef::new(ProviderKind::Codex, "large".into()).unwrap();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![large("a"), large("b")]),
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(
            window.start, 1,
            "a window keeps one turn past the budget, never two"
        );
    }

    #[test]
    fn recording_replaces_the_same_occurrence_and_keeps_known_items() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("repeated", &["old"], TurnStatus::Completed)]),
            )
            .unwrap();
        // A reused turn ID with new items is a new occurrence.
        store
            .record(
                &target,
                &turn("repeated", &["new"], TurnStatus::Running),
                None,
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("repeated", &["new", "more"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let mut bare = turn("repeated", &[], TurnStatus::Failed);
        bare.items = None;
        store.record(&target, &bare, None).unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        let turns = window.response.thread.turns.as_ref().unwrap();
        assert_eq!(
            texts(&window),
            ["repeated/old", "repeated/new", "repeated/more"]
        );
        assert_eq!(turns[1].status, TurnStatus::Failed);
        assert_eq!(
            store
                .turn(&target, &"repeated".into())
                .unwrap()
                .unwrap()
                .status,
            TurnStatus::Failed
        );
        store
            .record(
                &target,
                &turn("repeated", &["old", "late"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.response.thread.turns.as_ref().unwrap().len(), 2);
        assert_eq!(
            texts(&window),
            [
                "repeated/old",
                "repeated/late",
                "repeated/new",
                "repeated/more"
            ]
        );
        assert_eq!(
            window.response.thread.turns.as_ref().unwrap()[1].status,
            TurnStatus::Failed
        );
    }

    #[test]
    fn known_items_identify_older_occurrences_before_empty_later_turns() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![
                    turn("repeated", &["old"], TurnStatus::Completed),
                    turn("repeated", &[], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("repeated", &["old", "late"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["repeated/old", "repeated/late"]);
        let turns = window.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].items.as_ref().unwrap().len(), 2);
        assert!(turns[1].items.as_ref().unwrap().is_empty());
    }

    #[test]
    fn removing_an_unrecorded_occurrence_does_not_replace_an_older_one() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("repeat", &["old"], TurnStatus::Completed)]),
            )
            .unwrap();
        // A completion can fail to persist before its last item is removed.
        store
            .record(
                &target,
                &turn("repeat", &[], TurnStatus::Completed),
                Some(&"unrecorded".into()),
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["repeat/old"]);
        let turns = window.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].items.as_ref().unwrap()[0].id.as_str(), "old");
        assert!(turns[1].items.as_ref().unwrap().is_empty());
    }

    #[test]
    fn restarts_interrupt_running_turns_and_renames_follow_imports() {
        let (_root, store) = store();
        let target = conversation();
        store
            .rename(&target, Some("ignored before import"))
            .unwrap();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("running", &["partial"], TurnStatus::Running)]),
            )
            .unwrap();
        store.rename(&target, Some("renamed")).unwrap();
        store.interrupt_running().unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.response.thread.name.as_deref(), Some("renamed"));
        assert_eq!(
            window.response.thread.turns.as_ref().unwrap()[0].status,
            TurnStatus::Interrupted
        );
    }

    #[test]
    fn late_updates_change_the_stored_turn_and_its_version() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![turn("done", &["answer"], TurnStatus::Completed)]),
            )
            .unwrap();
        let before = store
            .window(&target, None, 10)
            .unwrap()
            .unwrap()
            .response
            .thread
            .transcript_version;
        store
            .update_turn(&target, &"done".into(), None, |mut turn| {
                turn.items.as_mut()?.push(Arc::new(Item::new(
                    "late".into(),
                    Default::default(),
                    ItemBody::AssistantText {
                        text: "done/late".into(),
                        phase: AssistantPhase::Final,
                    },
                )));
                Some(turn)
            })
            .unwrap();
        store
            .update_turn(&target, &"missing".into(), None, |_| unreachable!())
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["done/answer", "done/late"]);
        assert!(window.response.thread.transcript_version > before);
    }

    #[test]
    fn every_change_gives_its_conversation_a_newer_version() {
        let (_root, store) = store();
        let target = conversation();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let version = |target: &SessionRef| {
            store
                .window(target, None, 10)
                .unwrap()
                .unwrap()
                .response
                .thread
                .transcript_version
                .unwrap()
        };
        store
            .import(&target, store.version(&target).unwrap(), history(vec![]))
            .unwrap();
        store
            .import(&other, store.version(&other).unwrap(), history(vec![]))
            .unwrap();
        let imported = version(&target);
        assert!(version(&other) > imported);
        store
            .record(&target, &turn("t", &["i"], TurnStatus::Running), None)
            .unwrap();
        let recorded = version(&target);
        assert!(recorded > version(&other));
        store.rename(&target, Some("renamed")).unwrap();
        let renamed = version(&target);
        assert!(renamed > recorded);
        store.interrupt_running().unwrap();
        assert!(version(&target) > renamed);
        assert_eq!(version(&target), version(&target), "reads keep the version");
    }

    #[test]
    fn complete_history_requires_an_import_and_is_not_limited_to_a_display_window() {
        let root = tempfile::tempdir().unwrap();
        let store = ConversationStore::open(&root.path().join("transcripts.sqlite"));
        let target = conversation();
        let mut first = turn("first", &["large"], TurnStatus::Completed);
        Arc::make_mut(&mut first.items.as_mut().unwrap()[0]).body =
            agent_protocol::items::ItemContent::Inline {
                body: Box::new(ItemBody::AssistantText {
                    text: "x".repeat(WINDOW_BYTES),
                    phase: AssistantPhase::Final,
                }),
            };
        store.record(&target, &first, None).unwrap();
        assert!(store.history(&target).unwrap().is_none());
        let second = turn("second", &["last"], TurnStatus::Completed);
        store
            .import(
                &target,
                store.version(&target).unwrap(),
                history(vec![first, second]),
            )
            .unwrap();
        assert!(store.window(&target, None, 10).unwrap().unwrap().start > 0);
        let complete = store.history(&target).unwrap().unwrap();
        assert_eq!(
            complete
                .thread
                .turns
                .unwrap()
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn an_unopened_store_fails_instead_of_reporting_empty_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let store = ConversationStore::open(root.path());
        assert!(store.window(&conversation(), None, 10).is_err());
        assert!(
            store
                .record(
                    &conversation(),
                    &turn("t", &["i"], TurnStatus::Completed),
                    None
                )
                .is_err()
        );
    }
}
