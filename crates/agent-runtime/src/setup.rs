//! Live worktree setup progress per thread. Memory only: a setup is tracked
//! from `begin` until its turn starts or it fails, plus a short retention so a
//! late subscriber still sees the outcome.
use crate::Clock;
use agent_domain::{
    ThreadId, WORKTREE_SETUP_DETAIL_MAX_LENGTH, WORKTREE_SETUP_ERROR_MAX_LENGTH,
    WORKTREE_SETUP_STAGE_ORDER, WORKTREE_SETUP_TAIL_LINE_MAX_LENGTH, WorktreeSetupPhase,
    WorktreeSetupSnapshot, WorktreeSetupStage, WorktreeSetupStageId, WorktreeSetupStageStatus,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

const TAIL_LINE_LIMIT: usize = 4;
/// Finished setups stay visible this long.
pub const FINISHED_RETENTION: Duration = Duration::from_secs(30);

/// Free text within the contract limit, ending in an ellipsis when cut.
fn clamp(text: &str, max: usize) -> String {
    if text.chars().map(char::len_utf16).sum::<usize>() <= max {
        return text.to_owned();
    }
    let mut units = 0;
    let mut clamped: String = text
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units < max
        })
        .collect();
    clamped.push('…');
    clamped
}

struct Tracked {
    snapshot: WorktreeSetupSnapshot,
    cancel: Option<CancellationToken>,
}

#[derive(Default)]
struct Inner {
    setups: HashMap<ThreadId, Tracked>,
    /// Sequences keep increasing across setups of one thread.
    last_sequence: HashMap<ThreadId, u64>,
    channels: HashMap<ThreadId, watch::Sender<Option<WorktreeSetupSnapshot>>>,
}

pub struct SetupTracker {
    clock: Arc<dyn Clock>,
    inner: Arc<Mutex<Inner>>,
}

impl Inner {
    fn publish(&mut self, thread: &ThreadId, snapshot: Option<WorktreeSetupSnapshot>) {
        if let Some(channel) = self.channels.get(thread) {
            channel.send_replace(snapshot);
            if channel.receiver_count() == 0 {
                self.channels.remove(thread);
            }
        }
    }
}

impl SetupTracker {
    pub fn new(clock: Arc<dyn Clock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            inner: Arc::default(),
        })
    }

    /// A fresh running snapshot for the thread, replacing any earlier one.
    /// Cancelling `cancel` stops the setup.
    pub fn begin(
        &self,
        thread: &ThreadId,
        branch: Option<String>,
        base_ref: Option<String>,
        stages: &[WorktreeSetupStageId],
        cancel: Option<CancellationToken>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let sequence = inner.last_sequence.get(thread).map_or(0, |last| last + 1);
        let snapshot = WorktreeSetupSnapshot {
            thread: thread.clone(),
            phase: WorktreeSetupPhase::Running,
            started_at: self.clock.now(),
            ended_at: None,
            branch,
            base_ref,
            worktree_path: None,
            setup_script: None,
            stages: WORKTREE_SETUP_STAGE_ORDER
                .into_iter()
                .filter(|id| stages.contains(id))
                .map(|id| WorktreeSetupStage {
                    id,
                    status: WorktreeSetupStageStatus::Pending,
                    started_at: None,
                    ended_at: None,
                    percent: None,
                    detail: None,
                    tail: vec![],
                })
                .collect(),
            error: None,
            sequence,
        };
        inner.last_sequence.insert(thread.clone(), sequence);
        inner.setups.insert(
            thread.clone(),
            Tracked {
                snapshot: snapshot.clone(),
                cancel,
            },
        );
        inner.publish(thread, Some(snapshot));
    }

    fn modify(
        &self,
        thread: &ThreadId,
        change: impl FnOnce(&mut Tracked),
    ) -> Option<WorktreeSetupSnapshot> {
        let mut inner = self.inner.lock().unwrap();
        let tracked = inner.setups.get_mut(thread)?;
        change(tracked);
        tracked.snapshot.sequence += 1;
        let snapshot = tracked.snapshot.clone();
        inner
            .last_sequence
            .insert(thread.clone(), snapshot.sequence);
        inner.publish(thread, Some(snapshot.clone()));
        Some(snapshot)
    }

    pub fn update(&self, thread: &ThreadId, change: impl FnOnce(&mut WorktreeSetupSnapshot)) {
        self.modify(thread, |tracked| change(&mut tracked.snapshot));
    }

    /// Changes one stage; its detail is clamped to the contract limit.
    pub fn stage(
        &self,
        thread: &ThreadId,
        stage: WorktreeSetupStageId,
        change: impl FnOnce(&mut WorktreeSetupStage),
    ) {
        self.update(thread, |snapshot| {
            if let Some(entry) = snapshot.stages.iter_mut().find(|entry| entry.id == stage) {
                change(entry);
                entry.detail = entry
                    .detail
                    .take()
                    .map(|detail| clamp(&detail, WORKTREE_SETUP_DETAIL_MAX_LENGTH));
            }
        });
    }

    /// Sets a stage's status and its start and end times; `detail` replaces the
    /// detail when given.
    pub fn stage_status(
        &self,
        thread: &ThreadId,
        stage: WorktreeSetupStageId,
        status: WorktreeSetupStageStatus,
        detail: Option<Option<String>>,
    ) {
        let at = self.clock.now();
        self.stage(thread, stage, |entry| {
            if entry.started_at.is_none() && status != WorktreeSetupStageStatus::Pending {
                entry.started_at = Some(at.clone());
            }
            entry.ended_at = match status {
                WorktreeSetupStageStatus::Running | WorktreeSetupStageStatus::Pending => None,
                _ => entry.ended_at.take().or(Some(at)),
            };
            entry.status = status;
            if let Some(detail) = detail {
                entry.detail = detail;
            }
        });
    }

    pub fn append_tail(&self, thread: &ThreadId, stage: WorktreeSetupStageId, line: &str) {
        self.stage(thread, stage, |entry| {
            entry
                .tail
                .push(clamp(line, WORKTREE_SETUP_TAIL_LINE_MAX_LENGTH));
            let excess = entry.tail.len().saturating_sub(TAIL_LINE_LIMIT);
            entry.tail.drain(..excess);
        });
    }

    /// Settles the setup; stages still running take the outcome. The snapshot is
    /// dropped after the retention window unless a newer setup replaced it.
    pub fn finish(
        &self,
        thread: &ThreadId,
        phase: WorktreeSetupPhase,
        error: Option<&str>,
    ) -> Option<WorktreeSetupSnapshot> {
        let at = self.clock.now();
        let snapshot = self.modify(thread, |tracked| {
            tracked.cancel = None;
            let snapshot = &mut tracked.snapshot;
            snapshot.phase = phase;
            snapshot.ended_at = Some(at.clone());
            snapshot.error = error.map(|error| clamp(error, WORKTREE_SETUP_ERROR_MAX_LENGTH));
            for entry in &mut snapshot.stages {
                if entry.status == WorktreeSetupStageStatus::Running {
                    entry.status = match phase {
                        WorktreeSetupPhase::Done | WorktreeSetupPhase::Running => {
                            WorktreeSetupStageStatus::Done
                        }
                        WorktreeSetupPhase::Cancelled => WorktreeSetupStageStatus::Skipped,
                        WorktreeSetupPhase::Failed => WorktreeSetupStageStatus::Failed,
                    };
                    entry.ended_at = Some(at.clone());
                }
            }
        })?;
        let (inner, thread, sequence) = (
            Arc::downgrade(&self.inner),
            thread.clone(),
            snapshot.sequence,
        );
        tokio::spawn(async move {
            tokio::time::sleep(FINISHED_RETENTION).await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            let mut inner = inner.lock().unwrap();
            if inner
                .setups
                .get(&thread)
                .is_some_and(|tracked| tracked.snapshot.sequence == sequence)
            {
                inner.setups.remove(&thread);
                // A subscriber that sees `None` accepts any later sequence.
                inner.last_sequence.remove(&thread);
                inner.publish(&thread, None);
            }
        });
        Some(snapshot)
    }

    /// Called right before the turn starts, so a late cancel cannot roll back a
    /// thread whose agent already began.
    pub fn mark_uncancellable(&self, thread: &ThreadId) {
        if let Some(tracked) = self.inner.lock().unwrap().setups.get_mut(thread) {
            tracked.cancel = None;
        }
    }

    /// Stops the running setup and waits until it settled. False when nothing
    /// runs or the setup is past cancellation.
    pub async fn cancel(&self, thread: &ThreadId) -> bool {
        let (token, mut changes) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(token) = inner
                .setups
                .get(thread)
                .filter(|tracked| tracked.snapshot.phase == WorktreeSetupPhase::Running)
                .and_then(|tracked| tracked.cancel.clone())
            else {
                return false;
            };
            (token, Self::channel(&mut inner, thread))
        };
        token.cancel();
        let _ = changes
            .wait_for(|snapshot| {
                snapshot
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.phase != WorktreeSetupPhase::Running)
            })
            .await;
        true
    }

    pub fn get(&self, thread: &ThreadId) -> Option<WorktreeSetupSnapshot> {
        self.inner
            .lock()
            .unwrap()
            .setups
            .get(thread)
            .map(|tracked| tracked.snapshot.clone())
    }

    fn channel(
        inner: &mut Inner,
        thread: &ThreadId,
    ) -> watch::Receiver<Option<WorktreeSetupSnapshot>> {
        let current = inner
            .setups
            .get(thread)
            .map(|tracked| tracked.snapshot.clone());
        inner
            .channels
            .entry(thread.clone())
            .or_insert_with(|| watch::Sender::new(current))
            .subscribe()
    }

    /// The current snapshot (or `None`), then the newest after every change. A
    /// slow subscriber only ever holds the latest state.
    pub fn subscribe(&self, thread: &ThreadId) -> watch::Receiver<Option<WorktreeSetupSnapshot>> {
        Self::channel(&mut self.inner.lock().unwrap(), thread)
    }
}

#[cfg(test)]
mod tests;
