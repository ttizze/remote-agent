//! One commit order for saved projections and the events used by every stream.
use super::*;
use std::collections::BTreeMap;

const THREAD_EVENTS: usize = 128;
const THREAD_BYTES: usize = 1024 * 1024;
const LIST_GAP: u64 = 1000;
const LIST_BYTES: usize = 8 * 1024 * 1024;

fn owner(key: &Key<SessionRef>) -> Option<&SessionRef> {
    match key {
        Key::Thread(id)
        | Key::Input { thread: id, .. }
        | Key::Turn { thread: id, .. }
        | Key::Item { thread: id, .. }
        | Key::Request { thread: id, .. } => Some(id),
        Key::Project(_) | Key::Catalog(_) => None,
    }
}

fn in_list(key: &Key<SessionRef>) -> bool {
    matches!(key, Key::Thread(_) | Key::Project(_) | Key::Catalog(_))
}

impl Storage {
    /// A prediction inside the single writer transaction, not a sequence update.
    pub(super) fn next_cursor(transaction: &Transaction<'_>) -> Result<Cursor, String> {
        let previous = cursor(transaction)?;
        Ok(Cursor {
            sequence: previous
                .sequence
                .checked_add(1)
                .ok_or("conversation sequence exhausted")?,
            ..previous
        })
    }

    /// The only owner that advances the stream. Receipt-only and private writes
    /// commit at the existing head; projection changes and events commit together.
    pub(super) fn commit(
        transaction: Transaction<'_>,
        target: Option<&SessionRef>,
        step: Step,
        rows: Vec<Row>,
        removed: Vec<Key<SessionRef>>,
        replaced: bool,
    ) -> Result<Committed, String> {
        let mut groups = BTreeMap::<Option<i64>, decision::records::Event>::new();
        let sequence = Self::next_cursor(&transaction)?;
        let event = || decision::records::Event {
            sequence: sequence.sequence,
            rows: Vec::new(),
            removed: Vec::new(),
        };
        for row in rows {
            if in_list(&row.key) {
                let visible = match &row.key {
                    Key::Thread(id) => transaction.query_row(
                        "SELECT loaded>=0 FROM threads WHERE ifnull(provider,'')=ifnull(?1,'') AND id=?2",
                        params![id.provider.map(ProviderKind::key),id.id], |row|row.get::<_,bool>(0)
                    ).optional().map_err(failure)?.unwrap_or(false),
                    _ => true,
                };
                if visible {
                    groups
                        .entry(None)
                        .or_insert_with(event)
                        .rows
                        .push(list::project(row.clone()));
                }
            }
            if let Some(id) = owner(&row.key) {
                let key = thread_key(&transaction, id)?;
                groups.entry(Some(key)).or_insert_with(event).rows.push(row);
            }
        }
        for key in removed {
            if in_list(&key) {
                groups
                    .entry(None)
                    .or_insert_with(event)
                    .removed
                    .push(key.clone());
            }
            if let Some(id) = owner(&key)
                && let Some(thread) = existing_key(&transaction, id)?
            {
                groups
                    .entry(Some(thread))
                    .or_insert_with(event)
                    .removed
                    .push(key);
            }
        }
        let replaced = if replaced {
            Some(target.ok_or("replacement requires a conversation")?)
        } else {
            None
        };
        let reset = replaced
            .map(|id| thread_key(&transaction, id))
            .transpose()?;
        if let Some(id) = replaced {
            let key = reset.expect("replaced conversation exists");
            let (header, _) = rows::header(&transaction, key)?;
            groups
                .entry(None)
                .or_insert_with(event)
                .rows
                .push(list::project(Row {
                    key: Key::Thread(id.clone()),
                    version: 0,
                    position: Position::Unordered,
                    content: Content::Full(Arc::new(Record::Thread(header))),
                }));
            groups.entry(Some(key)).or_insert_with(event);
        }
        let mut appended = false;
        for (thread, value) in groups.iter_mut() {
            // Multiple changes to one entity inside a transaction expose its
            // final value. Transactions themselves are never merged on replay.
            let mut unique = BTreeMap::new();
            for row in std::mem::take(&mut value.rows) {
                unique.insert(row.key.clone(), row);
            }
            value.removed.sort();
            value.removed.dedup();
            for key in &value.removed {
                unique.remove(key);
            }
            value.rows = unique.into_values().collect();
            decision::records::summarize(&mut value.rows, decision::records::WORK_DETAIL_BYTES)?;
            let is_reset = reset.is_some() && *thread == reset;
            if !is_reset && value.rows.is_empty() && value.removed.is_empty() {
                continue;
            }
            let body = if is_reset {
                Vec::new()
            } else {
                protocol::encode(&value).map_err(failure)?
            };
            transaction
                .execute(
                    "INSERT INTO journal(seq,thread,at,reset,body) VALUES (?1,?2,?3,?4,?5)",
                    params![
                        integer(sequence.sequence)?,
                        thread,
                        super::super::runtime::now(),
                        is_reset,
                        body
                    ],
                )
                .map_err(failure)?;
            appended = true;
        }
        let through = if appended {
            transaction
                .execute("UPDATE store SET seq=?1", [integer(sequence.sequence)?])
                .map_err(failure)?;
            sequence
        } else {
            cursor(&transaction)?
        };
        if let Some((operation, outcome)) = &step.receipt {
            let id = target.ok_or("receipt requires a conversation")?;
            transaction
                .execute(
                    "INSERT INTO receipts VALUES (?1,?2,?3,?4,?5)",
                    params![
                        operation.receipt_key(),
                        id.provider.map(ProviderKind::key),
                        id.id,
                        integer(through.sequence)?,
                        protocol::encode(outcome).map_err(failure)?
                    ],
                )
                .map_err(failure)?;
        }
        transaction.commit().map_err(failure)?;
        Ok(Committed {
            step,
            cursor: through,
        })
    }
}

/// The caller holds one read transaction for the head, retention fence, events
/// and snapshot fallback. Live delivery and reconnect call this same reader.
pub(super) fn replay(
    connection: &Connection,
    thread: Option<i64>,
    cached: Option<Cursor>,
    head: Cursor,
) -> Result<Option<Message>, String> {
    let pruned: i64 = connection
        .query_row("SELECT pruned_through FROM store", [], |row| row.get(0))
        .map_err(failure)?;
    if !agent_protocol::sync::can_resume(cached, head, pruned.try_into().map_err(failure)?) {
        return Ok(None);
    }
    let after = cached.expect("resumable cursor");
    if thread.is_none() && head.sequence.saturating_sub(after.sequence) > LIST_GAP {
        return Ok(None);
    }
    let limit = if thread.is_some() {
        THREAD_EVENTS
    } else {
        LIST_GAP as usize
    };
    let bytes = if thread.is_some() {
        THREAD_BYTES
    } else {
        LIST_BYTES
    };
    let mut query = connection.prepare(
        "SELECT reset,length(body),body FROM journal WHERE thread IS ?1 AND seq>?2 AND seq<=?3 ORDER BY seq LIMIT ?4"
    ).map_err(failure)?;
    let mut rows = query
        .query(params![
            thread,
            integer(after.sequence)?,
            integer(head.sequence)?,
            i64::try_from(limit + 1).map_err(failure)?
        ])
        .map_err(failure)?;
    let mut events = Vec::new();
    let mut raw_bytes = 0usize;
    while let Some(row) = rows.next().map_err(failure)? {
        let reset: bool = row.get(0).map_err(failure)?;
        let length = usize::try_from(row.get::<_, i64>(1).map_err(failure)?).map_err(failure)?;
        raw_bytes = raw_bytes.saturating_add(length);
        if reset || raw_bytes > bytes || events.len() >= limit {
            return Ok(None);
        }
        let body: Vec<u8> = row.get(2).map_err(failure)?;
        events.push(protocol::decode(&body).map_err(failure)?);
    }
    let message = Message::Events {
        through: head,
        events,
    };
    if protocol::encode(&message).map_err(failure)?.len() > bytes {
        return Ok(None);
    }
    Ok(Some(message))
}
