//! The Working section (beta): which threads fold into it, and how the inbox
//! and the section order their rows.
use super::thread_sort::sort_newest_first;
use super::thread_summary::{RuntimeStatus, ThreadSummary};
use agent_domain::InteractionMode;
use std::collections::{BTreeMap, BTreeSet};

/// Busy with work that does not need the user: a running run, or one parked
/// on background work that will wake it. Requests, plan prompts and failures
/// stay in the inbox.
pub fn is_thread_working(thread: &ThreadSummary) -> bool {
    if thread.has_pending_approvals || thread.has_pending_user_input {
        return false;
    }
    if !thread.runtime_is_active() && thread.runtime_status() != Some(RuntimeStatus::Idle) {
        return false;
    }
    !(thread.interaction_mode == InteractionMode::Plan
        && thread.has_actionable_proposed_plan
        && thread.latest_run_settled())
}

impl ThreadSummary {
    /// The latest run ended and no longer owns the runtime.
    pub fn latest_run_settled(&self) -> bool {
        let Some(run) = &self.latest_run else {
            return false;
        };
        !run.status.is_active()
            && self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.active_run.as_ref())
                != Some(&run.id)
    }
}

/// Newest first by when each thread last came back to the user; `returns`
/// adds returns the Host does not stamp.
pub fn sort_inbox_threads_by_return<T: AsRef<ThreadSummary>>(
    threads: Vec<T>,
    returns: &InboxReturns,
) -> Vec<T> {
    sort_newest_first(threads, |thread| {
        let thread = thread.as_ref();
        let run = thread.latest_run.as_ref();
        [
            Some(thread.created_at),
            thread.unsettled_at,
            run.and_then(|run| run.requested_at),
            run.and_then(|run| run.completed_at),
            returns.returned_at(&thread.id),
        ]
        .into_iter()
        .flatten()
        .fold(0, i64::max)
    })
}

/// Newest first by the last message the user sent; finishing and waking do
/// not move a row.
pub fn sort_working_threads_by_send<T: AsRef<ThreadSummary>>(threads: Vec<T>) -> Vec<T> {
    sort_newest_first(threads, |thread| {
        let thread = thread.as_ref();
        let sent = match thread.latest_user_authored_message_at {
            Some(authored) => authored,
            None => thread.latest_run.as_ref().and_then(|run| run.requested_at),
        };
        thread.created_at.max(sent.unwrap_or(0))
    })
}

/// When this device saw each thread leave the Working section. The first
/// observation only takes a baseline, so opening the list never reshuffles it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxReturns {
    working: Option<BTreeSet<String>>,
    returns: BTreeMap<String, i64>,
}

impl InboxReturns {
    /// Call with every thread on each list rebuild, or `None` while the beta
    /// is off.
    pub fn observe(&mut self, threads: Option<&[ThreadSummary]>, now_ms: i64) {
        let Some(threads) = threads else {
            *self = Self::default();
            return;
        };
        let present: BTreeSet<_> = threads.iter().map(|thread| thread.id.clone()).collect();
        let working: BTreeSet<_> = threads
            .iter()
            .filter(|thread| is_thread_working(thread))
            .map(|thread| thread.id.clone())
            .collect();
        self.returns.retain(|id, _| present.contains(id));
        for id in self.working.iter().flatten() {
            if present.contains(id) && !working.contains(id) {
                self.returns.insert(id.clone(), now_ms);
            }
        }
        self.working = Some(working);
    }

    pub fn returned_at(&self, thread: &str) -> Option<i64> {
        self.returns.get(thread).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::thread_summary::{
        RunSummary,
        fixtures::{ms, run, runtime, summary},
    };

    fn thread(id: &str, working: bool) -> ThreadSummary {
        ThreadSummary {
            created_at: ms("2026-06-01T00:00:00.000Z"),
            runtime: working.then(|| runtime(RuntimeStatus::Running)),
            ..summary(id)
        }
    }

    #[test]
    fn stamps_a_thread_when_it_stops_working_but_never_on_the_first_observation() {
        let mut tracker = InboxReturns::default();
        tracker.observe(Some(&[thread("a", true), thread("b", false)]), 1);
        assert_eq!(tracker.returned_at("a"), None);
        assert_eq!(tracker.returned_at("b"), None);
        tracker.observe(Some(&[thread("a", false), thread("b", false)]), 2);
        assert!(tracker.returned_at("a").is_some());
        assert_eq!(tracker.returned_at("b"), None);
    }

    #[test]
    fn forgets_deleted_threads_and_resets_when_the_beta_turns_off() {
        let mut tracker = InboxReturns::default();
        tracker.observe(Some(&[thread("a", true), thread("b", true)]), 1);
        tracker.observe(Some(&[thread("a", false), thread("b", false)]), 2);
        tracker.observe(Some(&[thread("b", false)]), 3);
        assert_eq!(tracker.returned_at("a"), None);
        assert!(tracker.returned_at("b").is_some());
        tracker.observe(None, 4);
        assert_eq!(tracker.returned_at("b"), None);
        tracker.observe(Some(&[thread("b", true)]), 5);
        tracker.observe(Some(&[thread("b", false)]), 6);
        assert!(tracker.returned_at("b").is_some());
    }

    #[test]
    fn orders_working_threads_by_the_last_message_the_user_sent_not_by_later_runs() {
        let sent_first = ThreadSummary {
            latest_user_authored_message_at: Some(Some(ms("2026-06-01T01:00:00.000Z"))),
            latest_run: Some(RunSummary {
                requested_at: Some(ms("2026-06-01T04:00:00.000Z")),
                started_at: Some(ms("2026-06-01T04:00:00.000Z")),
                ..run("run:wake", RuntimeStatus::Running)
            }),
            ..thread("sent-first", true)
        };
        let sent_last = ThreadSummary {
            latest_user_authored_message_at: Some(Some(ms("2026-06-01T02:00:00.000Z"))),
            ..thread("sent-last", true)
        };
        let launched = ThreadSummary {
            latest_user_authored_message_at: Some(None),
            ..thread("launched", true)
        };
        let sorted = sort_working_threads_by_send(vec![launched, sent_first, sent_last]);
        assert_eq!(
            sorted.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["sent-last", "sent-first", "launched"]
        );
    }
}
