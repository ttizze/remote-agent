//! Ordered replication of saved rows. The same values and pure application
//! rules serve Host projections and client caches. Payload types belong to the
//! domain using the replica; no transport, database or provider state is held here.
use crate::ids::{ClientInputId, ItemId, RequestId, TurnId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod output;
pub use output::{Output, OutputError, OutputUpdate, TAIL_CHARS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub store: uuid::Uuid,
    pub sequence: u64,
}

/// Boundary values survive deletion of the item originally at the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub item: u64,
    pub turn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope<C> {
    Conversation(C),
    List,
}

/// Existing IDs need only be unique within a conversation, even when newly
/// allocated UUIDs happen to be globally unique.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Key<C> {
    Thread(C),
    Input { thread: C, id: ClientInputId },
    Turn { thread: C, id: TurnId },
    Item { thread: C, id: ItemId },
    Request { thread: C, id: RequestId },
    Project(String),
    Catalog(crate::session::ProviderKind),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Position {
    Unordered,
    Turn {
        ordinal: u64,
    },
    Item {
        turn: TurnId,
        ordinal: u64,
        position: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Content<T> {
    Full(T),
    Summary(T),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row<C, T> {
    pub key: Key<C>,
    pub version: u64,
    pub position: Position,
    pub content: Content<T>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal<C> {
    pub key: Key<C>,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message<C, T> {
    Snapshot {
        cursor: Cursor,
        window: Option<Window>,
        rows: Vec<Row<C, T>>,
    },
    /// Both live transactions and compacted catch-up describe complete coverage
    /// from one cursor through another. Numeric gaps are normal across scopes.
    Changes {
        from: Cursor,
        through: Cursor,
        rows: Vec<Row<C, T>>,
        removed: Vec<Removal<C>>,
    },
    Extend {
        cursor: Cursor,
        window: Window,
        rows: Vec<Row<C, T>>,
    },
    Hydrate {
        cursor: Cursor,
        row: Row<C, T>,
    },
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("a new subscription requires a strictly newer local generation")]
    Generation,
    #[error("the database identity changed; request a snapshot")]
    Store,
    #[error("the previous scope cursor does not match; subscribe again")]
    Discontinuity,
    #[error("the window does not match this scope")]
    Window,
    #[error("a row or removal has an invalid version")]
    Version,
    #[error("a change contains a duplicate entity")]
    Duplicate,
    #[error("row position does not match its entity kind")]
    Position,
    #[error("an item has no matching turn in the scope")]
    Parent,
    #[error("row identity does not match its content")]
    Identity,
}

/// The cache owner allocates a fresh subscription generation whenever it opens
/// a stream, including reconnects to a different Host. Generations are local
/// routing facts, not values trusted from a remote message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replica<C: Ord, T> {
    scope: Scope<C>,
    window: Option<Window>,
    expanded: bool,
    cursor: Option<Cursor>,
    rows: BTreeMap<Key<C>, Row<C, T>>,
    subscription: u64,
}

pub fn can_resume(cached: Option<Cursor>, current: Cursor, pruned: u64, history: u64) -> bool {
    cached.is_some_and(|cached| {
        cached.store == current.store
            && cached.sequence <= current.sequence
            && cached.sequence >= pruned
            && cached.sequence >= history
    })
}

impl<C: Ord + Clone, T: Clone> Replica<C, T> {
    pub fn new(scope: Scope<C>, subscription: u64) -> Self {
        Self {
            scope,
            window: None,
            expanded: false,
            cursor: None,
            rows: BTreeMap::new(),
            subscription,
        }
    }

    pub fn scope(&self) -> &Scope<C> {
        &self.scope
    }
    pub fn subscription(&self) -> u64 {
        self.subscription
    }
    pub fn snapshot(&self) -> Option<Message<C, T>> {
        Some(Message::Snapshot {
            cursor: self.cursor?,
            window: self.window,
            rows: self.rows.values().cloned().collect(),
        })
    }
    pub fn window(&self) -> Option<Window> {
        self.window
    }
    pub fn expanded(&self) -> bool {
        self.expanded
    }
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }
    pub fn rows(&self) -> &BTreeMap<Key<C>, Row<C, T>> {
        &self.rows
    }

    /// The connection owner supplies its routing generation. Keeping allocation
    /// there also fences messages when switching Hosts, not just this replica.
    pub fn resubscribe(&self, subscription: u64) -> Result<Self, ApplyError> {
        if subscription <= self.subscription {
            return Err(ApplyError::Generation);
        }
        Ok(Self {
            subscription,
            ..self.clone()
        })
    }

    /// Returns a whole new state, so a malformed frame never partially changes
    /// its caller's rows or advances the caller's resumable cursor.
    pub fn apply(&self, subscription: u64, message: Message<C, T>) -> Result<Self, ApplyError> {
        if subscription != self.subscription {
            return Ok(self.clone());
        }
        let mut next = self.clone();
        match message {
            Message::Snapshot {
                cursor,
                window,
                rows,
            } => {
                if matches!(
                    (&self.scope, window),
                    (Scope::List, Some(_)) | (Scope::Conversation(_), None)
                ) {
                    return Err(ApplyError::Window);
                }
                next.rows.clear();
                next.window = window;
                next.expanded = false;
                next.insert(rows, 0, cursor.sequence)?;
                next.cursor = Some(cursor);
            }
            Message::Changes {
                from,
                through,
                rows,
                removed,
            } => {
                let current = self.cursor.ok_or(ApplyError::Discontinuity)?;
                if current.store != through.store || from.store != through.store {
                    return Err(ApplyError::Store);
                }
                if through.sequence <= current.sequence {
                    return Ok(next);
                }
                if current != from {
                    return Err(ApplyError::Discontinuity);
                }
                let mut seen =
                    next.insert(rows, from.sequence.saturating_add(1), through.sequence)?;
                for removal in removed {
                    if !seen.insert(removal.key.clone()) {
                        return Err(ApplyError::Duplicate);
                    }
                    if removal.version <= from.sequence || removal.version > through.sequence {
                        return Err(ApplyError::Version);
                    }
                    next.remove(removal);
                }
                next.cursor = Some(through);
            }
            Message::Extend {
                cursor,
                window,
                rows,
            } => {
                if self.cursor != Some(cursor) {
                    return Err(ApplyError::Discontinuity);
                }
                let old = self.window.ok_or(ApplyError::Window)?;
                if (window.turn, window.item) > (old.turn, old.item) {
                    return Err(ApplyError::Window);
                }
                next.window = Some(window);
                next.expanded |= window != old;
                next.insert(rows, 0, cursor.sequence)?;
            }
            Message::Hydrate { cursor, row } => {
                if !self.cursor.is_some_and(|current| {
                    current.store == cursor.store && cursor.sequence <= current.sequence
                }) {
                    return Err(ApplyError::Discontinuity);
                }
                // A resend cannot introduce a new row to an already-complete
                // range, nor can it change row metadata or the scope cursor.
                if next
                    .rows
                    .get(&row.key)
                    .is_some_and(|old| old.version == row.version && old.position != row.position)
                {
                    return Err(ApplyError::Position);
                }
                if next
                    .rows
                    .get(&row.key)
                    .is_some_and(|old| old.version == row.version)
                {
                    next.insert(vec![row], 0, cursor.sequence)?;
                }
            }
            Message::Missing => {
                next.rows.clear();
                next.cursor = None;
                next.window = None;
            }
        }
        next.check_parents()?;
        Ok(next)
    }

    fn insert(
        &mut self,
        rows: Vec<Row<C, T>>,
        minimum: u64,
        maximum: u64,
    ) -> Result<BTreeSet<Key<C>>, ApplyError> {
        let mut seen = BTreeSet::new();
        for row in rows {
            if !seen.insert(row.key.clone()) {
                return Err(ApplyError::Duplicate);
            }
            if row.version < minimum || row.version > maximum {
                return Err(ApplyError::Version);
            }
            check_position(&row.key, &row.position)?;
            if !contains(&self.scope, self.window, &row.key, &row.position) {
                continue;
            }
            let replace = self.rows.get(&row.key).is_none_or(|old| {
                row.version > old.version
                    || (row.version == old.version
                        && matches!(
                            (&old.content, &row.content),
                            (Content::Summary(_), Content::Full(_))
                        ))
            });
            if replace {
                self.rows.insert(row.key.clone(), row);
            }
        }
        Ok(seen)
    }

    fn remove(&mut self, removal: Removal<C>) {
        if self
            .rows
            .get(&removal.key)
            .is_some_and(|row| row.version >= removal.version)
        {
            return;
        }
        self.rows.remove(&removal.key);
        if let Key::Turn { thread, id } = removal.key {
            self.rows.retain(|key, row| !matches!((key, &row.position),
                (Key::Item { thread: owner, .. }, Position::Item { turn, .. }) if owner == &thread && turn == &id));
        }
    }

    fn check_parents(&self) -> Result<(), ApplyError> {
        for row in self.rows.values() {
            if let (Key::Item { thread, .. }, Position::Item { turn, ordinal, .. }) =
                (&row.key, &row.position)
            {
                let parent = self.rows.get(&Key::Turn {
                    thread: thread.clone(),
                    id: turn.clone(),
                });
                if !parent.is_some_and(|row| row.position == (Position::Turn { ordinal: *ordinal }))
                {
                    return Err(ApplyError::Parent);
                }
            }
        }
        Ok(())
    }
}

fn check_position<C>(key: &Key<C>, position: &Position) -> Result<(), ApplyError> {
    match (key, position) {
        (Key::Turn { .. }, Position::Turn { .. }) | (Key::Item { .. }, Position::Item { .. }) => {
            Ok(())
        }
        (
            Key::Thread(_)
            | Key::Project(_)
            | Key::Catalog(_)
            | Key::Input { .. }
            | Key::Request { .. },
            Position::Unordered,
        ) => Ok(()),
        _ => Err(ApplyError::Position),
    }
}

pub fn contains<C: Eq>(
    scope: &Scope<C>,
    window: Option<Window>,
    key: &Key<C>,
    position: &Position,
) -> bool {
    match (scope, key) {
        (Scope::List, Key::Thread(_) | Key::Project(_) | Key::Catalog(_)) => true,
        (Scope::Conversation(owner), Key::Thread(thread) | Key::Input { thread, .. } | Key::Request { thread, .. }) => owner == thread,
        (Scope::Conversation(owner), Key::Turn { thread, .. }) => owner == thread && window.is_some_and(|floor| matches!(position, Position::Turn { ordinal } if *ordinal >= floor.turn)),
        (Scope::Conversation(owner), Key::Item { thread, .. }) => owner == thread && window.is_some_and(|floor| matches!(position, Position::Item { position, ordinal, .. } if *ordinal > floor.turn || (*ordinal == floor.turn && *position >= floor.item))),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
