//! Workspace preparation is metadata of the canonical conversation header.
use super::*;
use agent_protocol::{
    conversation::records::WorkspacePreparation,
    workspace::{SetupPhase, SetupStageId, SetupStageStatus},
};

pub(in crate::host_rpc) enum SetupChange {
    Begin {
        input: ClientInputId,
        workspace: Box<WorkspacePreparation>,
    },
    Progress(crate::worktrees::Progress),
    Script {
        handle: String,
    },
    Output(Vec<String>),
    Stage {
        id: SetupStageId,
        status: SetupStageStatus,
        exit_code: Option<i32>,
    },
    Finish {
        phase: SetupPhase,
        error: Option<String>,
    },
    AgentStarted,
    Expire {
        ended_at: u64,
    },
}

impl Storage {
    pub(in crate::host_rpc) fn setup(
        &self,
        target: &SessionRef,
        generation: Uuid,
        update: SetupChange,
        now: i64,
    ) -> Result<Committed, String> {
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let key = thread_key(&transaction, target)?;
            let (mut header, counters) = rows::header(&transaction, key)?;
            let state = rows::load(&transaction, key, self.process)?;
            let changed = match update {
                SetupChange::Begin { input, workspace } => {
                    if state.inputs.get(&input).is_some_and(|input| matches!(input.state, decision::InputState::Preparing { action, .. } if action == generation)) {
                        header.details.workspace = Some(workspace); true
                    } else { false }
                }
                SetupChange::Expire { ended_at } => {
                    if let Some(workspace) = header.details.workspace.as_mut().filter(|workspace| workspace.generation == generation && workspace.visible && workspace.setup.as_ref().is_some_and(|setup| setup.ended_at_ms == Some(ended_at))) {
                        workspace.visible = false; true
                    } else { false }
                }
                update => header.details.workspace.as_mut().filter(|workspace| workspace.generation == generation)
                    .and_then(|workspace| workspace.setup.as_mut()).is_some_and(|setup| apply(setup, update, now.max(0) as u64)),
            };
            let mut result = Committed { state: Some(state), step: Step::default(), cursor: cursor(&transaction)?, rows: vec![], removed: vec![], reset: false };
            if changed {
                result.step.outcome = Outcome::Accepted;
                result.cursor = Self::advance(&transaction)?;
                transaction.execute("UPDATE threads SET seq=?1,body=?2 WHERE key=?3", params![integer(result.cursor.sequence)?, codec::header(&header, &counters)?, key]).map_err(failure)?;
                result.rows.push(Row { key: Key::Thread(target.clone()), version: result.cursor.sequence, position: Position::Unordered, content: Content::Full(Arc::new(Record::Thread(header))) });
            }
            transaction.commit().map_err(failure)?;
            Ok(result)
        })
    }
    pub(in crate::host_rpc) fn workspace_scripts(
        &self,
        root: &str,
    ) -> Result<Vec<agent_protocol::models::ProjectScript>, String> {
        self.with(|connection| {
            let body: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT body FROM workspace_scripts WHERE root=?1",
                    [root],
                    |row| row.get(0),
                )
                .optional()
                .map_err(failure)?;
            body.map(|body| protocol::decode(&body).map_err(failure))
                .transpose()
                .map(Option::unwrap_or_default)
        })
    }
    pub(in crate::host_rpc) fn save_workspace_scripts(
        &self,
        root: &str,
        scripts: &[agent_protocol::models::ProjectScript],
    ) -> Result<(), String> {
        self.with(|connection| {
            connection.execute("INSERT INTO workspace_scripts(root,body) VALUES (?1,?2) ON CONFLICT(root) DO UPDATE SET body=excluded.body", params![root, protocol::encode(scripts).map_err(failure)?]).map_err(failure)?;
            Ok(())
        })
    }
}

fn apply(
    setup: &mut agent_protocol::workspace::WorkspaceSetup,
    update: SetupChange,
    now: u64,
) -> bool {
    if setup.phase != SetupPhase::Running {
        return false;
    }
    match update {
        SetupChange::Progress(progress) => match progress {
            crate::worktrees::Progress::Stage(id, status) => setup.stage(id, status, now),
            crate::worktrees::Progress::Base(base) => setup.base_ref = Some(base),
            crate::worktrees::Progress::Checkout(percent) => {
                if let Some(stage) = setup
                    .stages
                    .iter_mut()
                    .find(|stage| stage.id == SetupStageId::Checkout)
                {
                    stage.percent = Some(percent);
                }
            }
            crate::worktrees::Progress::Created { path, branch } => {
                setup.worktree_path = Some(path);
                setup.branch = Some(branch);
            }
        },
        SetupChange::Script { handle } => {
            setup.terminal_handle = Some(handle);
            setup.stage(SetupStageId::SetupScript, SetupStageStatus::Running, now);
        }
        SetupChange::Output(lines) => {
            for line in lines {
                setup.append_output(SetupStageId::SetupScript, &line);
            }
        }
        SetupChange::Stage {
            id,
            status,
            exit_code,
        } => {
            setup.stage(id, status, now);
            if let Some(stage) = setup.stages.iter_mut().find(|stage| stage.id == id) {
                stage.exit_code = exit_code;
            }
        }
        SetupChange::Finish { phase, error } => setup.finish(phase, error, now),
        SetupChange::AgentStarted => {
            setup.can_cancel = false;
            setup.stage(SetupStageId::Agent, SetupStageStatus::Running, now);
        }
        SetupChange::Begin { .. } | SetupChange::Expire { .. } => return false,
    }
    if setup.phase == SetupPhase::Running
        && setup.stages.iter().all(|stage| {
            !matches!(
                stage.status,
                SetupStageStatus::Pending | SetupStageStatus::Running
            )
        })
    {
        setup.finish(SetupPhase::Done, None, now);
    }
    true
}

/// A failed or interrupted script re-enters setup automatically. Working locally is an
/// explicit disposition of an early, unbound workspace preparation.
pub(super) fn retry(
    saved: Option<&WorkspacePreparation>,
    input: Option<&decision::InputState>,
    cwd: Option<&str>,
    bindings_empty: bool,
    work_locally: bool,
) -> Result<Option<(WorkspacePreparation, String)>, &'static str> {
    let Some(saved) = saved else {
        return if work_locally {
            Err("workspace setup is unavailable")
        } else {
            Ok(None)
        };
    };
    if work_locally {
        if !bindings_empty
            || !matches!(
                input,
                Some(
                    decision::InputState::Failed(_)
                        | decision::InputState::Preparing {
                            stage: agent_protocol::session::PreparationStage::Workspace
                                | agent_protocol::session::PreparationStage::Worktree
                                | agent_protocol::session::PreparationStage::SetupScript,
                            ..
                        }
                )
            )
        {
            return Err("the agent has already started");
        }
        let mut workspace = saved.clone();
        workspace.strategy = Some(agent_protocol::workspace::WorkspaceStrategy::Root);
        return Ok(Some((workspace, saved.project_cwd.clone())));
    }
    let failed_script = saved.setup.as_ref().is_some_and(|setup| {
        setup.stages.iter().any(|stage| {
            stage.id == SetupStageId::SetupScript
                && (stage.status == SetupStageStatus::Failed
                    || setup.phase == SetupPhase::Cancelled
                        && stage.status == SetupStageStatus::Skipped
                        && stage.started_at_ms.is_some())
        })
    });
    Ok(
        (failed_script && matches!(input, Some(decision::InputState::Failed(_))))
            .then(|| (saved.clone(), cwd.unwrap_or(&saved.project_cwd).to_owned())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        session::PreparationStage,
        workspace::{WorkspaceSetup, WorkspaceStrategy},
    };

    fn saved() -> WorkspacePreparation {
        WorkspacePreparation {
            project_cwd: "/project".into(),
            strategy: Some(WorkspaceStrategy::Worktree {
                base_ref: "main".into(),
                start_from_origin: false,
            }),
            generation: Uuid::from_u128(1),
            setup: Some(WorkspaceSetup::new(Some("main".into()), 0)),
            visible: true,
        }
    }

    #[test]
    fn work_locally_requires_an_early_unbound_input() {
        let saved = saved();
        for (stage, early) in [
            (PreparationStage::Workspace, true),
            (PreparationStage::Worktree, true),
            (PreparationStage::SetupScript, true),
            (PreparationStage::Provider, false),
        ] {
            let input = decision::InputState::Preparing {
                stage,
                action: Uuid::from_u128(2),
                mode: decision::DeliveryMode::Direct,
            };
            for unbound in [false, true] {
                let result = retry(Some(&saved), Some(&input), Some("/checkout"), unbound, true);
                if early && unbound {
                    let (workspace, cwd) = result.unwrap().unwrap();
                    assert_eq!(workspace.strategy, Some(WorkspaceStrategy::Root));
                    assert_eq!(cwd, "/project");
                } else {
                    assert!(result.is_err(), "{stage:?}, unbound={unbound}");
                }
            }
        }
        let failed = decision::InputState::Failed(Default::default());
        assert!(
            retry(Some(&saved), Some(&failed), Some("/checkout"), true, true)
                .unwrap()
                .is_some()
        );
        assert!(retry(Some(&saved), Some(&failed), Some("/checkout"), false, true).is_err());
        assert!(retry(None, Some(&failed), Some("/checkout"), true, true).is_err());
    }

    #[test]
    fn retry_reenters_only_a_failed_script_and_preserves_the_checkout() {
        let mut saved = saved();
        let failed = decision::InputState::Failed(Default::default());
        let queued = decision::InputState::Queued { steer: None };
        assert!(
            retry(Some(&saved), Some(&failed), Some("/checkout"), true, false)
                .unwrap()
                .is_none()
        );
        saved
            .setup
            .as_mut()
            .unwrap()
            .stage(SetupStageId::SetupScript, SetupStageStatus::Failed, 1);
        assert!(
            retry(Some(&saved), Some(&queued), Some("/checkout"), true, false)
                .unwrap()
                .is_none()
        );
        let (workspace, cwd) = retry(Some(&saved), Some(&failed), Some("/checkout"), true, false)
            .unwrap()
            .unwrap();
        assert_eq!(workspace, saved);
        assert_eq!(cwd, "/checkout");
    }
}
