use agent_protocol::{models::WorkspaceControls, operations::WorkspaceAction};
use anyhow::{Result, anyhow};
use std::path::Path;

pub(super) fn inspect(cwd: &Path) -> Result<WorkspaceControls> {
    let lines = |args: &[&str]| -> Result<Vec<String>> {
        Ok(crate::git::text(cwd, args)?
            .lines()
            .filter(|s| !s.ends_with("/HEAD"))
            .map(str::to_owned)
            .collect())
    };
    Ok(WorkspaceControls {
        branches: lines(&["for-each-ref", "--format=%(refname:lstrip=2)", "refs/heads"])?,
        references: lines(&[
            "-c",
            "core.warnAmbiguousRefs=true",
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ])?,
        remotes: lines(&["remote"])?,
        can_push: branch(cwd).is_ok()
            && crate::git::text(cwd, &["rev-parse", "--verify", "HEAD"]).is_ok(),
        scripts: Vec::new(),
    })
}

pub(super) fn branch(cwd: &Path) -> Result<String> {
    // Short symbolic refs can acquire a heads/ prefix when a tag has the same name.
    let reference = crate::git::text(cwd, &["symbolic-ref", "--quiet", "HEAD"])?;
    Ok(reference
        .trim()
        .strip_prefix("refs/heads/")
        .ok_or_else(|| anyhow!("HEAD does not name a local branch"))?
        .into())
}

pub(crate) fn change(cwd: &Path, expected_branch: &str, action: &WorkspaceAction) -> Result<()> {
    let symbolic = branch(cwd);
    let branch = symbolic
        .as_ref()
        .cloned()
        .or_else(|_| crate::git::text(cwd, &["rev-parse", "--short", "HEAD"]))?;
    let branch = branch.trim();
    if branch != expected_branch {
        return Err(anyhow!(
            "The workspace branch changed. Refresh and try again."
        ));
    }
    match action {
        WorkspaceAction::SwitchBranch { name } => {
            crate::git::text(
                cwd,
                &["show-ref", "--verify", &format!("refs/heads/{name}")],
            )?;
            crate::git::output(cwd, &["switch", "--", name])?;
        }
        WorkspaceAction::CreateBranch { name } => {
            crate::git::text(cwd, &["check-ref-format", "--branch", name])?;
            crate::git::output(cwd, &["switch", "-c", name, "--"])?;
        }
        WorkspaceAction::Push { remote } => {
            if symbolic.is_err() {
                return Err(anyhow!("Switch to a branch before pushing."));
            }
            push(cwd, branch, remote)?;
        }
        _ => return Err(anyhow!("Expected a Git action")),
    }
    Ok(())
}

fn push(cwd: &Path, branch: &str, remote: &str) -> Result<()> {
    let remotes = crate::git::text(cwd, &["remote"])?;
    if !remotes.lines().any(|name| name == remote) {
        return Err(anyhow!("Unknown Git remote"));
    }
    let configured_remote = crate::git::text(
        cwd,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .unwrap_or_default();
    let target = if configured_remote.trim() == remote {
        crate::git::text(cwd, &["config", "--get", &format!("branch.{branch}.merge")]).ok()
    } else {
        None
    }
    .filter(|target| target.trim().starts_with("refs/heads/"))
    .unwrap_or_else(|| format!("refs/heads/{branch}"));
    crate::git::bounded(
        cwd,
        &[
            "push",
            "--set-upstream",
            "--",
            remote,
            &format!("HEAD:{}", target.trim()),
        ],
        std::time::Duration::from_secs(60),
    )?;
    Ok(())
}
