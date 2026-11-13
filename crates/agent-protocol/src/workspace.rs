//! Workspace selection and observed preparation, independent of provider admission.
use serde::{Deserialize, Serialize};

/// Identity of a checkout owned by one preparation, retained by its cleanup job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedWorktree {
    pub path: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WorkspaceStrategy {
    Root,
    ExistingWorktree {
        path: String,
    },
    Worktree {
        base_ref: String,
        start_from_origin: bool,
    },
}
impl WorkspaceStrategy {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Worktree { base_ref, .. }
                if base_ref.len() > 4096
                    || base_ref.starts_with('-')
                    || base_ref.contains(['\0', '\r', '\n']) =>
            {
                Err("invalid worktree base")
            }
            Self::ExistingWorktree { path }
                if path.len() > 4096 || !crate::paths::is_absolute_host_path(path) =>
            {
                Err("existing worktree requires an absolute Host path")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOptions {
    pub is_repository: bool,
    pub default_worktree: bool,
    pub default_base: String,
    pub refs: Vec<String>,
    pub worktrees: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScript {
    pub name: String,
    pub command: String,
    pub asynchronous: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupStageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupStageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SetupPhase {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStage {
    pub id: SetupStageId,
    pub status: SetupStageStatus,
    pub started_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    pub percent: Option<u32>,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSetup {
    pub phase: SetupPhase,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub worktree_path: Option<String>,
    pub script: Option<SetupScript>,
    pub terminal_handle: Option<String>,
    pub stages: Vec<SetupStage>,
    pub error: Option<String>,
    pub can_cancel: bool,
}

impl WorkspaceSetup {
    pub fn new(base_ref: Option<String>, at: u64) -> Self {
        use SetupStageId::*;
        Self {
            phase: SetupPhase::Running,
            started_at_ms: at,
            ended_at_ms: None,
            branch: None,
            base_ref,
            worktree_path: None,
            script: None,
            terminal_handle: None,
            error: None,
            can_cancel: true,
            stages: [Fetch, Checkout, SetupScript, Agent]
                .into_iter()
                .map(|id| SetupStage {
                    id,
                    status: SetupStageStatus::Pending,
                    started_at_ms: None,
                    ended_at_ms: None,
                    percent: None,
                    detail: None,
                    exit_code: None,
                    tail: Vec::new(),
                })
                .collect(),
        }
    }

    pub fn stage(&mut self, id: SetupStageId, status: SetupStageStatus, at: u64) {
        if id == SetupStageId::Submodules && !self.stages.iter().any(|stage| stage.id == id) {
            let index = self
                .stages
                .iter()
                .position(|stage| stage.id == SetupStageId::SetupScript)
                .unwrap_or(self.stages.len());
            self.stages.insert(
                index,
                SetupStage {
                    id,
                    status: SetupStageStatus::Pending,
                    started_at_ms: None,
                    ended_at_ms: None,
                    percent: None,
                    detail: None,
                    exit_code: None,
                    tail: Vec::new(),
                },
            );
        }
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        stage.status = status;
        stage.started_at_ms.get_or_insert(at);
        stage.ended_at_ms = (!matches!(
            status,
            SetupStageStatus::Running | SetupStageStatus::Pending
        ))
        .then_some(at);
    }

    pub fn append_output(&mut self, id: SetupStageId, line: &str) {
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        stage.tail.push(line.chars().take(400).collect());
        if stage.tail.len() > 4 {
            stage.tail.remove(0);
        }
    }

    pub fn finish(&mut self, phase: SetupPhase, error: Option<String>, at: u64) {
        self.phase = phase;
        self.ended_at_ms = Some(at);
        self.can_cancel = false;
        self.error = error.map(|text| text.chars().take(4000).collect());
        for stage in &mut self.stages {
            if stage.status == SetupStageStatus::Running {
                stage.status = match phase {
                    SetupPhase::Done => SetupStageStatus::Done,
                    SetupPhase::Cancelled => SetupStageStatus::Skipped,
                    _ => SetupStageStatus::Failed,
                };
                stage.ended_at_ms = Some(at);
            }
        }
    }

    pub fn visible_at(&self, at: u64) -> bool {
        self.ended_at_ms
            .is_none_or(|ended| at.saturating_sub(ended) < 30_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_marks_only_running_stages_and_expires_after_thirty_seconds() {
        let mut setup = WorkspaceSetup::new(Some("origin/main".into()), 10);
        assert!(setup.visible_at(100_000));
        setup.stage(SetupStageId::Fetch, SetupStageStatus::Skipped, 11);
        setup.stage(SetupStageId::Checkout, SetupStageStatus::Running, 12);
        setup.finish(SetupPhase::Failed, Some("checkout failed".into()), 20);
        assert_eq!(setup.stages[0].status, SetupStageStatus::Skipped);
        assert_eq!(setup.stages[1].status, SetupStageStatus::Failed);
        assert_eq!(setup.stages[1].ended_at_ms, Some(20));
        assert_eq!(setup.stages[2].status, SetupStageStatus::Pending);
        assert!(!setup.can_cancel);
        assert!(setup.visible_at(0));
        assert!(setup.visible_at(30_019));
        assert!(!setup.visible_at(30_020));
    }

    #[test]
    fn live_output_is_bounded_without_splitting_unicode() {
        let mut setup = WorkspaceSetup::new(None, 0);
        for index in 0..6 {
            setup.append_output(
                SetupStageId::SetupScript,
                &format!("{index}{}", "あ".repeat(500)),
            );
        }
        let tail = &setup.stages[2].tail;
        assert_eq!(tail.len(), 4);
        assert!(tail[0].starts_with('2'));
        assert!(tail.iter().all(|line| line.chars().count() == 400));
    }

    #[test]
    fn cancellation_freezes_running_stages_and_submodules_are_inserted_once_in_execution_order() {
        let mut setup = WorkspaceSetup::new(None, 10);
        setup.stage(SetupStageId::Checkout, SetupStageStatus::Done, 11);
        setup.stage(SetupStageId::Submodules, SetupStageStatus::Running, 12);
        setup.stage(SetupStageId::Submodules, SetupStageStatus::Running, 13);
        assert_eq!(
            setup
                .stages
                .iter()
                .map(|stage| stage.id)
                .collect::<Vec<_>>(),
            vec![
                SetupStageId::Fetch,
                SetupStageId::Checkout,
                SetupStageId::Submodules,
                SetupStageId::SetupScript,
                SetupStageId::Agent
            ]
        );
        setup.finish(SetupPhase::Cancelled, None, 14);
        assert_eq!(setup.stages[2].started_at_ms, Some(12));
        assert_eq!(setup.stages[2].ended_at_ms, Some(14));
        assert_eq!(setup.stages[2].status, SetupStageStatus::Skipped);
        assert_eq!(setup.stages[1].status, SetupStageStatus::Done);
    }

    #[test]
    fn workspace_selection_accepts_refs_and_absolute_host_paths_but_rejects_option_and_path_injection()
     {
        for base in [
            "",
            "origin/main",
            "feature/日本語",
            "main~2",
            "release-v1",
            &"a".repeat(4096),
        ] {
            assert!(
                WorkspaceStrategy::Worktree {
                    base_ref: base.into(),
                    start_from_origin: true
                }
                .validate()
                .is_ok()
            );
        }
        for base in [
            "--detach",
            "main\nother",
            "main\r",
            "main\0",
            &"a".repeat(4097),
        ] {
            assert!(
                WorkspaceStrategy::Worktree {
                    base_ref: base.into(),
                    start_from_origin: false
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            WorkspaceStrategy::ExistingWorktree {
                path: "../other".into()
            }
            .validate()
            .is_err()
        );
        for path in ["/project/worktree", "C:\\project\\worktree"] {
            assert!(
                WorkspaceStrategy::ExistingWorktree { path: path.into() }
                    .validate()
                    .is_ok()
            );
        }
        assert!(WorkspaceStrategy::Root.validate().is_ok());
    }
}
