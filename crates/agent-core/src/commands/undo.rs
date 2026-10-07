//! Undo for thread actions: settling, snoozing, unpinning, archiving and
//! discarding a draft offer an Undo for five seconds. Consecutive actions of
//! one kind share a notice and are restored together. A later action of the
//! same kind on the same thread (or its reversal) expires an earlier Undo.
use super::build::LifecycleAction;
use crate::state::Draft;
use agent_domain::ThreadId;
use std::collections::BTreeMap;

/// How long an Undo stays offered after the latest undoable action.
pub const THREAD_UNDO_WINDOW_MS: i64 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadUndoAction {
    Settled,
    Snoozed,
    Unpinned,
    Archived,
    Discarded,
}
impl ThreadUndoAction {
    fn verb(self) -> &'static str {
        match self {
            Self::Settled => "Settled",
            Self::Snoozed => "Snoozed",
            Self::Unpinned => "Unpinned",
            Self::Archived => "Archived",
            Self::Discarded => "Discarded",
        }
    }
}

/// The compact confirmation the sidebar shows while an Undo is offered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadUndoNotice {
    pub action: ThreadUndoAction,
    pub count: u32,
    /// `Settled 2 threads` or `Discarded 1 draft`.
    pub label: String,
    /// When the Undo goes away, in milliseconds.
    pub expires_at_ms: i64,
}

/// What restores one undone action.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoRestore {
    /// Commands in order; `reopen` opens the thread again, as archiving had
    /// left it.
    Thread {
        thread: ThreadId,
        actions: Vec<LifecycleAction>,
        reopen: bool,
    },
    /// The discarded draft under its key, unless new content replaced it.
    Draft { key: String, draft: Box<Draft> },
}

/// One action kind on one thread or draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoClaim {
    key: (String, String),
    token: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct UndoEntry {
    action: ThreadUndoAction,
    claim: UndoClaim,
    restore: UndoRestore,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadUndo {
    claims: BTreeMap<(String, String), u64>,
    next_token: u64,
    /// Actions sent and not yet confirmed, by command id.
    pending: BTreeMap<String, UndoEntry>,
    /// Confirmed actions that can be undone, oldest first.
    live: Vec<UndoEntry>,
    expires_at_ms: Option<i64>,
}

impl ThreadUndo {
    /// Claims one kind of action on `key`; a later claim of that kind expires
    /// this one's Undo.
    pub fn begin(&mut self, kind: &str, key: &str) -> UndoClaim {
        self.next_token += 1;
        let claim = UndoClaim {
            key: (kind.to_owned(), key.to_owned()),
            token: self.next_token,
        };
        self.claims.insert(claim.key.clone(), claim.token);
        claim
    }

    /// Expires this kind's Undo on `key`, leaving other kinds alone.
    pub fn invalidate(&mut self, kind: &str, key: &str) {
        self.claims.remove(&(kind.to_owned(), key.to_owned()));
    }

    pub fn is_current(&self, claim: &UndoClaim) -> bool {
        self.claims.get(&claim.key) == Some(&claim.token)
    }

    fn finish(&mut self, claim: &UndoClaim) {
        if self.is_current(claim) {
            self.claims.remove(&claim.key);
        }
    }

    /// Holds an action until the command `command_id` is confirmed.
    pub fn await_confirmation(
        &mut self,
        command_id: &str,
        action: ThreadUndoAction,
        claim: UndoClaim,
        restore: UndoRestore,
    ) {
        self.pending.insert(
            command_id.to_owned(),
            UndoEntry {
                action,
                claim,
                restore,
            },
        );
    }

    /// The command went through: its action becomes undoable.
    pub fn confirmed(&mut self, command_id: &str, now_ms: i64) {
        if let Some(entry) = self.pending.remove(command_id) {
            self.show(entry.action, entry.claim, entry.restore, now_ms);
        }
    }

    /// The command failed: nothing to undo.
    pub fn failed(&mut self, command_id: &str) {
        if let Some(entry) = self.pending.remove(command_id) {
            self.finish(&entry.claim);
        }
    }

    /// Offers an Undo for a done action, unless a newer action of its kind
    /// already expired it. The window restarts.
    pub fn show(
        &mut self,
        action: ThreadUndoAction,
        claim: UndoClaim,
        restore: UndoRestore,
        now_ms: i64,
    ) {
        self.expire(now_ms);
        if !self.is_current(&claim) {
            return;
        }
        self.live.push(UndoEntry {
            action,
            claim,
            restore,
        });
        self.expires_at_ms = Some(now_ms + THREAD_UNDO_WINDOW_MS);
    }

    fn expire(&mut self, now_ms: i64) {
        if self.expires_at_ms.is_some_and(|at| now_ms >= at) {
            for entry in std::mem::take(&mut self.live) {
                self.finish(&entry.claim);
            }
            self.expires_at_ms = None;
        }
    }

    /// The trailing run of current actions of the latest kind.
    fn latest_group(&self, now_ms: i64) -> Vec<usize> {
        if self.expires_at_ms.is_none_or(|at| now_ms >= at) {
            return vec![];
        }
        let mut current = self
            .live
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, entry)| self.is_current(&entry.claim));
        let Some((index, latest)) = current.next() else {
            return vec![];
        };
        std::iter::once(index)
            .chain(
                current
                    .take_while(|(_, entry)| entry.action == latest.action)
                    .map(|(index, _)| index),
            )
            .collect()
    }

    pub fn notice(&self, now_ms: i64) -> Option<ThreadUndoNotice> {
        let group = self.latest_group(now_ms);
        let action = self.live.get(*group.first()?)?.action;
        let count = group.len() as u32;
        let noun = if action == ThreadUndoAction::Discarded {
            "draft"
        } else {
            "thread"
        };
        Some(ThreadUndoNotice {
            action,
            count,
            label: format!(
                "{} {count} {noun}{}",
                action.verb(),
                if count == 1 { "" } else { "s" }
            ),
            expires_at_ms: self.expires_at_ms?,
        })
    }

    /// Takes the group the notice shows, oldest first, consuming its claims so
    /// it is restored once; the preceding group is shown next.
    pub fn take_latest(&mut self, now_ms: i64) -> Vec<UndoRestore> {
        self.expire(now_ms);
        let group = self.latest_group(now_ms);
        let mut taken = vec![];
        let mut index = self.live.len();
        while index > 0 {
            index -= 1;
            if !self.is_current(&self.live[index].claim) || group.contains(&index) {
                let entry = self.live.remove(index);
                if group.contains(&index) {
                    self.finish(&entry.claim);
                    taken.push(entry.restore);
                }
            }
        }
        taken.reverse();
        taken
    }
}

#[cfg(test)]
mod tests;
