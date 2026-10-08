//! `git pull --ff-only` on the checked-out branch.
use super::process::{Execute, execute};
use super::status::{current_upstream, remote_details};
use super::stdout;
use agent_protocol::vcs::{PullResult, PullStatus};
use anyhow::{Result, anyhow};
use std::path::Path;
use std::time::Duration;

const PULL_TIMEOUT: Duration = Duration::from_secs(30);

fn failed(cwd: &Path, detail: &str) -> anyhow::Error {
    anyhow!(
        "Git command failed in GitVcsDriver.pullCurrentBranch ({}): {detail}",
        cwd.display()
    )
}

/// Fast-forwards the checked-out branch to its upstream.
pub(crate) async fn pull_current_branch(cwd: &str) -> Result<PullResult> {
    let cwd = Path::new(cwd).to_owned();
    let (branch, upstream, before) = tokio::task::spawn_blocking({
        let cwd = cwd.clone();
        move || -> Result<_> {
            let details = remote_details(&cwd)?
                .ok_or_else(|| failed(&cwd, "Not a Git repository."))?;
            let branch = details
                .branch
                .ok_or_else(|| failed(&cwd, "Cannot pull from detached HEAD."))?;
            if !details.has_upstream {
                return Err(failed(
                    &cwd,
                    "Current branch has no upstream configured. Push with upstream first.",
                ));
            }
            let before = stdout(&cwd, &["rev-parse", "HEAD"]).unwrap_or_default();
            Ok((branch, details.upstream_ref, before))
        }
    })
    .await??;
    let pulled = execute(Execute {
        env: &[("GIT_TERMINAL_PROMPT", "0")],
        timeout: Some(PULL_TIMEOUT),
        ..Execute::new(&cwd, &["pull", "--ff-only"])
    })
    .await?;
    if !pulled.ok() {
        return Err(failed(&cwd, "git pull failed"));
    }
    let (after, upstream_after) = tokio::task::spawn_blocking({
        let cwd = cwd.clone();
        move || {
            (
                stdout(&cwd, &["rev-parse", "HEAD"]).unwrap_or_default(),
                current_upstream(&cwd).map(|upstream| upstream.reference),
            )
        }
    })
    .await?;
    Ok(PullResult {
        status: if !before.is_empty() && before == after {
            PullStatus::SkippedUpToDate
        } else {
            PullStatus::Pulled
        },
        ref_name: branch,
        upstream_ref: upstream_after.or(upstream),
    })
}
