use super::*;
use agent_protocol::models::{Empty, ProjectScript, WorkspaceReview};
use agent_protocol::operations::{ChangeWorkspace, ReviewWorkspace, WorkspaceAction};
use std::path::Path;

impl HostRpcService {
    pub(super) async fn workspace_options(
        &self,
        cwd: &str,
    ) -> Result<agent_protocol::workspace::WorkspaceOptions, Failure> {
        let settings = self
            .inner
            .worktrees
            .settings(None)
            .await
            .map_err(|error| Failure::new("worktree_settings_failed", error))?;
        let cwd = cwd.to_owned();
        tokio::task::spawn_blocking(move || {
            let root = Path::new(&cwd);
            let refs = crate::git::text(
                root,
                &[
                    "for-each-ref",
                    "--format=%(refname:short)",
                    "refs/heads",
                    "refs/remotes",
                    "refs/tags",
                ],
            );
            let is_repository = refs.is_ok();
            let refs = refs
                .unwrap_or_default()
                .lines()
                .take(2000)
                .map(str::to_owned)
                .collect();
            let entries = crate::git::text(root, &["worktree", "list", "--porcelain", "-z"])
                .unwrap_or_default();
            let worktrees = entries
                .split("\0\0")
                .filter_map(|entry| {
                    entry
                        .strip_prefix("worktree ")?
                        .split('\0')
                        .next()
                        .map(str::to_owned)
                })
                .collect();
            agent_protocol::workspace::WorkspaceOptions {
                is_repository,
                default_worktree: settings.create_on_new_session,
                default_base: if is_repository {
                    crate::worktrees::default_base(root).unwrap_or_else(|_| "HEAD".into())
                } else {
                    String::new()
                },
                refs,
                worktrees,
            }
        })
        .await
        .map_err(|error| Failure::new("workspace_options_failed", error))
    }
    pub(super) async fn review_workspace(
        &self,
        params: &ReviewWorkspace,
    ) -> Result<WorkspaceReview, Failure> {
        let mut review = crate::inspect_workspace(params.cwd.clone(), params.base_ref.clone())
            .await
            .map_err(|error| Failure::new("workspace_review_failed", error))?;
        review.controls.scripts = match self.script_project_root(&params.cwd).await? {
            Some(root) => {
                let store = self.inner.conversation.storage.clone();
                tokio::task::spawn_blocking(move || store.workspace_scripts(&root))
                    .await
                    .map_err(|error| Failure::new("workspace_review_failed", error))?
                    .map_err(|error| Failure::new("workspace_review_failed", error))?
            }
            None => Vec::new(),
        };
        if params.include_options {
            review.options = Some(self.workspace_options(&params.cwd).await?);
        }
        Ok(review)
    }

    pub(super) async fn change_workspace(
        &self,
        params: &ChangeWorkspace,
    ) -> Result<Empty, Failure> {
        let _access = self.inner.workspace_access.lock().await;
        let params = params.clone();
        let script = match &params.action {
            WorkspaceAction::SaveScript { name, command, .. } => Some(
                validate_script(name.clone(), command.clone())
                    .map_err(|error| Failure::new("workspace_change_failed", error))?,
            ),
            _ => None,
        };
        let script_root = if matches!(
            params.action,
            WorkspaceAction::SaveScript { .. } | WorkspaceAction::RemoveScript { .. }
        ) {
            let root = self
                .script_project_root(&params.cwd)
                .await?
                .ok_or_else(|| {
                    Failure::new(
                        "workspace_change_failed",
                        "Project scripts are unavailable for this workspace.",
                    )
                })?;
            Some(root)
        } else {
            None
        };
        let store = self.inner.conversation.storage.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let cwd = Path::new(&params.cwd);
            match params.action {
                WorkspaceAction::SaveScript { previous_name, .. } => store.save_workspace_script(
                    script_root.as_deref().expect("script root resolved"),
                    previous_name.as_deref(),
                    script.expect("script validated"),
                ),
                WorkspaceAction::RemoveScript { name } => store.remove_workspace_script(
                    script_root.as_deref().expect("script root resolved"),
                    &name,
                ),
                action => crate::workspace_review::controls::change(cwd, &params.branch, &action)
                    .map_err(|error| error.to_string()),
            }
        })
        .await
        .map_err(|error| Failure::new("workspace_change_failed", error))?
        .map_err(|error| Failure::new("workspace_change_failed", error))?;
        Ok(Empty {})
    }
    async fn script_project_root(&self, cwd: &str) -> Result<Option<String>, Failure> {
        let directory = cwd.to_owned();
        let source = tokio::task::spawn_blocking(move || source_script_root(Path::new(&directory)))
            .await
            .map_err(|error| Failure::new("project_read_failed", error))?
            .map_err(|error| Failure::new("project_read_failed", error))?;
        let Some(source) = source else {
            return Ok(None);
        };
        let projects = self.project_snapshot().await?;
        if crate::projects::state::scratch_root(&projects.scratch_roots, Path::new(cwd)).is_some()
            || crate::projects::state::scratch_root(&projects.scratch_roots, Path::new(&source))
                .is_some()
        {
            return Ok(None);
        }
        let project = projects
            .project_for_workspace(cwd)
            .or_else(|| projects.project_for_workspace(&source))
            .and_then(|id| projects.projects.iter().find(|project| project.id == id));
        Ok(Some(
            match project.and_then(|project| project.roots.first()) {
                Some(root) => root.path.clone(),
                None => source,
            },
        ))
    }
}

fn source_script_root(cwd: &Path) -> Result<Option<String>, String> {
    let cwd = match dunce::canonicalize(cwd) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !cwd.is_dir() {
        return Err("project path must be a directory".into());
    }
    // The first Git worktree is the source checkout, including for manually
    // created linked worktrees. Plain folders own their project scripts directly.
    let source = crate::git::text(&cwd, &["worktree", "list", "--porcelain", "-z"])
        .ok()
        .and_then(|text| {
            text.strip_prefix("worktree ")
                .and_then(|text| text.split('\0').next())
                .map(std::path::PathBuf::from)
        })
        .unwrap_or(cwd);
    source
        .to_str()
        .map(|path| Some(path.into()))
        .ok_or("project path is not UTF-8".into())
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

#[cfg(test)]
mod tests;
