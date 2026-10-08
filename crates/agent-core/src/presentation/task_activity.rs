//! Compact task state for system surfaces. Never expose message or command bodies.
use crate::models::task_active;
use crate::{models::SessionStatus, session::SessionRef, state::Snapshot};
use agent_protocol::live_activity::{TaskActivitySummary, task_phase};
use std::collections::BTreeSet;

struct TaskActivity {
    session: SessionRef,
    status: &'static str,
    ongoing: bool,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TaskActivityOverview {
    pub summary: TaskActivitySummary,
    pub sessions: Vec<SessionRef>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// Unknown or missing state retains a previously active task until completion is observed.
    pub fn task_activity_overview(
        &self,
        previous_sessions: Vec<SessionRef>,
    ) -> TaskActivityOverview {
        let tasks = self.task_activities();
        let mut statuses = Vec::new();
        let mut sessions = BTreeSet::new();
        for task in &tasks {
            if task.ongoing
                || (task.status == "unknown" && previous_sessions.contains(&task.session))
            {
                statuses.push(task.status);
                sessions.insert(task.session.clone());
            }
        }
        for previous in previous_sessions {
            if !tasks.iter().any(|task| task.session == previous) {
                statuses.push("unknown");
                sessions.insert(previous);
            }
        }
        TaskActivityOverview {
            summary: TaskActivitySummary::from_statuses(statuses),
            sessions: sessions.into_iter().collect(),
        }
    }
}

impl Snapshot {
    /// Include observed sessions even when search or pagination hides their list row.
    fn task_activities(&self) -> Vec<TaskActivity> {
        let summaries = self
            .threads
            .as_ref()
            .map(|list| list.data.as_slice())
            .unwrap_or_default();
        let ids: BTreeSet<_> = self
            .conversations
            .keys()
            .chain(self.activity.active.keys())
            .chain(summaries.iter().filter_map(|thread| thread.id.as_ref()))
            .collect();
        ids.into_iter()
            .map(|id| {
                let summary = summaries
                    .iter()
                    .find(|thread| thread.id.as_ref() == Some(id));
                let conversation = self.conversations.get(id).map(AsRef::as_ref);
                let live = conversation.filter(|_| self.subscriptions.contains_key(id));
                let source = live
                    .filter(|thread| thread.status != SessionStatus::Unknown)
                    .or(summary);
                let observed = self.activity.active.get(id).copied();
                let session_status = source.map(|thread| thread.status).unwrap_or_default();
                // Unsubscribed history may belong to an earlier run of this session.
                let latest = live.and_then(|thread| thread.turns.as_ref()?.last());
                let waiting = live.is_some_and(|thread| {
                    thread.requests.values().any(|request| {
                        request.delivery == crate::session::RequestDelivery::Awaiting
                    })
                });
                let known = session_status != SessionStatus::Unavailable
                    && (observed == Some(true)
                        || source.is_some_and(|thread| {
                            thread.list_stale != Some(true)
                                && thread.status != SessionStatus::Unknown
                        }));
                let (status, ongoing) = task_phase(
                    known,
                    task_active(observed, session_status),
                    waiting && observed != Some(false),
                    latest.map(|turn| turn.status),
                );
                TaskActivity {
                    session: id.clone(),
                    status,
                    ongoing,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::Thread,
        protocol::Notification,
        state::{Event, reduce},
    };
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn overview_retains_only_previously_active_unknown_tasks_and_clears_observed_completion() {
        let running = snapshot("running", "running");
        let overview = running.task_activity_overview(vec![]);
        assert_eq!(overview.summary.running, 1);
        let unknown = snapshot("unavailable", "completed");
        assert!(!unknown.task_activity_overview(vec![]).summary.ongoing());
        let retained = unknown.task_activity_overview(overview.sessions);
        assert_eq!(retained.summary.unknown, 1);
        let absent = Snapshot::default().task_activity_overview(retained.sessions);
        assert_eq!(absent.summary.unknown, 1);
        let ended = snapshot("idle", "completed").task_activity_overview(absent.sessions);
        assert!(!ended.summary.ongoing());
        assert!(ended.sessions.is_empty());
    }

    fn snapshot(status: &str, turn: &str) -> Snapshot {
        let thread: Thread = serde_json::from_value(json!({
            "id":{"provider":"codex","id":"task"}, "name":"Build the app",
            "status": status, "turns":[{"id":"turn", "status":turn}]
        }))
        .unwrap();
        let id = thread.id.clone().unwrap();
        Snapshot {
            connected: true,
            subscriptions: Arc::new([(id.clone(), uuid::Uuid::nil())].into()),
            conversations: Arc::new([(id, Arc::new(thread))].into()),
            ..Default::default()
        }
    }

    #[rstest::rstest]
    #[case("running", "completed", "running", true)]
    #[case("idle", "completed", "completed", false)]
    #[case("idle", "failed", "failed", false)]
    #[case("idle", "interrupted", "interrupted", false)]
    #[case("idle", "running", "finishing", true)]
    #[case("unavailable", "completed", "unknown", false)]
    fn task_state_does_not_infer_success_from_inactivity(
        #[case] session: &str,
        #[case] turn: &str,
        #[case] expected: &str,
        #[case] ongoing: bool,
    ) {
        let tasks = snapshot(session, turn).task_activities();
        assert_eq!(tasks[0].status, expected);
        assert_eq!(tasks[0].ongoing, ongoing);
    }

    #[test]
    fn notifications_override_stale_history_without_a_visible_list_row() {
        let initial = snapshot("idle", "completed");
        let id = initial.task_activities()[0].session.clone();
        let event = |active| {
            Event::Notification(Notification::Activity {
                session: id.clone(),
                active,
                finished: !active,
            })
        };
        let running = reduce(&initial, event(true)).0;
        assert_eq!(running.task_activities()[0].status, "running");
        let finished = reduce(&running, event(false)).0;
        assert!(!finished.task_activities()[0].ongoing);
        assert!(finished.threads.is_none());
    }

    #[test]
    fn awaiting_requests_are_distinct_from_submitted_answers() {
        let mut state = snapshot("running", "running");
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.conversations)
                .values_mut()
                .next()
                .unwrap(),
        );
        let request = serde_json::from_value(json!({"id":"request", "target":"session", "delivery":"awaiting", "body":{"question":{"questions":[]}}})).unwrap();
        thread.requests.insert("request".into(), Arc::new(request));
        assert_eq!(state.task_activities()[0].status, "waiting");
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.conversations)
                .values_mut()
                .next()
                .unwrap(),
        );
        Arc::make_mut(thread.requests.values_mut().next().unwrap()).delivery =
            crate::session::RequestDelivery::Sent;
        assert_eq!(state.task_activities()[0].status, "running");
    }

    #[test]
    fn inactive_notifications_do_not_reuse_results_from_unsubscribed_history() {
        let mut state = snapshot("idle", "failed");
        state.subscriptions = Arc::default();
        let id = state.task_activities()[0].session.clone();
        Arc::make_mut(&mut state.activity).active.insert(id, false);
        assert_eq!(state.task_activities()[0].status, "unknown");
        let disconnected = reduce(&state, Event::Disconnected("offline".into())).0;
        assert_eq!(disconnected.task_activities()[0].status, "unknown");
    }
    #[test]
    fn provider_unavailability_cannot_certify_a_cached_terminal_result() {
        let mut state = snapshot("unavailable", "failed");
        let id = state.task_activities()[0].session.clone();
        Arc::make_mut(&mut state.activity).active.insert(id, false);
        assert_eq!(state.task_activities()[0].status, "unknown");
    }
    #[test]
    fn fresh_list_state_takes_priority_over_unsubscribed_status_and_old_requests() {
        use crate::state::operations::{ListSessions, Operation};
        let mut state = snapshot("idle", "failed");
        state.subscriptions = Arc::default();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.conversations)
                .values_mut()
                .next()
                .unwrap(),
        );
        let request = serde_json::from_value(json!({"id":"request","target":"session","delivery":"awaiting","body":{"question":{"questions":[]}}})).unwrap();
        thread.requests.insert("request".into(), Arc::new(request));
        for (status, expected) in [("running", "running"), ("idle", "finished")] {
            ListSessions::new(Default::default()).apply(
                &mut state,
                serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"task"}, "status":status}],
                "projects":[],  "hasMore":false, }))
                .unwrap(),
            );
            assert_eq!(state.task_activities()[0].status, expected);
        }
    }

    #[test]
    fn fresh_list_status_survives_unknown_history_and_keeps_providers_distinct() {
        use crate::state::operations::{ListSessions, Operation};
        let mut state = snapshot("unknown", "unknown");
        ListSessions::new(Default::default()).apply(&mut state, serde_json::from_value(json!({
            "data":[
                {"id":{"provider":"codex","id":"task"}, "status":"running", "name":""},
                {"id":{"provider":"claude","id":"task"}, "status":"running", "preview":"Claude task"}
            ], "projects":[],  "hasMore":false, })).unwrap());
        let tasks = state.task_activities();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|task| task.ongoing));
        assert_ne!(tasks[0].session, tasks[1].session);
        state.threads = None;
        assert_eq!(state.task_activities()[0].status, "unknown");
    }

    #[test]
    fn stale_list_rows_cannot_end_or_start_an_activity() {
        use crate::state::operations::{ListSessions, Operation};
        let mut state = Snapshot::default();
        ListSessions::new(Default::default()).apply(
            &mut state,
            serde_json::from_value(json!({
            "data":[{"id":{"provider":"codex","id":"task"}, "status":"running", "listStale":true}],
            "projects":[],  "hasMore":false, }))
            .unwrap(),
        );
        assert_eq!(state.task_activities()[0].status, "unknown");
        assert!(!state.task_activities()[0].ongoing);
        let id = state.task_activities()[0].session.clone();
        Arc::make_mut(&mut state.activity).active.insert(id, true);
        assert_eq!(state.task_activities()[0].status, "running");
    }
}
