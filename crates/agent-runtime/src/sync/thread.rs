//! `subscribeThread`: a snapshot or a bounded replay, an optional completion marker,
//! then the facts of every later commit. Ported from T3 `ThreadStream.ts` and ws.ts.
use super::history::{PagePolicy, bounded_state};
use super::live::{LIVE_STREAM_MAX_BYTES, LiveReceiver};
use super::wire::client_state;
use crate::{StoredFact, ThreadHead};
use agent_domain::State;
use serde::Serialize;
use std::sync::Arc;

/// Facts a resume may replay before a snapshot is cheaper.
pub const RESUME_MAX_REPLAY_FACTS: u64 = 128;
/// Encoded bytes a resume may replay.
pub const RESUME_MAX_REPLAY_ENCODED_BYTES: u64 = 1_048_576;
/// Stored payload bytes a resume may decode.
pub const RESUME_MAX_RAW_PAYLOAD_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadSubscribe {
    /// The last global sequence the subscriber has folded; `None` asks for a snapshot.
    pub after_global_seq: Option<u64>,
    /// Send `Synchronized` once the snapshot or replay is out.
    pub request_completion_marker: bool,
    /// Accept a recent-window snapshot plus a history cursor instead of the full projection.
    pub accept_bounded_snapshot: bool,
    /// Updates buffered before a slow subscriber is closed.
    pub capacity: usize,
    /// Serialized fact bytes buffered before a slow subscriber is closed.
    pub max_bytes: u64,
}
impl Default for ThreadSubscribe {
    fn default() -> Self {
        Self {
            after_global_seq: None,
            request_completion_marker: false,
            accept_bounded_snapshot: false,
            capacity: 1024,
            max_bytes: LIVE_STREAM_MAX_BYTES,
        }
    }
}

/// The history window of a bounded snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotWindow {
    pub history_cursor: Option<String>,
    pub has_more_history: bool,
    pub latest_local_ordinal: Option<u64>,
    pub payload_budget_exceeded: bool,
}

#[derive(Debug, Clone)]
pub struct ThreadSnapshot {
    /// Live facts follow this global sequence.
    pub snapshot_seq: u64,
    pub thread_seq: u64,
    pub state: Arc<State>,
    /// Present when the snapshot is bounded.
    pub window: Option<SnapshotWindow>,
}

impl ThreadSnapshot {
    /// The full projection unless the subscriber accepts a bounded one.
    pub fn build(state: &Arc<State>, head: ThreadHead, bounded: bool) -> Self {
        if !bounded {
            return Self {
                snapshot_seq: head.global_seq,
                thread_seq: head.thread_seq,
                state: client_state(state),
                window: None,
            };
        }
        let bounded = bounded_state(&client_state(state), head.global_seq, PagePolicy::RECENT);
        Self {
            snapshot_seq: head.global_seq,
            thread_seq: head.thread_seq,
            state: Arc::new(bounded.state),
            window: Some(SnapshotWindow {
                history_cursor: bounded.history_cursor,
                has_more_history: bounded.has_more_history,
                latest_local_ordinal: bounded.latest_local_ordinal,
                payload_budget_exceeded: bounded.payload_budget_exceeded,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ThreadUpdate {
    Snapshot(ThreadSnapshot),
    /// The facts of one committed step, or the replayed gap.
    Facts(Arc<[StoredFact]>),
    Synchronized,
}

impl ThreadUpdate {
    /// What the update holds against a live budget: the encoded facts. Snapshots are
    /// the subscription's starting point and are not charged.
    pub fn live_bytes(&self) -> u64 {
        match self {
            Self::Facts(facts) => replay_encoded_bytes(facts),
            Self::Snapshot(_) | Self::Synchronized => 0,
        }
    }
}

/// The stream closes when the subscriber falls behind; resubscribe with `after_global_seq`.
pub struct ThreadSubscription {
    pub updates: LiveReceiver<ThreadUpdate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeInput {
    pub after: u64,
    pub high_water: u64,
    pub replay_facts: u64,
    pub replay_encoded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumePlan {
    Replay { after: u64, through: u64 },
    Snapshot,
}

/// Replays the gap after the client's cursor unless the cursor is ahead of the
/// store or the replay is larger than one snapshot should be.
pub fn decide_resume(input: ResumeInput) -> ResumePlan {
    if input.after > input.high_water
        || input.replay_facts > RESUME_MAX_REPLAY_FACTS
        || input.replay_encoded_bytes > RESUME_MAX_REPLAY_ENCODED_BYTES
    {
        return ResumePlan::Snapshot;
    }
    ResumePlan::Replay {
        after: input.after,
        through: input.high_water,
    }
}

/// Checked before stored payloads are decoded; separate from the encoded size.
pub fn replay_raw_payload_safe(raw_payload_bytes: u64) -> bool {
    raw_payload_bytes <= RESUME_MAX_RAW_PAYLOAD_BYTES
}

/// UTF-8 JSON bytes across replayed items.
pub fn replay_encoded_bytes<T: Serialize>(items: &[T]) -> u64 {
    items
        .iter()
        .map(|item| serde_json::to_vec(item).map_or(0, |json| json.len() as u64))
        .sum()
}

#[cfg(test)]
mod tests;
