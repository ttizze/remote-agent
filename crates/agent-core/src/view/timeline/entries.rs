//! Timeline entries in the committed item order: messages, proposed plans,
//! work-log entries and lifecycle events, plus messages this device sent that
//! the thread has not folded yet.
use crate::view::work_log::WorkLogEntry;
use agent_domain::{
    Attachment, AttemptStatus, ContextTransferId, InputIntent, Item, MessageAuthor, MessageContext,
    MessageId, PlanId, Role, RunAttemptId, RunId, Timestamp,
};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub id: MessageId,
    pub role: Role,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
    pub run: Option<RunId>,
    pub streaming: bool,
    pub created_by: Option<MessageAuthor>,
    pub creation_source: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub input_intent: Option<InputIntent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposedPlan {
    pub id: PlanId,
    pub run: Option<RunId>,
    pub markdown: String,
    pub status: PlanStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// The attempt an entry's item belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineAttempt {
    pub id: RunAttemptId,
    pub run: RunId,
    pub ordinal: u64,
    pub status: AttemptStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineEntryKind {
    Message {
        message: ChatMessage,
        item: Option<Arc<Item>>,
    },
    ProposedPlan(ProposedPlan),
    Work(Box<WorkLogEntry>),
    /// A lifecycle item drawn on its own: interrupt, fork, subagent.
    Event(Arc<Item>),
    /// A provider handoff delivered to the run it precedes.
    Handoff {
        transfer: ContextTransferId,
        run: RunId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineEntry {
    pub id: String,
    pub created_at: Timestamp,
    pub attempt: Option<TimelineAttempt>,
    pub kind: TimelineEntryKind,
}

impl TimelineEntry {
    pub fn message(&self) -> Option<&ChatMessage> {
        match &self.kind {
            TimelineEntryKind::Message { message, .. } => Some(message),
            _ => None,
        }
    }
    pub fn work(&self) -> Option<&WorkLogEntry> {
        match &self.kind {
            TimelineEntryKind::Work(entry) => Some(entry),
            _ => None,
        }
    }
    /// The item an event or work entry presents.
    pub fn item(&self) -> Option<&Arc<Item>> {
        match &self.kind {
            TimelineEntryKind::Message { item, .. } => item.as_ref(),
            TimelineEntryKind::Work(entry) => entry.item.as_ref(),
            TimelineEntryKind::Event(item) => Some(item),
            TimelineEntryKind::ProposedPlan(_) | TimelineEntryKind::Handoff { .. } => None,
        }
    }
}
