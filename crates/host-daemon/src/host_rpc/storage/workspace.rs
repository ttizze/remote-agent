//! Workspace preparation is metadata of the canonical conversation header.
use super::*;
use agent_protocol::workspace::SetupChange;

impl Storage {
    pub(in crate::host_rpc) fn setup(
        &self,
        target: &SessionRef,
        generation: Uuid,
        update: SetupChange,
        now: i64,
    ) -> Result<bool, String> {
        let at = chrono::DateTime::from_timestamp_millis(now)
            .ok_or("workspace timestamp is out of range")?
            .to_rfc3339();
        let events = self.observe_thread(&target.application_id(), &[], &at, |state| {
            agent_protocol::orchestration::workspace_progress(state, generation, update, &at)
        })?;
        Ok(!events.is_empty())
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
            if !registered {return Err("Project is not registered.".into());}
            save_scripts(&transaction, root, &saved)?;
            transaction.commit().map_err(failure)

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
    let id: String = transaction.query_row("SELECT project_id FROM projection_projects WHERE workspace_root=?1 AND deleted_at IS NULL",[root],|row|row.get(0)).optional().map_err(failure)?.ok_or("Scripts require a registered project.")?;
    let now = super::projects::timestamp();
    let changed = transaction.execute("UPDATE projection_projects SET scripts_json=?1,updated_at=?2 WHERE project_id=?3 AND scripts_json!=?1",
        params![serde_json::to_string(scripts).map_err(failure)?,now,id]).map_err(failure)?;
    if changed > 0 {
        projects::publish(transaction, &id, "project.meta-updated")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::workspace::{SetupPhase, SetupStageId, SetupStageStatus, WorkspaceSetup};

    #[test]
    fn an_async_script_failure_keeps_its_message_when_the_setup_card_finishes() {
        let mut setup = WorkspaceSetup::new(None, 0);
        for stage in &mut setup.stages {
            stage.status = SetupStageStatus::Done;
        }
        assert!(setup.apply(
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
