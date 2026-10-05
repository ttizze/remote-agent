use crate::{FactGap, StoredFact, ThreadHead};
use agent_domain::State;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct ThreadView {
    pub state: Arc<State>,
    pub head: ThreadHead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadSubscribe {
    /// The last global sequence the subscriber has folded; `None` asks for a snapshot.
    pub after_global_seq: Option<u64>,
    /// Updates buffered before a slow subscriber is closed.
    pub capacity: usize,
}
impl Default for ThreadSubscribe {
    fn default() -> Self {
        Self {
            after_global_seq: None,
            capacity: 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ThreadUpdate {
    Snapshot(ThreadView),
    /// The facts of one committed step, or the replayed gap.
    Facts(Arc<[StoredFact]>),
    Synchronized,
}

/// The stream closes when the subscriber falls behind; resubscribe with `after_global_seq`.
pub struct ThreadSubscription {
    pub updates: mpsc::Receiver<ThreadUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeGap {
    pub after: u64,
    pub high_water: u64,
    pub gap: FactGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    Replay,
    Snapshot,
}

/// Chooses between replaying the missed facts and sending a fresh snapshot.
pub trait ResumePolicy: Send + Sync {
    fn decide(&self, gap: &ResumeGap) -> ResumeDecision;
}

/// Placeholder bounds until the sync layer supplies its resume decision.
pub struct BoundedReplay;
impl ResumePolicy for BoundedReplay {
    fn decide(&self, resume: &ResumeGap) -> ResumeDecision {
        if resume.after > resume.high_water
            || resume.gap.facts > 128
            || resume.gap.bytes > 1024 * 1024
            || resume.gap.contains_created
        {
            ResumeDecision::Snapshot
        } else {
            ResumeDecision::Replay
        }
    }
}
