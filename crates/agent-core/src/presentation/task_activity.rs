//! System surfaces render authoritative Host task state, independently of list pagination.
use crate::state::Snapshot;
use agent_protocol::live_activity::{
    TASK_ACTIVITY_DISMISS_SECONDS, TaskActivityDisplay, TaskActivityState,
};
use std::sync::Arc;

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn task_activity_dismiss_seconds() -> u32 {
    TASK_ACTIVITY_DISMISS_SECONDS
}

impl Snapshot {
    pub(crate) fn accept_task_activity(&mut self, state: TaskActivityState) {
        if self
            .task_activity
            .as_ref()
            .is_none_or(|current| current.revision < state.revision)
        {
            for (id, status) in &state.statuses {
                let active = match status {
                    crate::models::SessionStatus::Running => Some(true),
                    crate::models::SessionStatus::Idle => Some(false),
                    _ => None,
                };
                if self.activity.active.get(id).copied() != active {
                    let activity = Arc::make_mut(&mut self.activity);
                    if let Some(active) = active {
                        activity.active.insert(id.clone(), active);
                    } else {
                        activity.active.remove(id);
                    }
                }
            }
            let statuses: std::collections::BTreeMap<_, _> = state
                .statuses
                .iter()
                .map(|(id, status)| (id, status))
                .collect();
            let changed = |page: &crate::models::ThreadList| {
                page.data.iter().any(|thread| {
                    thread
                        .id
                        .as_ref()
                        .and_then(|id| statuses.get(id))
                        .is_some_and(|status| thread.status != **status)
                })
            };
            let update = |page: &mut Arc<crate::models::ThreadList>| {
                if changed(page) {
                    for thread in &mut Arc::make_mut(page).data {
                        if let Some(status) = thread.id.as_ref().and_then(|id| statuses.get(id)) {
                            thread.status = **status;
                        }
                    }
                }
            };
            if let Some(page) = &mut self.threads {
                update(page);
            }
            if self.project_threads.values().any(|page| changed(page)) {
                for page in Arc::make_mut(&mut self.project_threads).values_mut() {
                    update(page);
                }
            }
            self.task_activity = Some(Arc::new(state));
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn task_activity_display(&self) -> Option<TaskActivityDisplay> {
        self.task_activity
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
    use agent_protocol::live_activity::TaskActivitySummary;

    #[test]
    fn status_sync_updates_received_closed_pages_without_reading_titles() {
        let id =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "task".into())
                .unwrap();
        let mut state = Snapshot::default();
        let page = crate::models::ThreadList {
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
        state.project_threads = Arc::new([("closed".into(), Arc::new(page))].into());
        state.accept_task_activity(TaskActivityState {
            revision: 2,
            display: TaskActivitySummary::default().display(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Idle)],
        });
        assert!(!state.activity.active[&id]);
        assert_eq!(
            state.project_threads["closed"].data[0].status,
            crate::models::SessionStatus::Idle
        );
        assert_eq!(state.project_threads["closed"].limit, 15);
        let received = state.project_threads.clone();
        let activity = state.activity.clone();
        state.accept_task_activity(TaskActivityState {
            revision: 3,
            display: TaskActivitySummary::default().display(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Idle)],
        });
        assert!(Arc::ptr_eq(&state.project_threads, &received));
        assert!(Arc::ptr_eq(&state.activity, &activity));
        state.accept_task_activity(TaskActivityState {
            revision: 1,
            display: TaskActivitySummary {
                running: 1,
                ..Default::default()
            }
            .display(),
            statuses: vec![(id.clone(), crate::models::SessionStatus::Running)],
        });
        assert!(!state.activity.active[&id]);
        assert!(state.expanded_projects.is_empty() && state.operations.is_empty());
    }
    #[test]
    fn delayed_read_cannot_replace_a_newer_completion_or_depend_on_visible_rows() {
        let running = TaskActivityState {
            statuses: Vec::new(),
            revision: 1,
            display: TaskActivitySummary {
                running: 3,
                ..Default::default()
            }
            .display(),
        };
        let completed = TaskActivityState {
            statuses: Vec::new(),
            revision: 2,
            display: TaskActivitySummary::default().display(),
        };
        let mut state = Snapshot::default();
        ReadTaskActivity {}.apply(&mut state, running.clone());
        assert_eq!(state.task_activity_display(), Some(running.display.clone()));
        state = reduce(
            &state,
            Event::Notification(Notification::TaskActivity {
                state: completed.clone(),
            }),
        )
        .0;
        ReadTaskActivity {}.apply(&mut state, running);
        assert_eq!(state.task_activity_display(), Some(completed.display));
        assert!(state.threads.is_none());
        assert!(state.conversations.is_empty());
        let disconnected = reduce(&state, Event::Disconnected("offline".into())).0;
        assert_eq!(
            disconnected.task_activity_display(),
            state.task_activity_display()
        );
        let mut reconnected = reduce(&disconnected, Event::Connected).0;
        assert_eq!(reconnected.task_activity_display(), None);
        ReadTaskActivity {}.apply(
            &mut reconnected,
            TaskActivityState {
                statuses: Vec::new(),
                revision: 0,
                display: TaskActivitySummary::default().display(),
            },
        );
        assert!(!reconnected.task_activity_display().unwrap().ongoing);
    }
}
