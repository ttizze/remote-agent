//! System surfaces render authoritative Host task state, independently of list pagination.
use crate::state::Snapshot;
use agent_protocol::live_activity::{TASK_ACTIVITY_DISMISS_SECONDS, TaskActivityDisplay};

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn task_activity_dismiss_seconds() -> u32 {
    TASK_ACTIVITY_DISMISS_SECONDS
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn task_activity_display(&self) -> Option<TaskActivityDisplay> {
        self.lists
            .task_activity
            .as_ref()
            .map(|state| state.display.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::Notification,
        state::{
            Event,
            operations::{Operation, ReadTaskActivity},
            reduce,
        },
    };
    use agent_protocol::live_activity::{TaskActivityState, TaskActivitySummary};
    use std::sync::Arc;

    #[test]
    fn status_sync_updates_received_closed_pages_without_reading_titles() {
        let id =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "task".into())
                .unwrap();
        let mut state = Snapshot::default();
        let page = crate::models::ThreadList {
            revision: 0,
            limit: 15,
            data: vec![crate::models::Thread {
                id: Some(id.clone()),
                status: crate::models::SessionStatus::Running,
                ..Default::default()
            }],
            projects: vec![],
            has_more: false,
            has_more_projects: false,
            project_pages: Default::default(),
            provider_errors: None,
        };
        state.lists.project_threads = Arc::new([("closed".into(), Arc::new(page))].into());
        let _ = state.accept_task_activity(TaskActivityState {
            attention: Vec::new(),
            revision: 2,
            waiting: Vec::new(),
            display: TaskActivitySummary::default().display(),
            background: Vec::new(),
            unread: Vec::new(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Idle)],
        });
        assert!(!state.lists.activity.active[&id]);
        assert_eq!(
            state.lists.project_threads["closed"].data[0].status,
            crate::models::SessionStatus::Idle
        );
        assert_eq!(state.lists.project_threads["closed"].limit, 15);
        let received = state.lists.project_threads.clone();
        let activity = state.lists.activity.clone();
        let _ = state.accept_task_activity(TaskActivityState {
            attention: Vec::new(),
            revision: 3,
            waiting: Vec::new(),
            display: TaskActivitySummary::default().display(),
            background: Vec::new(),
            unread: Vec::new(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Idle)],
        });
        assert!(Arc::ptr_eq(&state.lists.project_threads, &received));
        assert!(Arc::ptr_eq(&state.lists.activity, &activity));
        let _ = state.accept_task_activity(TaskActivityState {
            attention: Vec::new(),
            revision: 1,
            waiting: Vec::new(),
            display: TaskActivitySummary {
                running: 1,
                ..Default::default()
            }
            .display(),
            background: Vec::new(),
            unread: Vec::new(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Running)],
        });
        assert!(!state.lists.activity.active[&id]);
        assert!(state.lists.expanded_projects.is_empty() && state.operations.is_empty());
    }
    #[test]
    fn delayed_read_cannot_replace_a_newer_completion_or_depend_on_visible_rows() {
        let running = TaskActivityState {
            attention: Vec::new(),
            waiting: Vec::new(),
            background: Vec::new(),
            unread: Vec::new(),
            statuses: Vec::new(),
            revision: 1,
            display: TaskActivitySummary {
                running: 3,
                ..Default::default()
            }
            .display(),
        };
        let completed = TaskActivityState {
            attention: Vec::new(),
            waiting: Vec::new(),
            background: Vec::new(),
            unread: Vec::new(),
            statuses: Vec::new(),
            revision: 2,
            display: TaskActivitySummary::default().display(),
        };
        let mut state = Snapshot::default();
        ReadTaskActivity {}.apply(&mut state, "", running.clone());
        assert_eq!(state.task_activity_display(), Some(running.display.clone()));
        state = reduce(
            &state,
            "",
            Event::Notification(Notification::TaskActivity {
                state: completed.clone(),
            }),
        )
        .0;
        ReadTaskActivity {}.apply(&mut state, "", running);
        assert_eq!(state.task_activity_display(), Some(completed.display));
        assert!(state.lists.threads.is_none());
        assert!(state.sessions.conversations.is_empty());
        let disconnected = reduce(&state, "", Event::Disconnected).0;
        assert_eq!(
            disconnected.task_activity_display(),
            state.task_activity_display()
        );
        let mut reconnected = reduce(&disconnected, "", Event::Connected).0;
        assert_eq!(reconnected.task_activity_display(), None);
        ReadTaskActivity {}.apply(
            &mut reconnected,
            "",
            TaskActivityState {
                attention: Vec::new(),
                waiting: Vec::new(),
                background: Vec::new(),
                unread: Vec::new(),
                statuses: Vec::new(),
                revision: 0,
                display: TaskActivitySummary::default().display(),
            },
        );
        assert!(!reconnected.task_activity_display().unwrap().ongoing);
    }
}
