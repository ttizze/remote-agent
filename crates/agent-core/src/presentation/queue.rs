//! The Host queue's read model and action admission, shared by every client.
use crate::{
    localization::{UiLanguage, localized_ui_format},
    state::{Intent, follow_up::QueueAction},
};
use agent_protocol::{ids::ClientInputId, session::SessionRef};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueuedMessage {
    pub id: ClientInputId,
    pub fresh_context: bool,
    /// Plain preview; editing reads the owner's original parts, not this text.
    pub text: String,
    pub attachments: u32,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueControl {
    Edit,
    CancelEdit,
    Steer,
    Remove,
    MoveUp,
    MoveDown,
}

#[cfg(test)]
mod tests;

#[derive(Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueControlPresentation {
    pub action: QueueControl,
    pub title: String,
    pub enabled: bool,
}

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct QueuePresentation {
    pub(super) messages: Vec<QueuedMessage>,
    pub(super) held: bool,
    pub(super) resumable: bool,
    pub(super) editing: Option<ClientInputId>,
    pub(super) busy: bool,
    pub(super) can_steer: bool,
    pub(super) skips_compaction: bool,
    pub(super) can_change: bool,
    pub(super) edit_blocked: bool,
    pub(super) session: Option<SessionRef>,
    pub(super) language: UiLanguage,
}

impl QueuePresentation {
    pub(super) fn empty(language: UiLanguage) -> Arc<Self> {
        Arc::new(Self {
            messages: Vec::new(),
            held: false,
            resumable: false,
            editing: None,
            busy: false,
            can_steer: false,
            skips_compaction: false,
            can_change: false,
            edit_blocked: false,
            session: None,
            language,
        })
    }

    fn message(&self, id: &ClientInputId) -> Option<&QueuedMessage> {
        self.messages.iter().find(|message| &message.id == id)
    }

    /// A newly selected hidden editor must expose its Cancel control. A later
    /// explicit collapse is still the pane's choice, as T3's Show fewer.
    pub(super) fn reveal_edit(&self, previous: Option<&ClientInputId>) -> bool {
        self.editing
            .as_ref()
            .filter(|id| Some(*id) != previous)
            .and_then(|id| self.messages.iter().position(|message| &message.id == id))
            .is_some_and(|index| index >= self.messages(false).len())
    }

    fn steer_available(&self, message: &QueuedMessage) -> bool {
        self.can_steer && (!message.fresh_context || self.skips_compaction)
    }

    fn confirmed_ids(&self) -> Vec<ClientInputId> {
        self.messages
            .iter()
            .filter(|message| !message.pending)
            .map(|message| message.id.clone())
            .collect()
    }

    fn adjacent(&self, id: &ClientInputId, up: bool) -> Option<QueueAction> {
        let ids = self.confirmed_ids();
        let index = ids.iter().position(|message| message == id)?;
        let insert = if up { index.checked_sub(1)? } else { index + 2 };
        crate::state::follow_up::drop_at(&ids, id, insert)
    }

    fn allowed(
        &self,
        message: &QueuedMessage,
        control: QueueControl,
        allow_edited_input: bool,
    ) -> Option<QueueAction> {
        self.session.as_ref()?;
        let id = message.id.clone();
        let editing = self.editing.as_ref() == Some(&id);
        if control == QueueControl::CancelEdit {
            return editing.then_some(QueueAction::CancelEdit);
        }
        if !self.can_change || self.busy || message.pending {
            return None;
        }
        match control {
            QueueControl::Edit if !editing && !self.edit_blocked => Some(QueueAction::Edit {
                client_user_message_id: id,
            }),
            QueueControl::Steer
                if (!editing || allow_edited_input) && self.steer_available(message) =>
            {
                Some(QueueAction::Steer {
                    client_user_message_id: id,
                })
            }
            QueueControl::Remove if !editing => Some(QueueAction::Remove {
                client_user_message_id: id,
            }),
            QueueControl::MoveUp => self.adjacent(&id, true),
            QueueControl::MoveDown => self.adjacent(&id, false),
            _ => None,
        }
    }

    fn intent(&self, action: QueueAction) -> Option<Intent> {
        Some(Intent::Queue {
            thread_id: self.session.clone()?,
            action,
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl QueuePresentation {
    /// QueuedRunsControl shows the first two only when there are more than three.
    pub fn messages(&self, expanded: bool) -> Vec<QueuedMessage> {
        self.messages
            .iter()
            .take(if self.can_expand() && !expanded {
                2
            } else {
                self.messages.len()
            })
            .cloned()
            .collect()
    }
    pub fn can_expand(&self) -> bool {
        self.messages.len() > 3
    }
    pub fn message_count(&self) -> u32 {
        self.messages.len() as u32
    }
    pub fn editing(&self) -> Option<ClientInputId> {
        self.editing.clone()
    }
    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn held(&self) -> bool {
        self.held
    }

    pub fn resume(&self) -> Option<Intent> {
        (self.held && !self.busy && self.can_change && !self.messages.is_empty())
            .then(|| self.intent(QueueAction::Resume))
            .flatten()
    }
    pub fn equivalent(&self, other: Arc<Self>) -> bool {
        self == other.as_ref()
    }

    /// Rendering and callbacks use the same admission. Arrow commands select
    /// the current neighbor, even if the queue changed after a row was painted.
    pub fn action(&self, id: ClientInputId, control: QueueControl) -> Option<Intent> {
        self.intent(self.allowed(self.message(&id)?, control, false)?)
    }

    pub fn steer_next(&self) -> Option<Intent> {
        // The global shortcut addresses the stored queue, independently of an
        // unsaved editor; row controls continue to protect that edited input.
        let message = self.messages.iter().find(|message| !message.pending)?;
        self.intent(self.allowed(message, QueueControl::Steer, true)?)
    }

    pub fn edit_latest(&self) -> Option<Intent> {
        if self.editing.is_some() {
            return None;
        }
        self.action(
            self.messages
                .iter()
                .rev()
                .find(|message| !message.pending)?
                .id
                .clone(),
            QueueControl::Edit,
        )
    }

    pub fn controls(&self, id: ClientInputId) -> Vec<QueueControlPresentation> {
        let Some(message) = self.message(&id) else {
            return Vec::new();
        };
        let mut controls = if self.editing.as_ref() == Some(&id) {
            vec![QueueControl::CancelEdit]
        } else {
            let mut controls = Vec::new();
            if self.steer_available(message) {
                controls.push(QueueControl::Steer);
            }
            controls.extend([QueueControl::Edit, QueueControl::Remove]);
            controls
        };
        if !message.pending {
            controls.extend([QueueControl::MoveUp, QueueControl::MoveDown]);
        }
        controls
            .into_iter()
            .map(|action| {
                let key = match action {
                    QueueControl::Edit => "コンポーザーで編集",
                    QueueControl::CancelEdit => "キャンセル",
                    QueueControl::Steer => "今すぐ送信",
                    QueueControl::Remove => "キューから削除",
                    QueueControl::MoveUp => "上へ移動",
                    QueueControl::MoveDown => "下へ移動",
                };
                QueueControlPresentation {
                    action,
                    title: self.language.text(key).into(),
                    enabled: self.allowed(message, action, false).is_some(),
                }
            })
            .collect()
    }

    /// Native drag geometry supplies a stable destination identity. The common
    /// owner resolves its current index and rejects stale, pending or busy rows.
    pub fn drop_on(&self, id: ClientInputId, target: ClientInputId, after: bool) -> Option<Intent> {
        if !self.can_change || self.busy {
            return None;
        }
        let ids = self.confirmed_ids();
        let index = ids.iter().position(|id| id == &target)? + usize::from(after);
        self.intent(crate::state::follow_up::drop_at(&ids, &id, index)?)
    }

    pub fn labels(&self) -> HashMap<String, String> {
        [
            ("title", "キューのメッセージ"),
            ("saving", "キューへ保存中"),
            ("editing", "コンポーザーで編集中"),
            ("freshContext", "新しいセッション"),
            ("less", "表示を減らす"),
            (
                "reorder",
                "キューのメッセージを並べ替え（ドラッグ、または矢印キー）",
            ),
        ]
        .into_iter()
        .map(|(key, source)| (key.into(), self.language.text(source).into()))
        .chain(std::iter::once((
            "more".into(),
            localized_ui_format(
                self.language,
                "キューの{0}件をすべて表示".into(),
                vec![self.messages.len().to_string()],
            ),
        )))
        .collect()
    }

    pub fn attachment_label(&self, id: ClientInputId) -> Option<String> {
        let message = self.message(&id)?;
        (message.attachments > 0).then(|| {
            localized_ui_format(
                self.language,
                "添付 {0}件".into(),
                vec![message.attachments.to_string()],
            )
        })
    }
}
