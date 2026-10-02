//! Immutable conversation projection shared by desktop, Swift and Kotlin.
//!
//! This module owns the public data and binding surface. Private modules handle
//! cache reconciliation, item materialization, turn layout, requests, and wording.
mod items;
mod projection;
mod requests;
mod rows;
mod status;

use super::body;
use crate::{
    models,
    state::{PendingSubmission, Snapshot},
};
use agent_protocol::requests::Request as WireRequest;
use status::progress_label;
#[cfg(test)]
use status::turn_error;
use std::sync::Arc;

impl Snapshot {
    /// Display pending input before a new conversation has a server ID.
    pub fn conversation_thread(&self) -> Option<Arc<models::Thread>> {
        if let Some(source) = self
            .navigation
            .thread_id
            .as_ref()
            .and_then(|id| self.conversations.get(id))
        {
            return Some(source.clone());
        }
        self.pending_submissions
            .values()
            .any(|pending| pending.draft_key == self.navigation.draft_key)
            .then(|| {
                Arc::new(models::Thread {
                    ..Default::default()
                })
            })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedConversation {
    pub source: Arc<models::Thread>,
    pending: PendingItems,
    pub turns: Vec<Arc<RenderedTurn>>,
    pub queued: Vec<Arc<RenderedItem>>,
    pub request_rows: Vec<ConversationRow>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedTurn {
    pub source: Arc<models::Turn>,
    pending: PendingItems,
    requests: Vec<Arc<WireRequest>>,
    pub rows: Vec<ConversationRow>,
}
/// Native clients cache this layout per unchanged turn; expansion only filters activity rows.
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationRow {
    pub id: String,
    pub content: ConversationRowContent,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationRowContent {
    OlderItems {
        turn_id: agent_protocol::ids::TurnId,
    },
    User {
        item: Arc<RenderedItem>,
    },
    ActivityHeader {
        activity: ActivityPresentation,
    },
    Activity {
        item: Arc<RenderedItem>,
        turn_id: agent_protocol::ids::TurnId,
    },
    PendingRequest {
        request: Box<Request>,
    },
    Error {
        error: TurnErrorPresentation,
    },
    Response {
        item: Arc<RenderedItem>,
        fork_turn_id: Option<agent_protocol::ids::TurnId>,
    },
    InProgress {
        turn_id: agent_protocol::ids::TurnId,
    },
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityPresentation {
    pub id: String,
    pub status: String,
    pub activity_summary: String,
    pub activity_initially_expanded: bool,
    pub activity_can_collapse: bool,
    pub is_in_progress: bool,
}
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ActivityExpansion {
    pub status: String,
    pub expanded: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn activity_is_expanded(
    activity: &ActivityPresentation,
    choice: Option<ActivityExpansion>,
) -> bool {
    choice
        .filter(|choice| choice.status == activity.status)
        .map_or(activity.activity_initially_expanded, |choice| {
            choice.expanded
        })
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedTurn {
    pub fn conversation_rows(&self) -> Vec<ConversationRow> {
        self.rows.clone()
    }

    pub fn progress_label(&self, include_action: bool, now_seconds: f64) -> String {
        let action = self.rows.iter().rev().find_map(|row| match &row.content {
            ConversationRowContent::Activity { item, .. } if include_action => {
                Some(item.data.title.as_str())
            }
            _ => None,
        });
        progress_label(&self.source, action, now_seconds)
    }
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TurnErrorPresentation {
    pub title: String,
    pub message: String,
    pub details: Option<String>,
    pub is_reconnecting: bool,
}

pub enum ItemSource {
    Native(Arc<models::Item>),
    Pending(String, Arc<PendingSubmission>),
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RenderedItem {
    pub source: ItemSource,
    pub data: ItemPresentation,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedItem {
    pub fn expanded_body(&self) -> String {
        match &self.source {
            ItemSource::Native(item) => body::expanded_body(item),
            ItemSource::Pending(..) => self.data.body.clone(),
        }
    }
}

/// Pass the previous projection to retain native render identities across deltas.
pub fn project_conversation(
    snapshot: &Snapshot,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    projection::project_conversation(snapshot, source, previous)
}

pub fn request(source: &WireRequest) -> Request {
    requests::request(source)
}

/// Native editors keep typed choice IDs separate from free text.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn build_question_answer(
    multiple: bool,
    text: String,
    choice_ids: Vec<String>,
) -> agent_protocol::requests::QuestionAnswer {
    requests::build_question_answer(multiple, text, choice_ids)
}

pub fn answer_from_json(
    body: &agent_protocol::requests::RequestBody,
    text: &str,
) -> Result<agent_protocol::requests::Answer, String> {
    requests::answer_from_json(body, text)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn request_input_default(body: agent_protocol::requests::RequestBody) -> String {
    requests::request_input_default(body)
}

pub type PendingItems = Vec<(agent_protocol::ids::ClientInputId, Arc<PendingSubmission>)>;
#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ItemPresentation {
    pub id: String,
    pub native_id: Option<agent_protocol::ids::ItemId>,
    pub body: String,
    pub image_sources: Vec<String>,
    pub image_placeholder: bool,
    pub deferred: bool,
    pub kind: String,
    pub title: String,
    pub collapsible: bool,
    pub visible: bool,
}
#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Request {
    pub id: agent_protocol::ids::RequestId,
    pub title: String,
    pub body: String,
    pub can_respond: bool,
    pub details: String,
    pub request_body: agent_protocol::requests::RequestBody,
}

/// Shared read-state wording; native views only render this projection.
pub fn history_notice(thread: &models::Thread) -> Option<String> {
    status::history_notice(thread)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod progress_tests {
    use super::*;
    #[test]
    fn elapsed_work_uses_provider_time_and_describes_the_current_tool() {
        for value in [
            serde_json::json!({"id":"t","startedAt":100.5}),
            serde_json::json!({"id":"t","startedAtMs":100500}),
        ] {
            let turn = serde_json::from_value(value).unwrap();
            assert_eq!(
                progress_label(&turn, Some("cargo test"), 108.5),
                "8秒 作業中 · cargo test"
            );
            assert_eq!(progress_label(&turn, None, 110.5), "10秒 作業中…");
            assert_eq!(progress_label(&turn, None, 99.), "0秒 作業中…");
        }
        assert_eq!(
            progress_label(&models::Turn::default(), None, 100.),
            "作業中…"
        );
    }
}
