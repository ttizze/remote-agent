use agent_domain::{Fact, FactBody, MessageId, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One `thread_shells` row. The payload is the list summary sent to clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellRow {
    pub project: String,
    pub archived: bool,
    pub deleted: bool,
    pub needs_recovery: bool,
    pub payload: serde_json::Value,
}

/// Computes the list summary of a thread from its projection.
pub trait ShellProjector: Send + Sync {
    fn project(&self, state: &State) -> Option<ShellRow>;
}

/// Placeholder until agent-domain provides the shell summary.
pub struct ThreadRecordShell;
impl ShellProjector for ThreadRecordShell {
    fn project(&self, state: &State) -> Option<ShellRow> {
        let thread = state.thread.as_ref()?;
        Some(ShellRow {
            project: thread.project.clone(),
            archived: thread.archived_at.is_some(),
            deleted: thread.deleted_at.is_some(),
            needs_recovery: needs_recovery(state),
            payload: serde_json::json!({
                "thread": thread,
                "active_run": state.active_run().map(|run| (&run.id, run.status)),
            }),
        })
    }
}

/// Work that a restarted Host must hand back to the state machine through `Recover`.
pub fn needs_recovery(state: &State) -> bool {
    state.runs.iter().any(|run| run.status.blocking())
        || !state.captures.is_empty()
        || state.rollback.is_some()
        || !state.pending_forks.is_empty()
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

/// Attachment paths newly referenced by these facts.
pub fn attachment_paths(facts: &[Fact]) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for fact in facts {
        match &fact.body {
            FactBody::MessageCreated { attachments, .. }
            | FactBody::MessageEdited {
                attachments: Some(attachments),
                ..
            } => paths.extend(attachments.iter().map(|a| a.path.clone())),
            FactBody::RequestResolved { attachments, .. } => paths.extend(
                attachments
                    .values()
                    .flatten()
                    .map(|attachment| attachment.path.clone()),
            ),
            _ => {}
        }
    }
    paths.into_iter().collect()
}
