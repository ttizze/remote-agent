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
        error: Option<String>,
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
            let changed = match update {
                SetupChange::Begin { input, workspace } => {
                    if rows::input(&transaction, key, input)?.is_some_and(|input| matches!(input.state, decision::InputState::Preparing { action, .. } if action == generation)) {
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
            let mut result = Committed { step: Step::default(), cursor: cursor(&transaction)?, rows: vec![], removed: vec![], reset: false };
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
            error,
        } => {
            setup.stage(id, status, now);
            if error.is_some() {
                setup.error = error;
            }
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
        setup.finish(SetupPhase::Done, setup.error.clone(), now);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::workspace::WorkspaceSetup;

    #[test]
    fn an_async_script_failure_keeps_its_message_when_the_setup_card_finishes() {
        let mut setup = WorkspaceSetup::new(None, 0);
        for stage in &mut setup.stages {
            stage.status = SetupStageStatus::Done;
        }
        assert!(apply(
            &mut setup,
            SetupChange::Stage {
                id: SetupStageId::SetupScript,
                status: SetupStageStatus::Failed,
                exit_code: None,
                error: Some("terminal output failed".into())
            },
            1
        ));
        assert_eq!(setup.phase, SetupPhase::Done);
        assert_eq!(setup.error.as_deref(), Some("terminal output failed"));
        assert_eq!(
            setup
                .stages
                .iter()
                .find(|stage| stage.id == SetupStageId::SetupScript)
                .unwrap()
                .status,
            SetupStageStatus::Failed
        );
    }
}
