use super::*;
use agent_protocol::models::{Empty, ProjectScript, WorkspaceReview};
use agent_protocol::operations::{ChangeWorkspace, ReviewWorkspace, WorkspaceAction};
use std::path::Path;

impl HostRpcService {
    pub(super) async fn review_workspace(
        &self,
        params: &ReviewWorkspace,
    ) -> Result<WorkspaceReview, Failure> {
        let mut review = crate::inspect_workspace(params.cwd.clone(), params.base_ref.clone())
            .await
            .map_err(|error| Failure::new("workspace_review_failed", error))?;
        let store = self.inner.store.clone();
        let cwd = params.cwd.clone();
        review.controls.scripts = tokio::task::spawn_blocking(move || {
            match std::fs::metadata(&cwd) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error.to_string()),
                Ok(_) => {}
            }
            store.load_state::<Vec<ProjectScript>>(&script_key(Path::new(&cwd))?)
        })
        .await
        .map_err(|error| Failure::new("workspace_review_failed", error))?
        .map_err(|error| Failure::new("workspace_review_failed", error))?;
        Ok(review)
    }

    pub(super) async fn change_workspace(
        &self,
        params: &ChangeWorkspace,
    ) -> Result<Empty, Failure> {
        let _access = self.inner.workspace_access.lock().await;
        let params = params.clone();
        let store = self.inner.store.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let cwd = Path::new(&params.cwd);
            match params.action {
                WorkspaceAction::SaveScript {
                    previous_name,
                    name,
                    command,
                } => {
                    let script = validate_script(name, command)?;
                    store.update_state::<Vec<ProjectScript>>(&script_key(cwd)?, |scripts| {
                        save_script(scripts, previous_name.as_deref(), script)
                    })
                }
                WorkspaceAction::RemoveScript { name } => {
                    store.update_state::<Vec<ProjectScript>>(&script_key(cwd)?, |scripts| {
                        scripts.retain(|script| script.name != name);
                        Ok(())
                    })
                }
                action => crate::workspace_review::controls::change(cwd, &params.branch, &action)
                    .map_err(|error| error.to_string()),
            }
        })
        .await
        .map_err(|error| Failure::new("workspace_change_failed", error))?
        .map_err(|error| Failure::new("workspace_change_failed", error))?;
        Ok(Empty {})
    }
}

fn script_key(cwd: &Path) -> Result<String, String> {
    // Linked worktrees share scripts with their source checkout. Plain folders
    // use their canonical path; no project files are created or modified.
    let common = crate::git::text(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .map(|path| std::path::PathBuf::from(path.trim()));
    let path =
        dunce::canonicalize(common.as_deref().unwrap_or(cwd)).map_err(|error| error.to_string())?;
    Ok(format!("project-scripts:{}", path.to_string_lossy()))
}

fn validate_script(name: String, command: String) -> Result<ProjectScript, String> {
    let name = name.trim();
    let command = command.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err("Enter a script name of up to 120 bytes.".into());
    }
    if command.is_empty() || command.len() > 16384 || command.contains('\0') {
        return Err("Enter a command of up to 16384 bytes.".into());
    }
    Ok(ProjectScript {
        name: name.into(),
        command: command.into(),
    })
}

fn save_script(
    scripts: &mut Vec<ProjectScript>,
    previous_name: Option<&str>,
    script: ProjectScript,
) -> Result<(), String> {
    if scripts
        .iter()
        .any(|s| s.name == script.name && Some(s.name.as_str()) != previous_name)
    {
        return Err("A script with this name already exists.".into());
    }
    if let Some(previous_name) = previous_name {
        let existing = scripts
            .iter_mut()
            .find(|s| s.name == previous_name)
            .ok_or("The script changed. Refresh and try again.")?;
        *existing = script;
    } else {
        scripts.push(script);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
