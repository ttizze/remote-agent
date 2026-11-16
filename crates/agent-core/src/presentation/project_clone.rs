//! Native adapters render these shared controls without duplicating phase rules.
use crate::state::Snapshot;
use agent_protocol::project_clone::*;

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectCloneView {
    pub state: ProjectCloneState,
    pub can_cancel: bool,
    pub can_retry: bool,
    pub can_dismiss: bool,
    pub blocks_send: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn project_clone_views(&self) -> Vec<ProjectCloneView> {
        self.lists
            .project_clones
            .iter()
            .flat_map(|catalog| &catalog.clones)
            .map(|state| {
                let available = self.connection.connected
                    && !self.operation_running(
                        crate::state::operations::OperationKey::ProjectClone {
                            id: Some(state.request.id.clone()),
                        },
                    );
                ProjectCloneView {
                    state: state.clone(),
                    can_cancel: available && state.phase.can_cancel(),
                    can_retry: available && state.phase.can_retry(),
                    can_dismiss: available && !state.phase.active(),
                    blocks_send: state.phase != ProjectClonePhase::Completed,
                }
            })
            .collect()
    }
}
