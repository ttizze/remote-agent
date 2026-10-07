//! Undo for thread actions: which commands claim or expire an Undo, and
//! restoring the latest group.
use super::{
    intents::{Next, invalid},
    owner::{Owner, now_ms},
};
use crate::{
    commands::{
        build::LifecycleAction,
        outbox::{PendingCommand, Request},
        undo::{ThreadUndoAction, UndoRestore},
    },
    peer::PeerError,
};
use agent_domain::Command;

impl Owner {
    /// A thread action about to be sent claims its Undo, or expires an
    /// earlier one it reverses or supersedes.
    pub(super) fn claim_undo(&mut self, entry: &PendingCommand) {
        let Request::Dispatch(dispatch) = &entry.request else {
            return;
        };
        let thread = dispatch.thread_id.clone();
        let key = thread.to_string();
        let row = self.state.thread_row(&thread).cloned();
        let undo = &mut self.state.thread_undo;
        let (action, kind, actions) = match &dispatch.command {
            Command::Pin { pinned: true, .. } | Command::ReorderPinned { .. } => {
                undo.invalidate("pin", &key);
                return;
            }
            Command::Pin { pinned: false, .. } => (
                ThreadUndoAction::Unpinned,
                "pin",
                vec![LifecycleAction::Pin {
                    order: row.and_then(|row| row.pin_order),
                }],
            ),
            Command::Settle { settled: true, .. } => {
                // Settling also clears the pin and the snooze, so Undo puts
                // them back, and an older unpin or snooze Undo would undo it.
                undo.invalidate("pin", &key);
                undo.invalidate("snooze", &key);
                let mut actions = vec![LifecycleAction::Unsettle];
                if let Some(row) = row {
                    if row.pinned_at.is_some() {
                        actions.push(LifecycleAction::Pin {
                            order: row.pin_order,
                        });
                    }
                    if let Some(until) = row.snoozed_until {
                        actions.push(LifecycleAction::Snooze { until });
                    }
                }
                (ThreadUndoAction::Settled, "settle", actions)
            }
            Command::Settle { settled: false, .. } => {
                undo.invalidate("settle", &key);
                return;
            }
            Command::Snooze { until: Some(_) } => (
                ThreadUndoAction::Snoozed,
                "snooze",
                vec![LifecycleAction::Unsnooze],
            ),
            Command::Snooze { until: None } => {
                undo.invalidate("snooze", &key);
                return;
            }
            Command::Archive { archived: true } => (
                ThreadUndoAction::Archived,
                "archive",
                vec![LifecycleAction::Unarchive],
            ),
            Command::Archive { archived: false } => {
                undo.invalidate("archive", &key);
                return;
            }
            _ => return,
        };
        // Undoing an archive brings the reader back when archiving left them.
        let reopen = action == ThreadUndoAction::Archived
            && self.state.selected_thread.as_ref() == Some(&thread);
        let undo = &mut self.state.thread_undo;
        let claim = undo.begin(kind, &key);
        undo.await_confirmation(
            entry.id.as_str(),
            action,
            claim,
            UndoRestore::Thread {
                thread,
                actions,
                reopen,
            },
        );
    }

    /// Drops a draft behind an Undo.
    pub(super) fn discard_draft(&mut self, key: String) {
        let Some(draft) = self.state.drafts.remove(&key) else {
            return;
        };
        let undo = &mut self.state.thread_undo;
        let claim = undo.begin("discard", &key);
        undo.show(
            ThreadUndoAction::Discarded,
            claim,
            UndoRestore::Draft { key, draft },
            now_ms() as i64,
        );
    }

    /// Restores the group the Undo notice shows.
    pub(super) fn undo_thread_action(&mut self) -> Result<Next, PeerError> {
        let mut commands = vec![];
        let mut error = None;
        for restore in self.state.thread_undo.take_latest(now_ms() as i64) {
            match restore {
                UndoRestore::Thread {
                    thread,
                    actions,
                    reopen,
                } => {
                    commands.extend(
                        actions
                            .into_iter()
                            .map(|action| self.lifecycle(thread.clone(), action)),
                    );
                    if reopen {
                        self.select_thread(Some(thread));
                    }
                }
                UndoRestore::Draft { key, draft } => {
                    if self
                        .state
                        .drafts
                        .get(&key)
                        .is_some_and(|current| !current.is_empty())
                    {
                        error = Some("Failed to restore draft: The draft has new content.");
                    } else {
                        self.state.drafts.insert(key, draft);
                    }
                }
            }
        }
        if let Some(error) = error {
            self.state.error = Some(error.into());
            if commands.is_empty() {
                return Err(invalid(error));
            }
        }
        Ok(if commands.is_empty() {
            Next::Done
        } else {
            Next::Commands(commands)
        })
    }
}
