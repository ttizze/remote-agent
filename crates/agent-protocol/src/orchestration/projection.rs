//! T3's thread snapshot and own-thread event stream. Visible positions are
//! separate from an item's thread-local ordinal, including inherited history.
use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: Thread,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub nodes: Vec<ExecutionNode>,
    pub subagents: Vec<Subagent>,
    pub provider_sessions: Vec<ProviderSession>,
    pub provider_threads: Vec<ProviderThread>,
    pub provider_turns: Vec<ProviderTurn>,
    pub runtime_requests: Vec<RuntimeRequest>,
    pub plans: Vec<Plan>,
    pub turn_items: Vec<Arc<TurnItem>>,
    pub checkpoint_scopes: Vec<CheckpointScope>,
    pub checkpoints: Vec<Checkpoint>,
    pub context_handoffs: Vec<ContextHandoff>,
    pub context_transfers: Vec<ContextTransfer>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Local,
    Inherited,
    Synthetic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: Visibility,
    pub source_thread_id: String,
    pub source_item_id: String,
    pub item: Arc<TurnItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshot {
    pub snapshot_sequence: u64,
    pub projection: ThreadProjection,
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ThreadStreamItem {
    Snapshot(ThreadSnapshot),
    Event { sequence: u64, event: Event },
    Synchronized,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadHistoryPage {
    pub snapshot_sequence: u64,
    pub items: Vec<ProjectedTurnItem>,
    pub next_cursor: Option<String>,
    pub has_more_history: bool,
}

/// Same visibility rules as T3's orchestrationV2Timeline. This transient index
/// is shared by snapshot reads and client updates; it is never saved or synced.
pub struct TurnItemVisibility<'a> {
    statuses: BTreeMap<&'a str, RunStatus>,
    superseded_roots: BTreeSet<(&'a str, &'a str)>,
    interrupt_runs: BTreeSet<Option<&'a str>>,
}

impl<'a> TurnItemVisibility<'a> {
    pub fn new(runs: &'a [Run], attempts: &'a [RunAttempt], items: &'a [Arc<TurnItem>]) -> Self {
        Self {
            statuses: runs
                .iter()
                .filter(|run| {
                    matches!(
                        run.status,
                        RunStatus::RolledBack | RunStatus::Queued | RunStatus::Cancelled
                    )
                })
                .map(|run| (run.id.as_str(), run.status))
                .collect(),
            superseded_roots: attempts
                .iter()
                .filter(|attempt| attempt.status == RunAttemptStatus::Superseded)
                .map(|attempt| (attempt.run_id.as_str(), attempt.root_node_id.as_str()))
                .collect(),
            interrupt_runs: items
                .iter()
                .filter(|item| matches!(item.body, TurnItemBody::RunInterruptRequest { .. }))
                .map(|item| item.run_id.as_deref())
                .collect(),
        }
    }

    pub fn visible(&self, item: &TurnItem) -> bool {
        let status = item.run_id.as_deref().and_then(|id| self.statuses.get(id));
        if status == Some(&RunStatus::RolledBack) {
            return false;
        }
        if matches!(status, Some(RunStatus::Queued | RunStatus::Cancelled))
            && matches!(&item.body,TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
        {
            return false;
        }
        !matches!((&item.body,item.run_id.as_deref(),item.node_id.as_deref()),
            (TurnItemBody::RunInterruptResult {..},Some(run),Some(node))
                if self.superseded_roots.contains(&(run,node)) && !self.interrupt_runs.contains(&Some(run)))
    }
}
