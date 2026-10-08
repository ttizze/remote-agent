//! Git as the clients see it: the checkout's status with its remote half,
//! the branches of the base picker, the `Uncommitted` and `Changes` diffs,
//! pull, the stacked commit / push / PR actions, worktrees and `git init`.
//! `Changes` compares the working tree, untracked files included, with the
//! merge base of HEAD and the base branch.
mod actions;
mod broadcaster;
mod diff;
mod pr_checkout;
pub(crate) mod process;
mod pull;
mod pull_requests;
mod refs;
mod status;
mod worktree_ops;

pub(crate) use actions::start as start_action;
pub(crate) use broadcaster::VcsStatusBroadcaster;
pub(crate) use pr_checkout::{
    prepare as prepare_pull_request_thread, publish, resolve as resolve_pull_request,
};
pub(crate) use pull::pull_current_branch;
pub(crate) use refs::{create_ref, refs, switch_ref};
pub(crate) use status::local_status;
pub(crate) use worktree_ops::{
    create_worktree, delete_local_branch, init_repository, origin_start, remove_worktree,
};

use diff::preview;

use agent_protocol::workspace::{DiffPreview, DiffPreviewResult};
use anyhow::{Context as _, Result, anyhow};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const METADATA_MAX_BYTES: usize = 16 * 1024 * 1024;
const TRUNCATED_MARKER: &str = "\n\n[truncated]";
const BASE_CANDIDATES: [&str; 2] = ["main", "master"];
/// Shared by previews and status totals, so the Changes row matches the view.
const DIFF_ARGS: [&str; 8] = [
    "diff",
    "--find-renames",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--minimal",
    "--src-prefix=a/",
    "--dst-prefix=b/",
];

pub(super) struct Run {
    pub(super) code: i32,
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: String,
}
impl Run {
    pub(super) fn ok(&self) -> bool {
        self.code == 0
    }
    pub(super) fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

#[derive(Default)]
pub(super) struct Options<'a> {
    pub(super) env: Option<&'a [(String, String)]>,
    pub(super) stdin: Option<&'a [u8]>,
}

/// Runs Git to completion for a short read or write; its exit code is the
/// caller's to interpret.
pub(super) fn git(cwd: &Path, args: &[&str], options: Options<'_>) -> Result<Run> {
    let mut command = Command::new("git");
    command
        .arg("--no-optional-locks")
        .args(args)
        .current_dir(cwd)
        .stdin(if options.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in options.env.into_iter().flatten() {
        command.env(key, value);
    }
    let mut child = command.spawn().context("failed to run git")?;
    if let Some(input) = options.stdin {
        child
            .stdin
            .take()
            .context("git input unavailable")?
            .write_all(input)?;
    }
    let output = child.wait_with_output()?;
    Ok(Run {
        code: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The trimmed output of a Git command that succeeded.
pub(super) fn stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    git(cwd, args, Options::default())
        .ok()
        .filter(Run::ok)
        .map(|run| run.text().trim().to_owned())
}

/// A Git command that must succeed; the error names the operation like
/// `GitCommandError`, without Git's output.
pub(super) fn run_git(cwd: &Path, operation: &str, args: &[&str], detail: &str) -> Result<Run> {
    let run = git(cwd, args, Options::default())?;
    if !run.ok() {
        return Err(anyhow!(
            "Git command failed in {operation} ({}): {detail}",
            cwd.display()
        ));
    }
    Ok(run)
}

/// `git config --get`; `None` when unset.
pub(super) fn config_value(cwd: &Path, key: &str) -> Option<String> {
    stdout(cwd, &["config", "--get", key]).filter(|value| !value.is_empty())
}

pub(super) fn unborn_head(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    stderr.contains("bad revision 'head'")
        || (stderr.contains("unknown revision") && stderr.contains("path not in the working tree"))
}

pub(super) fn not_a_repository(stderr: &str) -> bool {
    stderr.to_lowercase().contains("not a git repository")
}

/// The checkout root and its branch; `None` outside a Git checkout.
pub(super) struct Repository {
    pub(super) root: PathBuf,
    pub(super) branch: Option<String>,
    pub(super) common_dir: PathBuf,
}

pub(super) fn repository(cwd: &Path) -> Option<Repository> {
    let common = stdout(cwd, &["rev-parse", "--git-common-dir"])?;
    let root = stdout(cwd, &["rev-parse", "--show-toplevel"])?;
    let branch = stdout(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    let common = Path::new(&common);
    Some(Repository {
        root: PathBuf::from(root),
        branch,
        common_dir: if common.is_absolute() {
            common.to_owned()
        } else {
            cwd.join(common)
        },
    })
}

/// Remote names in Git's order.
pub(super) fn remote_names_in_order(cwd: &Path) -> Vec<String> {
    stdout(cwd, &["remote"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Remote names, longest first so nested names match before their prefixes.
pub(super) fn remote_names(cwd: &Path) -> Vec<String> {
    let mut names = remote_names_in_order(cwd);
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names
}

/// `origin` when it exists, else the first remote.
pub(super) fn primary_remote(cwd: &Path) -> Option<String> {
    let names = remote_names_in_order(cwd);
    if names.iter().any(|name| name == "origin") {
        return Some("origin".into());
    }
    names.into_iter().next()
}

/// The repository and GitHub host selected by the checkout's primary remote.
/// Provider calls that accept a repository scope use this instead of relying
/// on `gh`'s implicit working-directory discovery, which is ambiguous for
/// enterprise hosts and fork checkouts.
pub(super) fn github_scope(cwd: &Path) -> (Option<String>, Option<String>) {
    let remote =
        primary_remote(cwd).and_then(|name| config_value(cwd, &format!("remote.{name}.url")));
    let repository = remote
        .as_deref()
        .and_then(pull_requests::repository_name_with_owner);
    let host = remote.as_deref().and_then(crate::repository::remote_host);
    (repository, host)
}

/// The remote's default branch as `refs/remotes/<remote>/HEAD` records it.
pub(super) fn default_branch(cwd: &Path, remote: &str) -> Option<String> {
    let reference = stdout(
        cwd,
        &["symbolic-ref", &format!("refs/remotes/{remote}/HEAD")],
    )?;
    reference
        .strip_prefix(&format!("refs/remotes/{remote}/"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

pub(super) fn ref_exists(cwd: &Path, reference: &str) -> bool {
    git(
        cwd,
        &["show-ref", "--verify", "--quiet", reference],
        Options::default(),
    )
    .is_ok_and(|run| run.ok())
}

/// `remote/branch` split at the longest remote name it starts with.
pub(super) fn split_remote_ref(reference: &str, remotes: &[String]) -> Option<(String, String)> {
    let reference = reference.trim();
    remotes.iter().find_map(|remote| {
        let branch = reference.strip_prefix(&format!("{remote}/"))?.trim();
        (!branch.is_empty()).then(|| (remote.clone(), branch.to_owned()))
    })
}

/// The branch name of a remote ref: after its remote, else after the first
/// slash (`remoteRefs.ts` extractBranchNameFromRemoteRef).
pub(super) fn branch_of_remote_ref(reference: &str, remotes: &[String]) -> String {
    let reference = reference.trim();
    let reference = reference.strip_prefix("refs/remotes/").unwrap_or(reference);
    if let Some((_, branch)) = split_remote_ref(reference, remotes) {
        return branch;
    }
    match reference.find('/') {
        Some(index) => reference[index + 1..].trim().to_owned(),
        None => reference.to_owned(),
    }
}

/// The branch `Changes` compares with: the configured merge base, the remote's
/// default branch, then `main` or `master`, preferring the remote copy. The
/// current branch compares with its own remote copy.
pub(super) fn base_branch(cwd: &Path, branch: &str) -> Option<String> {
    base_branch_with(cwd, branch, true)
}

/// `base_branch`, where `allow_remote_of_current` lets the current branch
/// compare with its own remote copy (the review diff) instead of skipping its
/// own name (the ahead count).
pub(super) fn base_branch_with(
    cwd: &Path,
    branch: &str,
    allow_remote_of_current: bool,
) -> Option<String> {
    let configured = config_value(cwd, &format!("branch.{branch}.gh-merge-base"));
    let remote = primary_remote(cwd);
    let default = remote
        .as_deref()
        .and_then(|remote| default_branch(cwd, remote));
    let candidates = configured
        .into_iter()
        .chain(default)
        .chain(BASE_CANDIDATES.map(str::to_owned));
    for candidate in candidates {
        let prefix = remote
            .as_deref()
            .filter(|remote| *remote != "origin")
            .map(|remote| format!("{remote}/"));
        let name = candidate
            .strip_prefix("origin/")
            .or_else(|| {
                prefix
                    .as_deref()
                    .and_then(|prefix| candidate.strip_prefix(prefix))
            })
            .unwrap_or(&candidate)
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let remote_copy = remote
            .as_deref()
            .filter(|remote| ref_exists(cwd, &format!("refs/remotes/{remote}/{name}")))
            .map(|remote| format!("{remote}/{name}"));
        if name == branch {
            if allow_remote_of_current && remote_copy.is_some() {
                return remote_copy;
            }
            continue;
        }
        if remote_copy.is_some() {
            return remote_copy;
        }
        if ref_exists(cwd, &format!("refs/heads/{name}")) {
            return Some(name);
        }
    }
    None
}

pub(super) struct MergeBase {
    pub(super) base_ref: Option<String>,
    pub(super) commit: String,
}

/// Without a usable base, `Changes` is the diff against HEAD.
pub(super) fn merge_base(
    cwd: &Path,
    branch: Option<&str>,
    explicit: Option<&str>,
) -> Result<MergeBase> {
    let base = explicit
        .map(str::to_owned)
        .or_else(|| branch.and_then(|branch| base_branch(cwd, branch)));
    let Some(base) = base else {
        return Ok(MergeBase {
            base_ref: None,
            commit: "HEAD".into(),
        });
    };
    let run = git(cwd, &["merge-base", &base, "HEAD"], Options::default())?;
    let commit = run.text().trim().to_owned();
    if run.ok() && !commit.is_empty() {
        return Ok(MergeBase {
            base_ref: Some(base),
            commit,
        });
    }
    let head = git(
        cwd,
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        Options::default(),
    )?;
    if explicit.is_none() && !head.ok() {
        return Ok(MergeBase {
            base_ref: None,
            commit: "HEAD".into(),
        });
    }
    Err(anyhow!(
        "Could not find a common commit between '{base}' and HEAD."
    ))
}

/// The canonical form of a checkout path, for caches keyed by checkout.
pub(super) async fn canonical(cwd: &str) -> String {
    match tokio::fs::canonicalize(cwd).await {
        Ok(path) => dunce::simplified(&path).to_string_lossy().into_owned(),
        Err(_) => cwd.to_owned(),
    }
}

pub(crate) async fn diff_preview(request: DiffPreview) -> Result<DiffPreviewResult> {
    tokio::task::spawn_blocking(move || preview(&request)).await?
}

#[cfg(test)]
mod tests;
