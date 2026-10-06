use agent_domain::{
    Fact, FactBody, ItemKind, MessageId, NodeId, RequestStatus, ResponseCapability,
    RuntimeRequestId, State, ThreadShell,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One `thread_shells` row with the list summary sent to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellRow {
    pub project: String,
    pub archived: bool,
    pub deleted: bool,
    pub needs_recovery: bool,
    pub summary: Box<ThreadShell>,
}

/// Computes the list summary of a thread from its projection.
pub trait ShellProjector: Send + Sync {
    fn project(&self, state: &State) -> Option<ShellRow>;
}

pub struct ThreadShellProjector;
impl ShellProjector for ThreadShellProjector {
    fn project(&self, state: &State) -> Option<ShellRow> {
        let thread = state.thread.as_ref()?;
        Some(ShellRow {
            project: thread.project.clone(),
            archived: thread.archived_at.is_some(),
            deleted: thread.deleted_at.is_some(),
            needs_recovery: needs_recovery(state),
            summary: Box::new(agent_domain::shell(state)?),
        })
    }
}

/// Work that a restarted Host must hand back to the state machine through `Recover`:
/// unsettled effects and everything `Recover` closes, including background work and
/// native children that outlive a finished run.
pub fn needs_recovery(state: &State) -> bool {
    let app_owned_task = |task: &NodeId| {
        state
            .tasks
            .iter()
            .any(|candidate| &candidate.id == task && candidate.app_owned())
    };
    let message_request = |request: &RuntimeRequestId| {
        state.requests.iter().any(|candidate| {
            &candidate.id == request && candidate.capability == ResponseCapability::Message
        })
    };
    state.runs.iter().any(|run| run.status.blocking())
        || !state.captures.is_empty()
        || !state.rollbacks.is_empty()
        || state.native_owner.is_some()
        || !state.background_work.is_empty()
        || state.messages.iter().any(|message| message.streaming)
        || state.requests.iter().any(|request| {
            request.status == RequestStatus::Pending
                && request.capability != ResponseCapability::Message
        })
        || state
            .tasks
            .iter()
            .any(|task| !task.app_owned() && !task.status.terminal())
        || state.items.iter().any(|item| {
            !item.status.terminal()
                && !matches!(&item.kind, ItemKind::Subagent { task } if app_owned_task(task))
                && !matches!(&item.kind, ItemKind::UserInputRequest { request } if message_request(request))
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRow {
    pub message: MessageId,
    pub role: String,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchChanges {
    pub cleared: bool,
    pub upserts: Vec<SearchRow>,
}

/// Search indexes finished messages only; a deleted thread leaves the index.
pub fn search_changes(state: &State, facts: &[Fact]) -> SearchChanges {
    let mut changes = SearchChanges::default();
    let mut touched = BTreeSet::new();
    for fact in facts {
        match &fact.body {
            FactBody::ThreadDeleted => {
                changes.cleared = true;
                touched.clear();
            }
            FactBody::MessageCreated { id, .. }
            | FactBody::MessageFinished { id }
            | FactBody::MessageEdited { id, .. } => {
                touched.insert(id.clone());
            }
            // Provider output finishes and rewrites messages through their items.
            FactBody::ItemCompleted { id, .. } | FactBody::ItemTextReplaced { id, .. } => {
                if let Some(
                    ItemKind::AssistantMessage { message } | ItemKind::UserMessage { message },
                ) = state
                    .items
                    .iter()
                    .find(|item| &item.id == id)
                    .map(|item| &item.kind)
                {
                    touched.insert(message.clone());
                }
            }
            _ => {}
        }
    }
    if state
        .thread
        .as_ref()
        .is_some_and(|t| t.deleted_at.is_some())
    {
        return changes;
    }
    changes.upserts = state
        .messages
        .iter()
        .filter(|message| touched.contains(&message.id) && !message.streaming)
        .map(|message| SearchRow {
            message: message.id.clone(),
            role: serde_json::to_value(message.role)
                .ok()
                .and_then(|role| role.as_str().map(str::to_owned))
                .unwrap_or_default(),
            text: message.text.clone(),
            created_at: message.created_at.as_str().to_owned(),
        })
        .collect();
    changes
}

#[cfg(test)]
mod tests;
