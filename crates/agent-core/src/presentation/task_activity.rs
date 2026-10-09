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
    fn delayed_read_cannot_replace_a_newer_completion_or_depend_on_visible_rows() {
        let running = TaskActivityState {
            revision: 1,
            display: TaskActivitySummary {
                running: 3,
                ..Default::default()
            }
            .display(),
        };
        let completed = TaskActivityState {
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
                revision: 0,
                display: TaskActivitySummary::default().display(),
            },
        );
        assert!(!reconnected.task_activity_display().unwrap().ongoing);
    }
}
