//! Input sent while a turn runs, as T3 Code handles it. This device's follow-up
//! behavior decides whether a message waits in the Host's queue or steers the
//! running turn, and Mod+Enter or a Ctrl/⌘-click does the opposite for one
//! message. Waiting messages can be edited in the composer, removed, sent as a
//! steer now, and resumed after a stop or failure held them.
use super::{DraftKey, Effect, Snapshot, Thread};
use agent_protocol::{
    ids::ClientInputId,
    operations::{self as rpc, FollowUp, QueueChange},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// What Enter does in the composer, as T3 Code's send shortcut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SendShortcut {
    /// Enter sends.
    #[default]
    Enter,
    /// Enter sends a one-line prompt; a multiline one needs Mod+Enter.
    ModEnterMultiline,
    /// Only Mod+Enter sends.
    ModEnter,
}

impl SendShortcut {
    /// Whether Enter alone sends `text` rather than starting a new line.
    /// Mod+Enter always sends, and while a turn runs it sends the alternate.
    pub fn enter_sends(self, text: &str) -> bool {
        match self {
            Self::Enter => true,
            Self::ModEnterMultiline => !text.contains(['\n', '\r']),
            Self::ModEnter => false,
        }
    }
}

/// This device's composer behavior, shared by every Host and window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerPreferences {
    pub follow_up: FollowUp,
    pub send_shortcut: SendShortcut,
}

/// What a message sent now asks a running turn for: the preference, or its
/// opposite when the user asks for the alternate.
pub fn requested(preference: FollowUp, alternate: bool) -> FollowUp {
    match (preference, alternate) {
        (preference, false) => preference,
        (FollowUp::Queue, true) => FollowUp::Steer,
        (FollowUp::Steer, true) => FollowUp::Queue,
    }
}

/// A fresh context always begins its own turn, independently of the steer preference.
pub fn for_context(requested: FollowUp, fresh_context: bool) -> FollowUp {
    if fresh_context {
        FollowUp::Queue
    } else {
        requested
    }
}

/// Whether input sent to `thread` now waits in the Host's queue, as the Host
/// routes it: a running turn takes only a steer it can accept, and nothing
/// overtakes input already waiting unless a stop holds that input.
pub fn waits(thread: &Thread, follow_up: FollowUp) -> bool {
    let running = thread.active_turn_id().is_some();
    if working(thread) {
        !(running && follow_up == FollowUp::Steer && steers(thread))
    } else {
        !thread.queue.held && !thread.queue.inputs.is_empty()
    }
}

/// Whether the conversation works: a turn runs, or it runs without an answered
/// turn waiting on background work, which takes new input as its next turn
/// (T3 Code's Waiting).
pub fn working(thread: &Thread) -> bool {
    // The answered turn stays the one Stop ends while its work runs on.
    let waiting = !thread.background.is_empty()
        && thread
            .turns
            .iter()
            .flatten()
            .all(|turn| turn.status != crate::models::TurnStatus::Running);
    !waiting
        && (thread.active_turn_id().is_some()
            || thread.status == crate::models::SessionStatus::Running)
}

/// The conversation's provider can take input into its running turn.
pub fn steers(thread: &Thread) -> bool {
    thread.capabilities.unwrap_or_default().steer
}

/// A queued message loaded into its conversation's composer. The composer's own
/// text waits here until the edit is saved or cancelled.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedEdit {
    pub client_user_message_id: ClientInputId,
    /// The queued text, which tells an unsaved edit from an untouched one.
    pub original: String,
    /// The composer text the edit replaced.
    pub saved: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueAction {
    /// Loads the queued message into the composer; sending saves the edit.
    Edit {
        client_user_message_id: ClientInputId,
    },
    CancelEdit,
    Remove {
        client_user_message_id: ClientInputId,
    },
    /// Moves the queued message before `before`, or to the end without it.
    Move {
        client_user_message_id: ClientInputId,
        before: Option<ClientInputId>,
    },
    /// Sends the queued message into the running turn now.
    Steer {
        client_user_message_id: ClientInputId,
    },
    /// Starts a queue a stop or failure held.
    Resume,
}

/// Dropping queued message `id` at queue position `insert` (0..=len), as T3
/// Code's drag and arrow keys reorder its queue: before the message now
/// there, or at the end. None when it would stay where it is.
pub fn drop_at(ids: &[ClientInputId], id: &ClientInputId, insert: usize) -> Option<QueueAction> {
    let from = ids.iter().position(|queued| queued == id)?;
    if insert == from || insert == from + 1 || insert > ids.len() {
        return None;
    }
    Some(QueueAction::Move {
        client_user_message_id: id.clone(),
        before: ids.get(insert).cloned(),
    })
}

/// The text a queued message shows and edits.
pub fn queued_text(input: &rpc::Submission) -> String {
    input
        .input
        .iter()
        .filter_map(|part| match part {
            rpc::Input::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn queued<'a>(thread: &'a Thread, id: &ClientInputId) -> Option<&'a rpc::Submission> {
    thread
        .queue
        .inputs
        .iter()
        .find(|input| &input.client_user_message_id == id)
}

fn set_text(snapshot: &mut Snapshot, key: &DraftKey, text: String) {
    let draft = Arc::make_mut(
        Arc::make_mut(&mut snapshot.input.drafts)
            .entry(key.clone())
            .or_default(),
    );
    draft.text = text;
}

/// Ends the conversation's edit and gives the composer back its own text.
fn end_edit(snapshot: &mut Snapshot, thread_id: &crate::session::SessionRef) {
    if let Some(edit) = Arc::make_mut(&mut snapshot.input.queued_edits).remove(thread_id) {
        set_text(snapshot, &thread_id.into(), edit.saved);
    }
}

pub(super) fn reduce(
    snapshot: &mut Snapshot,
    thread_id: crate::session::SessionRef,
    action: QueueAction,
) -> Vec<Effect> {
    let change = match action {
        QueueAction::Edit {
            client_user_message_id,
        } => {
            let Some(original) = snapshot
                .sessions
                .conversations
                .get(&thread_id)
                .and_then(|thread| queued(thread, &client_user_message_id))
                .map(queued_text)
            else {
                return Vec::new();
            };
            let key = DraftKey::from(&thread_id);
            let saved = match snapshot.input.queued_edits.get(&thread_id) {
                Some(edit) => edit.saved.clone(),
                None => snapshot
                    .input
                    .drafts
                    .get(&key)
                    .map(|draft| draft.text.clone())
                    .unwrap_or_default(),
            };
            set_text(snapshot, &key, original.clone());
            Arc::make_mut(&mut snapshot.input.queued_edits).insert(
                thread_id,
                QueuedEdit {
                    client_user_message_id,
                    original,
                    saved,
                },
            );
            return Vec::new();
        }
        QueueAction::CancelEdit => {
            end_edit(snapshot, &thread_id);
            return Vec::new();
        }
        QueueAction::Remove {
            client_user_message_id,
        } => {
            if snapshot
                .input
                .queued_edits
                .get(&thread_id)
                .is_some_and(|edit| edit.client_user_message_id == client_user_message_id)
            {
                end_edit(snapshot, &thread_id);
            }
            QueueChange::Remove {
                client_user_message_id,
            }
        }
        QueueAction::Move {
            client_user_message_id,
            before,
        } => QueueChange::Move {
            client_user_message_id,
            before,
        },
        QueueAction::Steer {
            client_user_message_id,
        } => QueueChange::Steer {
            client_user_message_id,
        },
        QueueAction::Resume => QueueChange::Resume,
    };
    vec![Effect::execute(rpc::ChangeQueue { thread_id, change })]
}

/// Sending while a queued message is edited saves the composer's text into it,
/// keeping the message's other parts, and gives the composer back its text.
pub(super) fn save_edit(
    snapshot: &mut Snapshot,
    thread_id: &crate::session::SessionRef,
) -> Option<Vec<Effect>> {
    let edit = snapshot.input.queued_edits.get(thread_id)?.clone();
    let text = snapshot
        .input
        .drafts
        .get(&DraftKey::from(thread_id))
        .map(|draft| draft.text.clone())
        .unwrap_or_default();
    let others = snapshot
        .sessions
        .conversations
        .get(thread_id)
        .and_then(|thread| queued(thread, &edit.client_user_message_id))
        .map(|input| {
            input
                .input
                .iter()
                .filter(|part| !matches!(part, rpc::Input::Text { .. }))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if text.trim().is_empty() && others.is_empty() {
        return Some(Vec::new());
    }
    end_edit(snapshot, thread_id);
    let input = (!text.trim().is_empty())
        .then_some(rpc::Input::Text { text })
        .into_iter()
        .chain(others)
        .collect();
    Some(vec![Effect::execute(rpc::ChangeQueue {
        thread_id: thread_id.clone(),
        change: QueueChange::Edit {
            client_user_message_id: edit.client_user_message_id,
            input,
        },
    })])
}

/// A queued message that started or was removed ends its edit, as in T3 Code:
/// an unsaved edit stays in a composer that had no text of its own, and is
/// otherwise dropped for that text.
pub(super) fn reconcile_edit(snapshot: &mut Snapshot, thread_id: &crate::session::SessionRef) {
    let Some(edit) = snapshot.input.queued_edits.get(thread_id) else {
        return;
    };
    if snapshot
        .sessions
        .conversations
        .get(thread_id)
        .is_none_or(|thread| queued(thread, &edit.client_user_message_id).is_some())
    {
        return;
    }
    let key = DraftKey::from(thread_id);
    let current = snapshot
        .input
        .drafts
        .get(&key)
        .map(|draft| draft.text.clone())
        .unwrap_or_default();
    let keep = current != edit.original && edit.saved.trim().is_empty();
    if keep {
        Arc::make_mut(&mut snapshot.input.queued_edits).remove(thread_id);
    } else {
        end_edit(snapshot, thread_id);
    }
}

#[cfg(test)]
mod tests;
