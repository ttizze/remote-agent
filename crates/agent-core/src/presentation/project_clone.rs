//! Native adapters render these shared controls without duplicating phase rules.
use crate::state::Snapshot;
use agent_protocol::project_clone::*;

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectCloneView {
    pub state: ProjectCloneState,
    pub title: String,
    pub status: String,
    pub progress_percent: Option<u8>,
    pub error: Option<super::error::ErrorText>,
    pub can_cancel: bool,
    pub can_retry: bool,
    pub can_dismiss: bool,
    pub can_open: bool,
    pub blocks_send: bool,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn project_clone_available(&self) -> bool {
        self.connection.connected
            && !self.operations.iter().any(|(key, operation)| {
                matches!(
                    key,
                    crate::state::operations::OperationKey::ProjectClone { id: Some(_) }
                ) && operation.phase == crate::state::operations::OperationPhase::Running
            })
    }
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
                    title: crate::localization::localized_ui_format(
                        self.catalog.ui_language,
                        match state.phase {
                            ProjectClonePhase::Completed => "{0}の取得が完了しました",
                            ProjectClonePhase::Cancelled => "{0}の取得を取り消しました",
                            ProjectClonePhase::Failed => "{0}を取得できませんでした",
                            _ => "{0}を取得中",
                        }
                        .into(),
                        vec![
                            crate::presentation::markdown::file_basename(
                                &state.request.destination,
                            )
                            .into(),
                        ],
                    ),
                    status: self
                        .catalog
                        .ui_language
                        .text(match state.phase {
                            ProjectClonePhase::Queued => "待機中",
                            ProjectClonePhase::Fetching => "受信中",
                            ProjectClonePhase::Resolving => "受信したデータを解析中",
                            ProjectClonePhase::Checkout => "ファイルを展開中",
                            ProjectClonePhase::Registering => "プロジェクトに追加中",
                            ProjectClonePhase::Completed => "完了",
                            ProjectClonePhase::Cancelling => "取り消し中",
                            ProjectClonePhase::Cancelled => "再試行してリポジトリを取得できます。",
                            ProjectClonePhase::Failed => "リポジトリの取得に失敗しました",
                        })
                        .into(),
                    progress_percent: state.progress.as_ref().and_then(progress_percent),
                    error: self
                        .operation_error(crate::state::operations::OperationKey::ProjectClone {
                            id: Some(state.request.id.clone()),
                        })
                        .or_else(|| {
                            state.error.as_ref().map(|message| {
                                super::error::error_text(self.catalog.ui_language, message)
                            })
                        }),
                    can_cancel: available && state.phase.can_cancel(),
                    can_retry: available && state.phase.can_retry(),
                    can_dismiss: available && !state.phase.active(),
                    can_open: available && state.phase == ProjectClonePhase::Completed,
                    blocks_send: state.phase != ProjectClonePhase::Completed,
                }
            })
            .collect()
    }
}

fn progress_percent(progress: &CloneProgress) -> Option<u8> {
    (progress.total > 0).then(|| {
        (u128::from(progress.current.min(progress.total)) * 100 / u128::from(progress.total)) as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        Event, Intent,
        operations::{OperationKey, OperationPhase, OperationState},
        reduce,
    };
    use std::sync::Arc;

    fn state(phase: ProjectClonePhase) -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        snapshot.lists.project_clones = Some(Arc::new(ProjectClones {
            epoch: "host".into(),
            revision: 1,
            clones: vec![ProjectCloneState {
                request: CloneProject {
                    id: "00000000-0000-0000-0000-000000000001".into(),
                    repository: "https://example.test/owner/repo.git".into(),
                    destination: "/projects/repo".into(),
                },
                phase,
                progress: Some(CloneProgress {
                    current: 1,
                    total: 3,
                }),
                error: None,
            }],
        }));
        snapshot
    }

    #[test]
    fn download_controls_follow_the_commit_boundary_and_the_connection_owner() {
        use ProjectClonePhase::*;
        for (phase, cancel, retry, dismiss, open, blocked) in [
            (Queued, true, false, false, false, true),
            (Fetching, true, false, false, false, true),
            (Resolving, true, false, false, false, true),
            (Checkout, true, false, false, false, true),
            (Registering, false, false, false, false, true),
            (Cancelling, false, false, false, false, true),
            (Cancelled, false, true, true, false, true),
            (Failed, false, true, true, false, true),
            (Completed, false, false, true, true, false),
        ] {
            let mut snapshot = state(phase);
            let view = &snapshot.project_clone_views()[0];
            assert_eq!(
                (
                    view.can_cancel,
                    view.can_retry,
                    view.can_dismiss,
                    view.can_open,
                    view.blocks_send
                ),
                (cancel, retry, dismiss, open, blocked)
            );
            assert_eq!(view.progress_percent, Some(33));
            assert!(!view.title.is_empty() && !view.status.is_empty());
            snapshot.connection.connected = false;
            assert!(!snapshot.project_clone_available());
            let view = &snapshot.project_clone_views()[0];
            assert!(!view.can_cancel && !view.can_retry && !view.can_dismiss && !view.can_open);
            snapshot.connection.connected = true;
            assert!(snapshot.project_clone_available());
            Arc::make_mut(&mut snapshot.operations).insert(
                OperationKey::ProjectClone {
                    id: Some(view.state.request.id.clone()),
                },
                OperationState {
                    generation: 1,
                    phase: OperationPhase::Running,
                },
            );
            let view = &snapshot.project_clone_views()[0];
            assert!(!view.can_cancel && !view.can_retry && !view.can_dismiss && !view.can_open);
            assert!(!snapshot.project_clone_available());
        }
    }

    #[test]
    fn background_reads_do_not_block_new_downloads_and_operation_errors_remain_recoverable() {
        let mut snapshot = state(ProjectClonePhase::Failed);
        snapshot.catalog.ui_language = crate::localization::UiLanguage::English;
        Arc::make_mut(snapshot.lists.project_clones.as_mut().unwrap()).clones[0].error =
            Some("git could not fetch".into());
        Arc::make_mut(&mut snapshot.operations).insert(
            OperationKey::ProjectClone { id: None },
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        Arc::make_mut(&mut snapshot.operations).insert(
            OperationKey::SessionList,
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        assert!(snapshot.project_clone_available());
        let view = &snapshot.project_clone_views()[0];
        assert_eq!(view.title, "Could not download repo");
        assert!(
            view.error
                .as_ref()
                .unwrap()
                .to_string()
                .contains("git could not fetch")
        );
        Arc::make_mut(&mut snapshot.operations).insert(
            OperationKey::ProjectClone {
                id: Some(view.state.request.id.clone()),
            },
            OperationState {
                generation: 2,
                phase: OperationPhase::Failed {
                    message: "retry request failed".into(),
                },
            },
        );
        let view = &snapshot.project_clone_views()[0];
        assert!(view.can_retry && view.can_dismiss && snapshot.project_clone_available());
        assert!(
            view.error
                .as_ref()
                .unwrap()
                .to_string()
                .contains("retry request failed")
        );
    }

    #[test]
    fn download_progress_is_bounded_without_overflow_or_an_invented_unknown_total() {
        for (current, total, expected) in [
            (0, 0, None),
            (0, 9, Some(0)),
            (1, 3, Some(33)),
            (9, 9, Some(100)),
            (10, 9, Some(100)),
            (u64::MAX, u64::MAX, Some(100)),
        ] {
            assert_eq!(
                progress_percent(&CloneProgress { current, total }),
                expected
            );
        }
    }

    #[test]
    fn selected_composer_follows_authoritative_clone_updates_and_does_not_leak_between_destinations()
     {
        let snapshot = state(ProjectClonePhase::Fetching);
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::from(Intent::NewChat {
                cwd: "/projects/repo".into(),
            }),
        );
        let composer = crate::presentation::composer::ComposerPresentation::project(
            &snapshot,
            snapshot.panes.primary(),
            None,
        );
        assert!(composer.controls.project_clone.as_ref().unwrap().can_cancel);
        assert_eq!(
            composer.controls.unavailable.as_deref(),
            Some("リポジトリを取得中")
        );
        let mut completed = snapshot
            .lists
            .project_clones
            .as_ref()
            .unwrap()
            .as_ref()
            .clone();
        completed.revision += 1;
        completed.clones[0].phase = ProjectClonePhase::Completed;
        let (snapshot, _) = reduce(
            &snapshot,
            "",
            Event::Notification(crate::protocol::Notification::ProjectClones { state: completed }),
        );
        let composer = crate::presentation::composer::ComposerPresentation::project(
            &snapshot,
            snapshot.panes.primary(),
            Some(&composer),
        );
        assert!(composer.controls.project_clone.is_none());
        let (elsewhere, _) = reduce(
            &state(ProjectClonePhase::Failed),
            "",
            Event::from(Intent::NewChat {
                cwd: "/other".into(),
            }),
        );
        let composer = crate::presentation::composer::ComposerPresentation::project(
            &elsewhere,
            elsewhere.panes.primary(),
            None,
        );
        assert!(composer.controls.project_clone.is_none());
    }
}
