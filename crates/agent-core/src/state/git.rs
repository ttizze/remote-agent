use agent_protocol::{vcs, workspace};
use std::collections::BTreeMap;

/// Device-owned folding of the Host's Git status and action streams.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GitState {
    /// The merged status, keyed by the canonical checkout path requested by a
    /// client.
    pub status: BTreeMap<String, workspace::VcsStatus>,
    /// The latest event of each subscribed checkout, retained for a view that
    /// opens after the stream has already produced its first snapshot.
    pub status_events: BTreeMap<String, vcs::VcsStatusStreamEvent>,
    /// The latest progress event of each running action.
    pub actions: BTreeMap<String, vcs::ActionProgressEvent>,
}

impl GitState {
    pub fn apply_status(&mut self, cwd: String, event: vcs::VcsStatusStreamEvent) {
        let current = self.status.get(&cwd).cloned();
        let next = match &event {
            vcs::VcsStatusStreamEvent::Snapshot { local, remote } => {
                workspace::VcsStatus::merge(local.clone(), remote.clone())
            }
            vcs::VcsStatusStreamEvent::LocalUpdated { local } => workspace::VcsStatus::merge(
                local.clone(),
                current.as_ref().map(workspace::VcsStatus::remote),
            ),
            vcs::VcsStatusStreamEvent::RemoteUpdated { remote } => workspace::VcsStatus::merge(
                current
                    .as_ref()
                    .map(workspace::VcsStatus::local)
                    .unwrap_or_else(vcs::VcsStatusLocal::not_repository),
                remote.clone(),
            ),
        };
        self.status.insert(cwd.clone(), next);
        self.status_events.insert(cwd, event);
    }

    pub fn apply_action(&mut self, event: vcs::ActionProgressEvent) {
        let action_id = event.action_id.clone();
        self.actions.insert(action_id, event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::vcs::{VcsStatusLocal, VcsStatusRemote};
    use agent_protocol::workspace::{BranchChanges, FileChangeTotals, WorkingTreeChanges};

    fn local(branch: &str, changed: bool) -> VcsStatusLocal {
        VcsStatusLocal {
            is_repo: true,
            source_control_provider: None,
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some(branch.into()),
            has_working_tree_changes: changed,
            working_tree: WorkingTreeChanges {
                files: changed
                    .then_some(FileChangeTotals {
                        path: "file".into(),
                        insertions: 1,
                        deletions: 0,
                    })
                    .into_iter()
                    .collect(),
                insertions: if changed { 1 } else { 0 },
                deletions: 0,
            },
            branch_changes: Some(BranchChanges {
                base_ref: Some("origin/main".into()),
                insertions: 0,
                deletions: 0,
            }),
        }
    }

    fn remote(ahead: u64, behind: u64) -> VcsStatusRemote {
        VcsStatusRemote {
            has_upstream: true,
            ahead_count: ahead,
            behind_count: behind,
            ahead_of_default_count: Some(ahead),
            pr: None,
        }
    }

    #[test]
    fn local_and_remote_updates_preserve_the_other_half() {
        let mut state = GitState::default();
        state.apply_status(
            "/repo".into(),
            vcs::VcsStatusStreamEvent::Snapshot {
                local: local("feature/demo", true),
                remote: Some(remote(2, 1)),
            },
        );
        state.apply_status(
            "/repo".into(),
            vcs::VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote(3, 0)),
            },
        );
        assert_eq!(
            state.status["/repo"].ref_name.as_deref(),
            Some("feature/demo")
        );
        assert!(state.status["/repo"].has_working_tree_changes);
        assert_eq!(state.status["/repo"].ahead_count, 3);
        assert_eq!(state.status["/repo"].behind_count, 0);
    }

    #[test]
    fn a_remote_first_event_is_available_until_local_status_arrives() {
        let mut state = GitState::default();
        state.apply_status(
            "/repo".into(),
            vcs::VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote(1, 0)),
            },
        );
        assert!(!state.status["/repo"].is_repo);
        assert_eq!(state.status["/repo"].ahead_count, 1);
        state.apply_status(
            "/repo".into(),
            vcs::VcsStatusStreamEvent::LocalUpdated {
                local: local("feature/demo", false),
            },
        );
        assert!(state.status["/repo"].is_repo);
        assert_eq!(
            state.status["/repo"].ref_name.as_deref(),
            Some("feature/demo")
        );
    }
}
