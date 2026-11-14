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
    Discarded,
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
                SetupChange::Discarded => {
                    header.details.workspace.as_mut().filter(|workspace| workspace.generation == generation && workspace.created_worktree)
                        .is_some_and(|workspace| {
                            workspace.created_worktree = false;
                            if let Some(setup) = &mut workspace.setup {
                                setup.worktree_path = None; setup.branch = None;
                                setup.finish(SetupPhase::Cancelled, None, now.max(0) as u64);
                            }
                            true
                        })
                }
                SetupChange::Expire { ended_at } => {
                    if let Some(workspace) = header.details.workspace.as_mut().filter(|workspace| workspace.generation == generation && workspace.visible && workspace.setup.as_ref().is_some_and(|setup| setup.ended_at_ms == Some(ended_at))) {
                        workspace.visible = false; true
                    } else { false }
                }
                update => header.details.workspace.as_mut().filter(|workspace| workspace.generation == generation)
                    .and_then(|workspace| workspace.setup.as_mut()).is_some_and(|setup| apply(setup, update, now.max(0) as u64)),
            };
            let mut step = Step::default();
            let mut updated = Vec::new();
            if changed {
                step.outcome = Outcome::Accepted;
                transaction.execute("UPDATE threads SET body=?1 WHERE key=?2",params![codec::header(&header, &counters)?,key]).map_err(failure)?;
                updated.push(Row { key: Key::Thread(target.clone()), version: 0, position: Position::Unordered, content: Content::Full(Arc::new(Record::Thread(header))) });
            }
            Self::commit(transaction, Some(target), step, updated, Vec::new(), false)
        })
    }
    pub(in crate::host_rpc) fn workspace_scripts(
        &self,
        root: &str,
    ) -> Result<Vec<agent_protocol::models::ProjectScript>, String> {
        self.with(|connection| {
            scripts(connection, root).map(|scripts| {
                scripts
                    .into_iter()
                    .map(|script| agent_protocol::models::ProjectScript {
                        name: script.name,
                        command: script.command,
                    })
                    .collect()
            })
        })
    }

    pub(in crate::host_rpc) fn save_workspace_script(
        &self,
        root: &str,
        previous_name: Option<&str>,
        script: agent_protocol::models::ProjectScript,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let mut saved = scripts(&transaction, root)?;
            if saved.iter().any(|saved| {
                saved.name == script.name && Some(saved.name.as_str()) != previous_name
            }) {
                return Err("A script with this name already exists.".into());
            }
            if let Some(previous) = previous_name {
                let saved = saved
                    .iter_mut()
                    .find(|script| script.name == previous)
                    .ok_or("The script changed. Refresh and try again.")?;
                saved.name = script.name;
                saved.command = script.command;
            } else {
                saved.push(agent_protocol::orchestration::ProjectScript {
                    id: Uuid::new_v4().to_string(),
                    name: script.name,
                    command: script.command,
                    icon: "play".into(),
                    run_on_worktree_create: false,
                    run_on_settle: None,
                    stop_on_settle: None,
                    asynchronous: None,
                    preview_url: None,
                    auto_open_preview: None,
                });
            }
            let registered:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM projection_projects WHERE workspace_root=?1 AND deleted_at IS NULL)",[root],|row|row.get(0)).map_err(failure)?;
            let mut updated=Vec::new();
            if !registered {
                let project=agent_protocol::models::Project {id:Uuid::new_v4().to_string(),
                    name:std::path::Path::new(root).file_name().and_then(|name|name.to_str()).unwrap_or(root).into(),
                    roots:vec![agent_protocol::models::ProjectRoot {path:root.into()}],icon:None};
                updated.push(projects::write(&transaction,&project)?);
                jobs::enqueue(&transaction,Uuid::new_v4(),"projects",super::super::runtime::now(),&jobs::Work::ProjectsChanged)?;
            }
            save_scripts(&transaction, root, &saved)?;
            Self::commit(transaction,None,Step::default(),updated,Vec::new(),false).map(|_|())
        })
    }

    pub(in crate::host_rpc) fn remove_workspace_script(
        &self,
        root: &str,
        name: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let mut saved = scripts(&transaction, root)?;
            let before = saved.len();
            saved.retain(|script| script.name != name);
            if saved.len() != before {
                save_scripts(&transaction, root, &saved)?;
            }
            transaction.commit().map_err(failure)
        })
    }
}

fn scripts(
    connection: &Connection,
    root: &str,
) -> Result<Vec<agent_protocol::orchestration::ProjectScript>, String> {
    let body=connection.query_row("SELECT scripts_json FROM projection_projects WHERE workspace_root=?1 AND deleted_at IS NULL",[root],|row|row.get::<_,String>(0)).optional().map_err(failure)?;
    body.map(|body| serde_json::from_str(&body).map_err(failure))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn save_scripts(
    transaction: &Transaction<'_>,
    root: &str,
    scripts: &[agent_protocol::orchestration::ProjectScript],
) -> Result<(), String> {
    let now = super::projects::timestamp();
    let count=transaction.execute("UPDATE projection_projects SET scripts_json=?1,updated_at=?2 WHERE workspace_root=?3 AND deleted_at IS NULL",
        params![serde_json::to_string(scripts).map_err(failure)?,now,root]).map_err(failure)?;
    if count != 1 {
        return Err("Scripts require a registered project.".into());
    }
    Ok(())
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
        SetupChange::Begin { .. } | SetupChange::Expire { .. } | SetupChange::Discarded => {
            return false;
        }
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
