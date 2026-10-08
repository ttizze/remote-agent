//! Compact task state for system surfaces. Never expose message or command bodies.
use crate::models::{task_active, task_title};
use crate::{models::SessionStatus, session::SessionRef, state::Snapshot};
use agent_protocol::live_activity::task_phase;
use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TaskActivity {
    pub session: SessionRef,
    pub title: String,
    pub status: String,
    pub status_label: String,
    pub ongoing: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    /// Include observed sessions even when search or pagination hides their list row.
    pub fn task_activities(&self) -> Vec<TaskActivity> {
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
                let (status, status_label, ongoing) = task_phase(
                    known,
                    task_active(observed, session_status),
                    waiting && observed != Some(false),
                    latest.map(|turn| turn.status),
                );
                let title_source = summary.or(conversation);
                TaskActivity {
                    session: id.clone(),
                    title: crate::models::compact_title(task_title(
                        title_source.and_then(|thread| thread.name.as_deref()),
                        title_source.and_then(|thread| thread.preview.as_deref()),
                    )),
                    status: status.into(),
                    status_label: status_label.into(),
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
        assert_eq!(state.task_activities()[0].status_label, "確認待ち");
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
        assert_eq!(state.task_activities()[0].status_label, "更新待ち");
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
            ListSessions::new(Default::default()).apply(&mut state,serde_json::from_value(json!({
                "data":[{"id":{"provider":"codex","id":"task"}, "status":status}],
                "projects":[], "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false
            })).unwrap());
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
            ], "projects":[], "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false
        })).unwrap());
        let tasks = state.task_activities();
        assert_eq!(tasks.len(), 2);
        assert!(tasks.iter().all(|task| task.ongoing));
        assert_ne!(tasks[0].session, tasks[1].session);
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.session.provider == crate::session::ProviderKind::Codex)
                .unwrap()
                .title,
            "無題のタスク"
        );
        assert_eq!(
            tasks
                .iter()
                .find(|task| task.session.provider == crate::session::ProviderKind::Claude)
                .unwrap()
                .title,
            "Claude task"
        );
        state.threads = None;
        assert_eq!(state.task_activities()[0].status, "unknown");
    }

    #[test]
    fn stale_list_rows_cannot_end_or_start_an_activity() {
        use crate::state::operations::{ListSessions, Operation};
        let mut state = Snapshot::default();
        ListSessions::new(Default::default()).apply(&mut state, serde_json::from_value(json!({
            "data":[{"id":{"provider":"codex","id":"task"}, "status":"running", "listStale":true}],
            "projects":[], "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false
        })).unwrap());
        assert_eq!(state.task_activities()[0].status, "unknown");
        assert!(!state.task_activities()[0].ongoing);
        let id = state.task_activities()[0].session.clone();
        Arc::make_mut(&mut state.activity).active.insert(id, true);
        assert_eq!(state.task_activities()[0].status, "running");
    }

    proptest::proptest! {
        #[test]
        fn titles_are_bounded_without_exposing_later_lines(title in ".{0,400}") {
            let mut state = snapshot("running", "running");
            let thread = Arc::make_mut(Arc::make_mut(&mut state.conversations).values_mut().next().unwrap());
            thread.name = Some(format!("{title}\nprivate body"));
            let tasks = state.task_activities();
            proptest::prop_assert!(tasks[0].title.chars().count() <= 121);
            proptest::prop_assert!(!tasks[0].title.contains('\n'));
            proptest::prop_assert!(!tasks[0].title.contains("private body"));
        }
    }
}
