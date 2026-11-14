use super::*;
pub use agent_protocol::project_clone::{ChangeProjectClone, CloneProject, ReadProjectClones};
use agent_protocol::project_clone::{ProjectClonePhase, ProjectClones};

impl Operation for CloneProject {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectClone {
            id: Some(self.id.clone()),
        })
    }
    fn navigation_bound(&self) -> bool {
        false
    }
    fn prepare(&mut self, _snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        self.validate()
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        snapshot.accept_project_clones(output)
    }
}
impl Operation for ChangeProjectClone {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectClone {
            id: Some(self.id.clone()),
        })
    }
    fn navigation_bound(&self) -> bool {
        false
    }
    fn prepare(&mut self, _snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        agent_protocol::project_clone::validate_id(&self.id)
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        snapshot.accept_project_clones(output)
    }
}
impl Operation for ReadProjectClones {
    rpc_operation!();
    const BACKGROUND: bool = true;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectClone { id: None })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestHostState(OperationKey::ProjectClone { id: None })
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        snapshot.accept_project_clones(output)
    }
}

impl Snapshot {
    pub(crate) fn accept_project_clones(&mut self, state: ProjectClones) -> Vec<Effect> {
        if self.lists.project_clones.as_ref().is_some_and(|previous| {
            previous.epoch == state.epoch && previous.revision >= state.revision
        }) {
            return Vec::new();
        }
        let completed: Vec<_> = state
            .clones
            .iter()
            .filter(|entry| {
                entry.phase == ProjectClonePhase::Completed
                    && !self
                        .lists
                        .project_clones
                        .iter()
                        .flat_map(|previous| &previous.clones)
                        .any(|previous| {
                            previous.request.id == entry.request.id
                                && previous.phase == ProjectClonePhase::Completed
                        })
            })
            .map(|entry| entry.request.destination.clone())
            .collect();
        self.lists.project_clones = Some(Arc::new(state));
        if completed.is_empty() {
            return Vec::new();
        }
        let mut effects = vec![Effect::execute(ListSessions::new(
            (*self.lists.list_query).clone(),
        ))];
        let panes: Vec<_> = self
            .panes
            .0
            .iter()
            .filter(|(_, pane)| completed.contains(&pane.navigation.cwd))
            .map(|(id, _)| id.clone())
            .collect();
        for id in panes {
            effects.extend(review_workspace(self, &id).map(|effect| effect.in_pane(&id)));
        }
        effects
    }
    pub(crate) fn project_clone_block_reason(&self, cwd: &str) -> Option<String> {
        let entry = self
            .lists
            .project_clones
            .as_ref()?
            .clones
            .iter()
            .find(|entry| entry.request.destination == cwd)?;
        match entry.phase {
            ProjectClonePhase::Completed => None,
            phase if phase.active() => Some("Cloning repository".into()),
            _ => Some("Repository not cloned".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::project_clone::{ProjectCloneAction, ProjectCloneState};
    #[test]
    fn unavailable_checkout_blocks_submission_without_clearing_the_draft() {
        let (mut snapshot, _) = crate::state::reduce(
            &Snapshot::default(),
            "",
            crate::state::Event::Intent(Intent::NewChat {
                cwd: "/repo".into(),
            }),
        );
        snapshot.connection.connected = true;
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        let draft = Arc::new(Draft {
            text: "keep this input".into(),
            model: Some(crate::models::ModelRef {
                provider: crate::session::ProviderKind::Codex,
                id: "fixture".into(),
            }),
            ..Default::default()
        });
        Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), draft.clone());
        snapshot.accept_project_clones(catalog("host", 1, ProjectClonePhase::Failed));
        let (blocked, effects) = crate::state::reduce_intent(
            &snapshot,
            "",
            Intent::Submit {
                thread_id: None,
                client_user_message_id: "input".into(),
                alternate: false,
            },
        );
        assert!(effects.is_empty());
        assert_eq!(blocked.input.drafts[&key], draft);
        assert!(blocked.sessions.pending_submissions.is_empty());
        assert!(blocked.project_clone_views()[0].can_retry);
        assert!(!blocked.project_clone_views()[0].can_cancel);
        let mut offline = blocked;
        offline.connection.connected = false;
        assert!(!offline.project_clone_views()[0].can_retry);
        assert!(offline.project_clone_views()[0].blocks_send);
        let (reconnected, _) = crate::state::reduce_event(&offline, crate::state::Event::Connected);
        assert!(
            reconnected.project_clone_views()[0].blocks_send,
            "reconnect retains the block until the authoritative Host read arrives"
        );
    }
    fn catalog(epoch: &str, revision: u64, phase: ProjectClonePhase) -> ProjectClones {
        ProjectClones {
            epoch: epoch.into(),
            revision,
            clones: vec![ProjectCloneState {
                request: CloneProject {
                    id: "00000000-0000-0000-0000-000000000001".into(),
                    repository: "https://host/repo".into(),
                    destination: "/repo".into(),
                },
                phase,
                progress: None,
                error: None,
            }],
        }
    }
    #[test]
    fn notification_wins_over_a_delayed_read_and_completion_refreshes_projects_once() {
        let mut state = Snapshot::default();
        assert!(
            state
                .accept_project_clones(catalog("first", 4, ProjectClonePhase::Fetching))
                .is_empty()
        );
        assert_eq!(
            state.project_clone_block_reason("/repo").as_deref(),
            Some("Cloning repository")
        );
        assert!(
            state
                .accept_project_clones(catalog("first", 3, ProjectClonePhase::Queued))
                .is_empty()
        );
        assert_eq!(state.lists.project_clones.as_ref().unwrap().revision, 4);
        assert_eq!(
            state
                .accept_project_clones(catalog("first", 5, ProjectClonePhase::Completed))
                .len(),
            1
        );
        assert!(
            state
                .accept_project_clones(catalog("first", 5, ProjectClonePhase::Completed))
                .is_empty()
        );
        assert_eq!(state.project_clone_block_reason("/repo"), None);
        assert!(
            state
                .accept_project_clones(catalog("restart", 0, ProjectClonePhase::Failed))
                .is_empty()
        );
        assert_eq!(
            state.project_clone_block_reason("/repo").as_deref(),
            Some("Repository not cloned")
        );
        assert_eq!(state.project_clone_block_reason("/other"), None);
        let request = ChangeProjectClone {
            id: "invalid".into(),
            action: ProjectCloneAction::Retry,
        };
        assert!(request.clone().prepare(&mut state, "").is_err());
    }
}
