//! Input sent while a turn runs, as T3 Code handles it. This device's follow-up
//! behavior decides whether a message waits in the Host's queue or steers the
//! running turn, and Mod+Enter or a Ctrl/⌘-click does the opposite for one
//! message. Waiting messages can be edited in the composer, removed, sent as a
//! steer now, and resumed after a stop or failure held them.
use super::{Draft, DraftKey, Effect, Snapshot, Thread};
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
    if working(thread) {
        !(follow_up == FollowUp::Steer && thread.steerable)
    } else {
        !thread.queue.held && !thread.queue.inputs.is_empty()
    }
}

/// Whether the conversation works: a turn runs, or it runs without an answered
/// turn waiting on background work, which takes new input as its next turn
/// (T3 Code's Waiting).
pub fn working(thread: &Thread) -> bool {
    thread.foreground_turn_id().is_some()
        || (thread.background.is_empty() && thread.status == crate::models::SessionStatus::Running)
}

/// The queued editor has its own draft identity, as T3's queuedRunEditDraftId.
/// The original conversation draft stays at its normal key while uploads and
/// callbacks keep the exact editor occurrence they started in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedEdit {
    pub client_user_message_id: ClientInputId,
    pub key: DraftKey,
    pub original: Arc<Draft>,
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

/// Dispose only this editor; the conversation's own draft never moved.
fn end_edit(snapshot: &mut Snapshot, thread_id: &crate::session::SessionRef) {
    if let Some(edit) = Arc::make_mut(&mut snapshot.input.queued_edits).remove(thread_id) {
        Arc::make_mut(&mut snapshot.input.drafts).remove(&edit.key);
    }
}

impl Snapshot {
    pub(crate) fn composer_key(
        &self,
        session: Option<&crate::session::SessionRef>,
        draft: &DraftKey,
    ) -> DraftKey {
        session
            .and_then(|id| self.input.queued_edits.get(id))
            .map_or_else(|| draft.clone(), |edit| edit.key.clone())
    }
    /// An upload may finish after cancellation. Its old key must not resurrect
    /// the editor or append to a later reopening of the same queued message.
    pub(crate) fn accepts_draft_result(&self, key: &DraftKey) -> bool {
        match key {
            DraftKey::Queued { session, .. } => self
                .input
                .queued_edits
                .get(session)
                .is_some_and(|edit| &edit.key == key),
            _ => true,
        }
    }
    pub(crate) fn retained_invocations(
        &self,
        key: &DraftKey,
    ) -> &[agent_protocol::composer::Invocation] {
        match key {
            DraftKey::Queued { session, .. } if self.accepts_draft_result(key) => {
                &self.input.queued_edits[session].original.invocations
            }
            _ => &[],
        }
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
            let Some(input) = snapshot
                .sessions
                .conversations
                .get(&thread_id)
                .and_then(|thread| queued(thread, &client_user_message_id))
            else {
                return Vec::new();
            };
            let conversation_key = DraftKey::from(&thread_id);
            let provider = input
                .model
                .as_ref()
                .map(|model| model.provider)
                .or_else(|| {
                    snapshot
                        .sessions
                        .conversations
                        .get(&thread_id)
                        .and_then(|thread| thread.active_provider)
                })
                .or(thread_id.provider);
            let Some(mut original) =
                crate::composer::input::from_input(&input.input, provider, input.fresh_context)
            else {
                return Vec::new();
            };
            let own = snapshot
                .input
                .drafts
                .get(&conversation_key)
                .cloned()
                .unwrap_or_default();
            original.model = input.model.clone().or_else(|| {
                own.model
                    .clone()
                    .filter(|model| Some(model.provider) == provider)
            });
            original.effort = input.effort.clone();
            original.service_tier = input.service_tier.clone();
            end_edit(snapshot, &thread_id);
            let key = DraftKey::Queued {
                session: thread_id.clone(),
                edit: uuid::Uuid::new_v4(),
            };
            let original = Arc::new(original);
            Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), original.clone());
            Arc::make_mut(&mut snapshot.input.queued_edits).insert(
                thread_id,
                QueuedEdit {
                    client_user_message_id,
                    key,
                    original,
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

/// Saving sends exactly the edited content. Keep both drafts until the Host
/// acknowledges it; an RPC failure must leave the edit available for retry.
pub(super) fn save_edit(
    snapshot: &Snapshot,
    key: &DraftKey,
    completing: Option<&super::op::OperationKey>,
) -> Vec<Effect> {
    let DraftKey::Queued {
        session: thread_id, ..
    } = key
    else {
        return Vec::new();
    };
    let Some(edit) = snapshot
        .input
        .queued_edits
        .get(thread_id)
        .filter(|edit| &edit.key == key)
    else {
        return Vec::new();
    };
    use super::op::OperationKey;
    let Some(thread) = snapshot.sessions.conversations.get(thread_id) else {
        return Vec::new();
    };
    if !snapshot.connection.connected
        || [
            OperationKey::Queue {
                session: thread_id.clone(),
            },
            OperationKey::Attachment {
                draft_key: key.clone(),
            },
            OperationKey::Dictation {
                draft_key: key.clone(),
            },
            OperationKey::Submission {
                draft_key: key.clone(),
            },
        ]
        .iter()
        .any(|operation| {
            Some(operation) != completing && snapshot.operation_running(operation.clone())
        })
    {
        return Vec::new();
    }
    let draft = snapshot
        .input
        .drafts
        .get(&edit.key)
        .cloned()
        .unwrap_or_default();
    let provider = queued(thread, &edit.client_user_message_id)
        .and_then(|input| input.model.as_ref().map(|model| model.provider))
        .or(thread.active_provider)
        .or(thread_id.provider);
    let input = crate::composer::input::to_input(
        draft.text.trim(),
        &draft.attachments,
        crate::composer::input::selected_invocations(
            &draft.invocations,
            &draft.text,
            provider,
            &edit.original.invocations,
        ),
        &draft.contexts,
    );
    if input.is_empty() {
        return Vec::new();
    }
    vec![Effect::execute(QueuedSave {
        session: thread_id.clone(),
        key: key.clone(),
        message: edit.client_user_message_id.clone(),
        submitted: draft,
        parts: input,
    })]
}

/// The local editor owns the submitted snapshot at dispatch, before scheduler
/// admission. Capturing the live draft only when the RPC starts can otherwise
/// close over newer text while sending older parts.
#[derive(Debug)]
struct QueuedSave {
    session: crate::session::SessionRef,
    key: DraftKey,
    message: ClientInputId,
    submitted: Arc<Draft>,
    parts: Vec<rpc::Input>,
}

impl super::op::Operation for QueuedSave {
    type Input = ();
    type Output = u64;
    fn key(&self) -> Option<super::op::OperationKey> {
        Some(super::op::OperationKey::Queue {
            session: self.session.clone(),
        })
    }
    fn navigation_bound(&self) -> bool {
        false
    }
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<(), crate::peer::PeerError> {
        snapshot
            .input
            .queued_edits
            .get(&self.session)
            .is_some_and(|edit| edit.key == self.key)
            .then_some(())
            .ok_or_else(|| {
                crate::peer::PeerError::InvalidMessage("queued editor was closed".into())
            })
    }
    async fn run(
        &self,
        _input: (),
        context: &mut crate::store::Execution<'_>,
    ) -> Result<Self::Output, crate::peer::PeerError> {
        super::op::command(
            context,
            &self.session,
            agent_protocol::conversation::Command::EditInput {
                id: self.message.clone(),
                parts: self.parts.clone(),
            },
        )
        .await
    }
    fn apply(self, snapshot: &mut Snapshot, _pane: &str, _sequence: Self::Output) -> Vec<Effect> {
        let Some(edit) = snapshot.input.queued_edits.get(&self.session) else {
            return Vec::new();
        };
        if edit.key != self.key {
            return Vec::new();
        }
        if snapshot
            .input
            .drafts
            .get(&self.key)
            .is_some_and(|current| current.same_content(&self.submitted))
        {
            end_edit(snapshot, &self.session);
        } else {
            // Input typed during the request is still an unsaved edit.
            Arc::make_mut(&mut snapshot.input.queued_edits)
                .get_mut(&self.session)
                .unwrap()
                .original = self.submitted;
        }
        Vec::new()
    }
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
    if snapshot.operation_running(super::op::OperationKey::Queue {
        session: thread_id.clone(),
    }) {
        return;
    }
    let key = edit.key.clone();
    let current = snapshot.input.drafts.get(&key).cloned().unwrap_or_default();
    let own_key = DraftKey::from(thread_id);
    let own = snapshot
        .input
        .drafts
        .get(&own_key)
        .cloned()
        .unwrap_or_default();
    let keep = !current.same_content(&edit.original)
        && own.text.trim().is_empty()
        && own.attachments.is_empty()
        && own.invocations.is_empty()
        && own.contexts.is_empty();
    end_edit(snapshot, thread_id);
    if keep {
        Arc::make_mut(&mut snapshot.input.drafts).insert(own_key, current);
    }
}

#[cfg(test)]
mod tests;
