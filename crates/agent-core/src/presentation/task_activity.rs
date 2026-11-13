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
        models::{SessionStatus, Thread, ThreadList},
        session::{ProviderKind, SessionRef},
        state::{Event, Intent, reduce},
    };
    #[test]
    fn system_surface_counts_the_whole_list_independently_of_pages_and_retains_its_offline_display()
    {
        let mut state = Snapshot::default();
        let data = (0..18)
            .map(|index| Thread {
                id: Some(SessionRef::new(ProviderKind::Codex, index.to_string()).unwrap()),
                status: SessionStatus::Running,
                ..Default::default()
            })
            .collect();
        crate::state::list_sync::seed(
            &mut state,
            ThreadList {
                data,
                ..Default::default()
            },
        );
        let display = state.task_activity_display().unwrap();
        assert_eq!(display.current.total, 18);
        assert_eq!(state.thread_list().unwrap().threads.len(), 5);
        let (expanded, effects) = reduce(
            &state,
            "",
            Event::Intent(Intent::ExpandThreadList { project_id: None }),
        );
        assert!(effects.is_empty());
        assert_eq!(expanded.thread_list().unwrap().threads.len(), 15);
        assert_eq!(expanded.task_activity_display(), Some(display.clone()));
        let (offline, _) = reduce(&expanded, "", Event::Disconnected);
        assert_eq!(offline.task_activity_display(), Some(display));
        let (reconnecting, _) = reduce(&offline, "", Event::Connected);
        assert!(reconnecting.task_activity_display().is_none());
    }
}
