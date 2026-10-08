//! Worktrees from the branch picker, `git init`, and the remote and branch
//! plumbing the pull request checkout and publishing need.
use super::process::{Execute, NON_INTERACTIVE_ENV, execute};
use super::{
    Options, config_value, git, primary_remote, remote_names, run_git, split_remote_ref, stdout,
};
use agent_protocol::vcs::{CreateWorktree, CreatedWorktree, WorktreeCheckout};
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const WORKTREE_ADD_TIMEOUT: Duration = Duration::from_secs(300);
const WORKTREE_REMOVE_TIMEOUT: Duration = Duration::from_secs(300);
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

fn failed(operation: &str, cwd: &Path, detail: &str) -> anyhow::Error {
    anyhow!(
        "Git command failed in {operation} ({}): {detail}",
        cwd.display()
    )
}

/// `git init` in `cwd`.
pub(crate) async fn init_repository(cwd: &str) -> Result<()> {
    let cwd = Path::new(cwd);
    let run = execute(Execute {
        timeout: Some(INIT_TIMEOUT),
        ..Execute::new(cwd, &["init"])
    })
    .await?;
    if !run.ok() {
        return Err(failed("GitVcsDriver.initRepo", cwd, "git init failed"));
    }
    Ok(())
}

/// Where a worktree of `branch` goes without an explicit path: the Host's
/// worktree directory, else `<repository>/.worktree`, then the repository's
/// name and the branch with `/` as `-`.
pub(crate) fn default_worktree_path(cwd: &Path, worktree_directory: &str, branch: &str) -> PathBuf {
    let parent = if worktree_directory.is_empty() {
        cwd.join(".worktree")
    } else {
        PathBuf::from(worktree_directory)
    };
    let repository = cwd
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repository".into());
    parent.join(repository).join(branch.replace('/', "-"))
}

/// `git worktree add`, submodules included, then the base the Changes view
/// compares a new branch with.
pub(crate) async fn create_worktree(
    request: &CreateWorktree,
    worktree_directory: &str,
) -> Result<CreatedWorktree> {
    let cwd = Path::new(&request.cwd);
    let target_branch = request
        .new_ref_name
        .clone()
        .unwrap_or_else(|| request.ref_name.clone());
    let worktree_path = request
        .path
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| default_worktree_path(cwd, worktree_directory, &target_branch));
    let path_text = worktree_path.to_string_lossy().into_owned();
    let checkout_workers = config_value(cwd, "checkout.workers").unwrap_or_else(|| "0".into());
    let workers = format!("checkout.workers={checkout_workers}");
    let mut args = vec!["-c", workers.as_str(), "worktree", "add"];
    if let Some(new_ref) = &request.new_ref_name {
        args.extend(["-b", new_ref.as_str()]);
    }
    args.extend(["--", path_text.as_str(), request.ref_name.as_str()]);
    let added = execute(Execute {
        timeout: Some(WORKTREE_ADD_TIMEOUT),
        ..Execute::new(cwd, &args)
    })
    .await?;
    if !added.ok() {
        return Err(failed(
            "GitVcsDriver.createWorktree",
            cwd,
            "git worktree add failed",
        ));
    }
    // `git worktree add` leaves submodules empty; populating them is best
    // effort and never fails the caller.
    if worktree_path.join(".gitmodules").exists() {
        let updated = execute(Execute {
            timeout: None,
            ..Execute::new(
                &worktree_path,
                &["submodule", "update", "--init", "--recursive"],
            )
        })
        .await;
        if !matches!(updated, Ok(run) if run.ok()) {
            tracing::warn!(
                path = %worktree_path.display(),
                "worktree submodule checkout failed; submodule paths are empty"
            );
        }
    }
    if let (Some(new_ref), Some(base)) = (&request.new_ref_name, &request.base_ref_name) {
        let base_branch = split_remote_ref(base, &remote_names(cwd))
            .map(|(_, branch)| branch)
            .unwrap_or_else(|| base.clone());
        let key = format!("branch.{new_ref}.gh-merge-base");
        run_git(
            cwd,
            "GitVcsDriver.createWorktree.configureBaseRef",
            &["config", &key, &base_branch],
            "git config failed",
        )?;
    }
    Ok(CreatedWorktree {
        worktree: WorktreeCheckout {
            path: path_text,
            ref_name: target_branch,
        },
    })
}

fn missing_worktree(stderr: &str) -> bool {
    let normalized = stderr.to_lowercase();
    normalized.contains("is not a working tree")
        || normalized.contains("cannot remove working tree")
}

/// `git worktree remove`; a worktree already gone is pruned instead so a
/// stale registration cannot block a later `worktree add` at its path.
pub(crate) async fn remove_worktree(cwd: &str, path: &str, force: bool) -> Result<()> {
    let cwd = Path::new(cwd);
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.extend(["--", path]);
    let removed = execute(Execute {
        timeout: Some(WORKTREE_REMOVE_TIMEOUT),
        ..Execute::new(cwd, &args)
    })
    .await?;
    if removed.ok() {
        return Ok(());
    }
    if missing_worktree(&removed.stderr) && !Path::new(path).exists() {
        return prune_worktrees(cwd);
    }
    tracing::warn!(
        operation = "host.vcs.remove_worktree",
        exit_code = removed.code,
        stderr_length = removed.stderr.len(),
        "git worktree remove failed"
    );
    Err(failed(
        "GitVcsDriver.removeWorktree",
        cwd,
        "git worktree remove failed",
    ))
}

pub(crate) fn prune_worktrees(cwd: &Path) -> Result<()> {
    run_git(
        cwd,
        "GitVcsDriver.pruneWorktrees",
        &["worktree", "prune"],
        "git worktree prune failed",
    )
    .map(|_| ())
}

pub(crate) fn delete_local_branch(cwd: &Path, branch: &str, force: bool) -> Result<()> {
    run_git(
        cwd,
        "GitVcsDriver.deleteLocalBranch",
        &["branch", if force { "-D" } else { "-d" }, "--", branch],
        "git branch delete failed",
    )
    .map(|_| ())
}

/// A remote name Git accepts: other characters become dashes; `fork` when
/// nothing is left.
fn sanitize_remote_name(value: &str) -> String {
    let mut name = String::new();
    let mut dash = false;
    for c in value.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            name.push(c);
            dash = false;
        } else if !dash {
            name.push('-');
            dash = true;
        }
    }
    let name = name.trim_matches('-');
    if name.is_empty() {
        "fork".into()
    } else {
        name.to_owned()
    }
}

/// `git remote -v` fetch URLs by remote name.
fn fetch_urls(cwd: &Path) -> BTreeMap<String, String> {
    stdout(cwd, &["remote", "-v"])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (name, url, direction) = (parts.next()?, parts.next()?, parts.next()?);
            (direction == "(fetch)").then(|| (name.to_owned(), url.to_owned()))
        })
        .collect()
}

/// The remote at `url`, reusing one whose URL names the same repository, or
/// adding it as `preferred_name` (`-1`, `-2`… when that name is taken).
pub(crate) fn ensure_remote(cwd: &Path, preferred_name: &str, url: &str) -> Result<String> {
    let preferred = sanitize_remote_name(preferred_name);
    let target = agent_runtime::normalize_remote_url(url);
    let remotes = fetch_urls(cwd);
    if let Some((name, _)) = remotes
        .iter()
        .find(|(_, existing)| agent_runtime::normalize_remote_url(existing) == target)
    {
        return Ok(name.clone());
    }
    let mut name = preferred.clone();
    let mut suffix = 1;
    while remotes.contains_key(&name) {
        name = format!("{preferred}-{suffix}");
        suffix += 1;
    }
    run_git(
        cwd,
        "GitVcsDriver.ensureRemote.add",
        &["remote", "add", "--", &name, url],
        "git remote add failed",
    )?;
    Ok(name)
}

/// Fetches `remote` without prompting; `refspec` narrows the fetch.
pub(crate) async fn fetch(cwd: &Path, remote: &str, refspec: Option<&str>) -> Result<()> {
    let mut args = vec!["fetch", "--quiet", "--no-tags", "--", remote];
    if let Some(refspec) = refspec {
        args.push(refspec);
    }
    let fetched = execute(Execute {
        env: &NON_INTERACTIVE_ENV,
        timeout: None,
        ..Execute::new(cwd, &args)
    })
    .await?;
    if !fetched.ok() {
        return Err(failed("GitVcsDriver.fetchRemote", cwd, "git fetch failed"));
    }
    Ok(())
}

/// Fetches a pull request's head into a local branch.
pub(crate) async fn fetch_pull_request_branch(cwd: &Path, number: u64, branch: &str) -> Result<()> {
    let remote = primary_remote(cwd).ok_or_else(|| {
        failed(
            "GitVcsDriver.resolvePrimaryRemoteName",
            cwd,
            "No git remote is configured for this repository.",
        )
    })?;
    fetch(
        cwd,
        &remote,
        Some(&format!("+refs/pull/{number}/head:refs/heads/{branch}")),
    )
    .await
    .map_err(|_| {
        failed(
            "GitVcsDriver.fetchPullRequestBranch",
            cwd,
            "git fetch pull request branch failed",
        )
    })
}

/// Fetches a same-repository branch into its normal remote-tracking ref.
/// Pull request materialization may have created only a local branch, so this
/// is kept separate from the PR ref fetch used for fork heads.
pub(crate) async fn fetch_remote_branch(cwd: &Path, remote: &str, branch: &str) -> Result<()> {
    fetch(
        cwd,
        remote,
        Some(&format!(
            "+refs/heads/{branch}:refs/remotes/{remote}/{branch}"
        )),
    )
    .await
}

/// Fetches the primary origin before an explicit-path worktree is created and
/// returns its remote-tracking commit when that branch exists. Keeping the
/// fallback local preserves the managed-worktree behavior for repositories
/// whose origin does not publish the requested base ref.
pub(crate) async fn origin_start(cwd: &Path, base_ref: &str) -> Result<String> {
    if config_value(cwd, "remote.origin.url").is_none() {
        return Ok(base_ref.to_owned());
    }
    fetch(cwd, "origin", None).await?;
    let remote = format!("refs/remotes/origin/{base_ref}");
    Ok(stdout(
        cwd,
        &["rev-parse", "--verify", &format!("{remote}^{{commit}}")],
    )
    .filter(|commit| !commit.is_empty())
    .unwrap_or_else(|| base_ref.to_owned()))
}

/// Fetches a pull request's head into `FETCH_HEAD` and names its commit, for
/// a head whose branch is checked out somewhere.
pub(crate) async fn fetch_pull_request_head_commit(cwd: &Path, number: u64) -> Result<String> {
    let remote = primary_remote(cwd).ok_or_else(|| {
        failed(
            "GitVcsDriver.resolvePrimaryRemoteName",
            cwd,
            "No git remote is configured for this repository.",
        )
    })?;
    fetch(cwd, &remote, Some(&format!("refs/pull/{number}/head")))
        .await
        .map_err(|_| {
            failed(
                "GitVcsDriver.fetchPullRequestHeadCommit",
                cwd,
                "git fetch pull request head failed",
            )
        })?;
    resolve_commit(cwd, "FETCH_HEAD")
}

pub(crate) fn resolve_commit(cwd: &Path, revision: &str) -> Result<String> {
    stdout(
        cwd,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .filter(|sha| !sha.is_empty())
    .ok_or_else(|| failed("GitVcsDriver.resolveCommit", cwd, "git rev-parse failed"))
}

pub(crate) fn set_branch_upstream(
    cwd: &Path,
    branch: &str,
    remote: &str,
    remote_branch: &str,
) -> Result<()> {
    run_git(
        cwd,
        "GitVcsDriver.setBranchUpstream",
        &[
            "branch",
            "--set-upstream-to",
            &format!("{remote}/{remote_branch}"),
            "--",
            branch,
        ],
        "git branch --set-upstream-to failed",
    )
    .map(|_| ())
}

/// What moving the checked-out branch onto a commit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refreshed {
    pub moved: bool,
    pub on_target: bool,
}

/// Moves the branch checked out in `cwd` onto `target`: fast-forwarding a
/// clean tree, or resetting it when HEAD still sits where the upstream was
/// before the fetch (so a rewritten head is taken without losing work).
pub(crate) fn refresh_checked_out_branch(
    cwd: &Path,
    target: &str,
    reset_when_head: Option<&str>,
) -> Result<Refreshed> {
    let head = resolve_commit(cwd, "HEAD")?;
    if head == target {
        return Ok(Refreshed {
            moved: false,
            on_target: true,
        });
    }
    let dirty = stdout(cwd, &["status", "--porcelain"]).is_some_and(|out| !out.trim().is_empty());
    if dirty {
        return Ok(Refreshed {
            moved: false,
            on_target: false,
        });
    }
    let is_ancestor = git(
        cwd,
        &["merge-base", "--is-ancestor", &head, target],
        Options::default(),
    )
    .is_ok_and(|run| run.ok());
    if !is_ancestor && Some(head.as_str()) != reset_when_head {
        return Ok(Refreshed {
            moved: false,
            on_target: false,
        });
    }
    let args: &[&str] = if is_ancestor {
        &["merge", "--ff-only", "--", target]
    } else {
        &["reset", "--hard", "--", target]
    };
    run_git(
        cwd,
        "GitVcsDriver.refreshCheckedOutBranch.move",
        args,
        "git could not move the checked-out branch",
    )?;
    Ok(Refreshed {
        moved: true,
        on_target: true,
    })
}

/// The worktree that has `branch` checked out, from `git worktree list`.
pub(crate) fn worktree_of_branch(cwd: &Path, branch: &str) -> Option<PathBuf> {
    let list = stdout(cwd, &["worktree", "list", "--porcelain", "-z"])?;
    let (mut path, mut found) = (None::<String>, false);
    for field in list.split('\0') {
        if field.is_empty() {
            if found && let Some(path) = path.take() {
                return Some(PathBuf::from(path));
            }
            path = None;
            found = false;
        } else if let Some(value) = field.strip_prefix("worktree ") {
            path = Some(value.to_owned());
        } else if field.strip_prefix("branch refs/heads/") == Some(branch) {
            found = true;
        }
    }
    None
}

/// Resolves `path`, or keeps it when it does not exist.
pub(crate) fn canonical_path(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}
