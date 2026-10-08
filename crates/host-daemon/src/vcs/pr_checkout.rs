//! GitHub pull request lookup and checkout. The Host owns all checkout I/O;
//! the returned record is enough for clients to open a thread at the result.
use super::process::{Execute, NON_INTERACTIVE_ENV, execute};
use super::worktree_ops::{
    canonical_path, create_worktree, fetch_pull_request_branch, fetch_pull_request_head_commit,
    fetch_remote_branch, refresh_checked_out_branch, resolve_commit, set_branch_upstream,
    worktree_of_branch,
};
use super::{Options, git, stdout};
use crate::github::cli::{GhError, GitHubCli, PullRequestRecord};
use agent_protocol::vcs::{
    CreateWorktree, PreparePullRequestThread, PreparedPullRequestThread, PullRequestThreadMode,
    ResolvePullRequest, ResolvedPullRequest, ResolvedPullRequestResult,
};
use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn provider_error(operation: &str, error: GhError) -> anyhow::Error {
    anyhow!("Source control provider github failed in {operation}: {error}")
}

fn resolved(record: PullRequestRecord) -> ResolvedPullRequest {
    ResolvedPullRequest {
        number: record.number,
        title: record.title,
        url: record.url,
        base_branch: record.base_ref_name,
        head_branch: record.head_ref_name,
        state: record.state,
    }
}

async fn resolve_record(
    github: &GitHubCli,
    request: &ResolvePullRequest,
) -> Result<PullRequestRecord> {
    let reference = request.reference.trim();
    if reference.is_empty() {
        return Err(anyhow!("A pull request number or URL is required."));
    }
    let reference = reference.strip_prefix('#').unwrap_or(reference);
    github
        .pull_request(Path::new(&request.cwd), reference)
        .await
        .map_err(|error| provider_error("resolvePullRequest", error))
}

pub(crate) async fn resolve(
    github: Option<&GitHubCli>,
    request: &ResolvePullRequest,
) -> Result<ResolvedPullRequestResult> {
    let github = github.ok_or_else(|| anyhow!("GitHub CLI is unavailable."))?;
    let record = resolve_record(github, request).await?;
    Ok(ResolvedPullRequestResult {
        pull_request: resolved(record),
    })
}

fn checkout_branch(record: &PullRequestRecord) -> String {
    let number = record.number;
    if record.is_cross_repository != Some(true) {
        return record.head_ref_name.clone();
    }
    let branch = record
        .head_ref_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let branch = branch.trim_matches('/');
    let branch = if branch.is_empty() { "head" } else { branch };
    format!("pr/{number}/{branch}")
}

async fn switch_branch(cwd: &Path, branch: &str) -> Result<()> {
    let args = ["switch", "--", branch];
    let result = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: Some(Duration::from_secs(60)),
        ..Execute::new(cwd, &args)
    })
    .await?;
    if result.ok() {
        Ok(())
    } else {
        Err(anyhow!("Git could not switch to the pull request branch."))
    }
}

async fn checkout_local(cwd: &Path, branch: &str, number: u64) -> Result<(String, bool)> {
    fetch_pull_request_branch(cwd, number, branch).await?;
    if stdout(cwd, &["branch", "--show-current"]).as_deref() != Some(branch) {
        switch_branch(cwd, branch).await?;
    }
    let target = resolve_commit(cwd, branch)?;
    let head = resolve_commit(cwd, "HEAD")?;
    if head != target {
        let moved = refresh_checked_out_branch(cwd, branch, None)?;
        if !moved.on_target {
            return Ok((branch.to_owned(), false));
        }
    }
    Ok((branch.to_owned(), resolve_commit(cwd, "HEAD")? == target))
}

fn should_prefer_ssh_remote(url: Option<&str>) -> bool {
    url.map(str::trim).is_some_and(|url| {
        let url = url.to_ascii_lowercase();
        url.starts_with("git@") || url.starts_with("ssh://")
    })
}

async fn restore_pull_request_upstream(
    github: &GitHubCli,
    cwd: &Path,
    record: &PullRequestRecord,
    branch: &str,
) {
    let (remote, remote_url) = if record.is_cross_repository == Some(true) {
        let Some(repository) = record
            .head_repository_name_with_owner
            .as_deref()
            .map(str::trim)
            .filter(|repository| !repository.is_empty())
        else {
            return;
        };
        let urls = match github.repository_clone_urls(cwd, repository).await {
            Ok(urls) => urls,
            Err(error) => {
                tracing::warn!(
                    operation = "host.vcs.pull_request_upstream.repository",
                    message = %provider_error("resolveHeadRepository", error),
                );
                return;
            }
        };
        let preferred_name = record
            .head_repository_owner_login
            .as_deref()
            .map(str::trim)
            .filter(|owner| !owner.is_empty())
            .or_else(|| repository.split('/').next().map(str::trim))
            .filter(|owner| !owner.is_empty())
            .unwrap_or("fork");
        let origin_url = super::config_value(cwd, "remote.origin.url");
        let remote_url = if should_prefer_ssh_remote(origin_url.as_deref()) {
            urls.ssh_url
        } else {
            urls.url
        };
        let remote = match super::worktree_ops::ensure_remote(cwd, preferred_name, &remote_url) {
            Ok(remote) => remote,
            Err(error) => {
                tracing::warn!(
                    operation = "host.vcs.pull_request_upstream.remote",
                    message = %format_args!("{error:#}"),
                );
                return;
            }
        };
        (remote, remote_url)
    } else {
        let Some(remote) = super::primary_remote(cwd) else {
            return;
        };
        (remote, String::new())
    };
    if let Err(error) = fetch_remote_branch(cwd, &remote, &record.head_ref_name).await {
        tracing::warn!(
            operation = "host.vcs.pull_request_upstream.fetch",
            remote = %remote,
            remote_url = %remote_url,
            message = %format_args!("{error:#}"),
        );
        return;
    }
    if let Err(error) = set_branch_upstream(cwd, branch, &remote, &record.head_ref_name) {
        tracing::warn!(
            operation = "host.vcs.pull_request_upstream.configure",
            remote = %remote,
            message = %format_args!("{error:#}"),
        );
    }
}

/// Resolves and checks out a pull request in the project's checkout or a
/// dedicated worktree. An existing clean checkout is fast-forwarded when it
/// still represents the PR branch.
pub(crate) async fn prepare(
    github: Option<&GitHubCli>,
    request: &PreparePullRequestThread,
    worktree_directory: &str,
) -> Result<PreparedPullRequestThread> {
    let github = github.ok_or_else(|| anyhow!("GitHub CLI is unavailable."))?;
    let record = resolve_record(
        github,
        &ResolvePullRequest {
            cwd: request.cwd.clone(),
            reference: request.reference.clone(),
        },
    )
    .await?;
    let branch = checkout_branch(&record);
    let resolved = resolved(record.clone());
    let cwd = Path::new(&request.cwd);
    match request.mode {
        PullRequestThreadMode::Local => {
            let (_, on_head) = checkout_local(cwd, &branch, resolved.number).await?;
            if on_head {
                restore_pull_request_upstream(github, cwd, &record, &branch).await;
            }
            Ok(PreparedPullRequestThread {
                pull_request: resolved,
                branch,
                worktree_path: None,
                is_on_pull_request_head: on_head,
            })
        }
        PullRequestThreadMode::Worktree => {
            let root = canonical_path(cwd);
            if let Some(existing) = worktree_of_branch(cwd, &branch) {
                let existing = canonical_path(&existing);
                if existing == root {
                    return Err(anyhow!(
                        "This pull request branch is already checked out in the main repository. Use Local, or switch the main repository off that branch before creating a worktree thread."
                    ));
                }
                // A branch checked out in another worktree cannot be updated by
                // writing refs/heads directly. Fetch FETCH_HEAD instead and let
                // the checkout helper fast-forward or safely reset it.
                let target = fetch_pull_request_head_commit(cwd, resolved.number).await?;
                let head_before = resolve_commit(&existing, "HEAD")?;
                let refreshed = refresh_checked_out_branch(
                    &existing,
                    &target,
                    Some(head_before.as_str()),
                )?;
                if refreshed.on_target {
                    restore_pull_request_upstream(github, &existing, &record, &branch).await;
                }
                return Ok(PreparedPullRequestThread {
                    pull_request: resolved,
                    branch,
                    worktree_path: Some(existing.to_string_lossy().into_owned()),
                    is_on_pull_request_head: refreshed.on_target,
                });
            }
            fetch_pull_request_branch(cwd, resolved.number, &branch).await?;
            let path = worktree_of_branch(cwd, &branch).unwrap_or_else(|| {
                let parent = if worktree_directory.is_empty() {
                    cwd.join(".worktree")
                } else {
                    PathBuf::from(worktree_directory)
                };
                parent.join("pull-request").join(resolved.number.to_string())
            });
            let path = canonical_path(&path);
            if !path.is_dir() {
                create_worktree(
                    &CreateWorktree {
                        cwd: request.cwd.clone(),
                        ref_name: branch.clone(),
                        new_ref_name: None,
                        base_ref_name: Some(resolved.base_branch.clone()),
                        path: Some(path.to_string_lossy().into_owned()),
                    },
                    worktree_directory,
                )
                .await?;
            }
            let target = resolve_commit(&path, &branch)?;
            let on_head = resolve_commit(&path, "HEAD")? == target;
            if on_head {
                restore_pull_request_upstream(github, &path, &record, &branch).await;
            }
            Ok(PreparedPullRequestThread {
                pull_request: resolved,
                branch,
                worktree_path: Some(path.to_string_lossy().into_owned()),
                is_on_pull_request_head: on_head,
            })
        }
    }
}

/// Publishes a local checkout as a GitHub repository, adds the selected remote
/// and pushes the current branch when the repository already has a commit.
pub(crate) async fn publish(
    github: Option<&GitHubCli>,
    request: &agent_protocol::vcs::PublishRepository,
) -> Result<agent_protocol::vcs::PublishedRepository> {
    use agent_protocol::vcs::{CloneProtocol, PublishStatus, RepositoryInfo, SourceControlProviderKind};
    if request.provider != SourceControlProviderKind::Github {
        return Err(anyhow!("Only GitHub repositories can be published."));
    }
    let github = github.ok_or_else(|| anyhow!("GitHub CLI is unavailable."))?;
    let cwd = Path::new(&request.cwd);
    let urls = github
        .create_repository(cwd, &request.repository, request.visibility)
        .await
        .map_err(|error| provider_error("publishRepository", error))?;
    let remote_name = request.remote_name.as_deref().unwrap_or("origin");
    let remote_url = match request.protocol.unwrap_or(CloneProtocol::Auto) {
        CloneProtocol::Ssh => urls.ssh_url.clone(),
        CloneProtocol::Https | CloneProtocol::Auto => urls.url.clone(),
    };
    let remote_name = super::worktree_ops::ensure_remote(cwd, remote_name, &remote_url)?;
    let branch = stdout(cwd, &["branch", "--show-current"]).unwrap_or_default();
    let has_head = git(cwd, &["rev-parse", "--verify", "HEAD"], Options::default())
        .is_ok_and(|result| result.ok());
    if branch.is_empty() || !has_head {
        return Ok(agent_protocol::vcs::PublishedRepository {
            repository: RepositoryInfo {
                provider: SourceControlProviderKind::Github,
                name_with_owner: urls.name_with_owner,
                url: urls.url,
                ssh_url: urls.ssh_url,
            },
            remote_name,
            remote_url,
            branch,
            upstream_branch: None,
            status: PublishStatus::RemoteAdded,
        });
    }
    let args = ["push", "--set-upstream", remote_name.as_str(), branch.as_str()];
    let pushed = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: Some(Duration::from_secs(5 * 60)),
        ..Execute::new(cwd, &args)
    })
    .await?;
    if !pushed.ok() {
        return Err(anyhow!("Git could not push the published repository."));
    }
    Ok(agent_protocol::vcs::PublishedRepository {
        repository: RepositoryInfo {
            provider: SourceControlProviderKind::Github,
            name_with_owner: urls.name_with_owner,
            url: urls.url,
            ssh_url: urls.ssh_url,
        },
        remote_name: remote_name.clone(),
        remote_url,
        branch: branch.clone(),
        upstream_branch: Some(branch),
        status: PublishStatus::Pushed,
    })
}

#[cfg(test)]
mod tests {
    use super::checkout_branch;
    use crate::github::cli::PullRequestRecord;
    use agent_protocol::vcs::ChangeRequestState;

    fn record(head: &str, cross_repository: Option<bool>) -> PullRequestRecord {
        PullRequestRecord {
            number: 7,
            title: "Change".into(),
            url: "https://github.com/acme/project/pull/7".into(),
            base_ref_name: "main".into(),
            head_ref_name: head.into(),
            state: ChangeRequestState::Open,
            is_draft: false,
            closed_at: None,
            merged_at: None,
            updated_at: None,
            is_cross_repository: cross_repository,
            head_repository_name_with_owner: None,
            head_repository_owner_login: None,
        }
    }

    #[test]
    fn same_repository_checkout_keeps_the_head_branch() {
        assert_eq!(checkout_branch(&record("feature/page", Some(false))), "feature/page");
        assert_eq!(checkout_branch(&record("feature/page", None)), "feature/page");
    }

    #[test]
    fn cross_repository_checkout_uses_an_isolated_local_branch() {
        assert_eq!(
            checkout_branch(&record("Feature/page", Some(true))),
            "pr/7/feature/page"
        );
    }
}
