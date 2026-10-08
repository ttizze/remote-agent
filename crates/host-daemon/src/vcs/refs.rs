//! The base picker's branches, checking a branch out and creating one.
use super::{Options, Run, git, ref_exists, remote_names, repository, stdout};
use agent_protocol::workspace::{
    CreateRef, ListRefs, RefKind, RefList, SwitchRef, SwitchedRef, VcsRef,
};
use anyhow::{Result, anyhow};
use std::{collections::HashMap, path::Path};

const DEFAULT_LIST_LIMIT: u32 = 100;

/// The base picker's branches; current and default lead, then newest commit.
pub(super) fn list_refs(request: &ListRefs) -> Result<RefList> {
    let cwd = Path::new(&request.cwd);
    let not_repository = RefList {
        refs: vec![],
        is_repo: false,
        has_primary_remote: false,
        next_cursor: None,
        total_count: 0,
    };
    if !cwd.is_dir() {
        return Ok(not_repository);
    }
    let Some(repository) = repository(cwd) else {
        return Ok(not_repository);
    };
    let git_dir = repository.common_dir.to_string_lossy().into_owned();
    let with_dir = |args: &[&str]| -> Vec<String> {
        ["--git-dir", git_dir.as_str()]
            .iter()
            .chain(args)
            .map(|arg| (*arg).to_owned())
            .collect()
    };
    let run = |args: Vec<String>| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git(cwd, &args, Options::default())
    };
    let refs = run(with_dir(&[
        "for-each-ref",
        "--format=%(refname)%09%(committerdate:unix)%09%(symref)",
        "refs/heads",
        "refs/remotes",
    ]))?;
    if !refs.ok() {
        return Err(anyhow!("Git ref snapshot enumeration failed."));
    }
    let default = run(with_dir(&["symbolic-ref", "refs/remotes/origin/HEAD"]))
        .ok()
        .filter(Run::ok)
        .map(|run| {
            let text = run.text();
            text.trim()
                .strip_prefix("refs/remotes/origin/")
                .unwrap_or(text.trim())
                .to_owned()
        });
    let mut worktrees: HashMap<String, String> = HashMap::new();
    if let Ok(list) = run(with_dir(&["worktree", "list", "--porcelain", "-z"]))
        && list.ok()
    {
        let (mut path, mut branch, mut prunable) = (None, None, false);
        for field in list.text().split('\0') {
            if field.is_empty() {
                if let (Some(path), Some(branch), false) = (path.take(), branch.take(), prunable)
                    && Path::new(&path).exists()
                {
                    worktrees.insert(branch, path);
                }
                prunable = false;
            } else if let Some(value) = field.strip_prefix("worktree ") {
                path = Some(value.to_owned());
            } else if let Some(value) = field.strip_prefix("branch refs/heads/") {
                branch = Some(value.to_owned());
            } else if field == "prunable" || field.starts_with("prunable ") {
                prunable = true;
            }
        }
    }
    let remotes = remote_names(cwd);
    let mut local: Vec<(i64, VcsRef)> = vec![];
    let mut remote: Vec<(i64, VcsRef)> = vec![];
    for line in refs.text().lines() {
        let mut fields = line.split('\t');
        let (Some(full), time, symbolic) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if full.is_empty() || symbolic.is_some_and(|target| !target.is_empty()) {
            continue;
        }
        let time = time.and_then(|time| time.parse().ok()).unwrap_or(0);
        if let Some(name) = full.strip_prefix("refs/heads/") {
            local.push((
                time,
                VcsRef {
                    name: name.into(),
                    is_remote: false,
                    remote_name: None,
                    current: false,
                    is_default: Some(name) == default.as_deref(),
                    worktree_path: worktrees.get(name).cloned(),
                },
            ));
        } else if let Some(name) = full.strip_prefix("refs/remotes/") {
            let parsed = remotes.iter().find_map(|remote| {
                let branch = name.strip_prefix(&format!("{remote}/"))?.trim();
                (!branch.is_empty()).then(|| (remote.clone(), branch.to_owned()))
            });
            remote.push((
                time,
                VcsRef {
                    name: name.into(),
                    is_remote: true,
                    is_default: parsed.as_ref().is_some_and(|(remote, branch)| {
                        remote == "origin" && Some(branch.as_str()) == default.as_deref()
                    }),
                    remote_name: parsed.map(|(remote, _)| remote),
                    current: false,
                    worktree_path: None,
                },
            ));
        }
    }
    let recent =
        |a: &(i64, VcsRef), b: &(i64, VcsRef)| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name));
    local.sort_by(recent);
    remote.sort_by(recent);
    let root = repository.root.to_string_lossy().into_owned();
    let in_worktree = local
        .iter()
        .any(|(_, entry)| entry.worktree_path.as_deref() == Some(root.as_str()));
    let mut local: Vec<VcsRef> = local.into_iter().map(|(_, entry)| entry).collect();
    for entry in &mut local {
        entry.current = if in_worktree {
            entry.worktree_path.as_deref() == Some(root.as_str())
        } else {
            Some(entry.name.as_str()) == repository.branch.as_deref()
        };
    }
    let local_names: Vec<String> = local.iter().map(|entry| entry.name.clone()).collect();
    let mut all: Vec<VcsRef> = local;
    all.extend(remote.into_iter().map(|(_, entry)| entry).filter(|entry| {
        if request.include_matching_remote_refs || entry.remote_name.as_deref() != Some("origin") {
            return true;
        }
        let first_slash = entry
            .name
            .find('/')
            .filter(|index| *index > 0 && *index < entry.name.len() - 1)
            .map_or(entry.name.as_str(), |index| &entry.name[index + 1..]);
        let without_remote = entry
            .name
            .strip_prefix("origin/")
            .filter(|rest| !rest.is_empty());
        ![Some(first_slash), without_remote]
            .into_iter()
            .flatten()
            .any(|candidate| local_names.iter().any(|name| name == candidate))
    }));
    all.sort_by_key(|entry| {
        if entry.current {
            0
        } else if entry.is_default {
            1
        } else {
            2
        }
    });
    let query = request
        .query
        .as_deref()
        .map(str::to_lowercase)
        .filter(|q| !q.is_empty());
    let filtered: Vec<VcsRef> = all
        .into_iter()
        .filter(|entry| match request.ref_kind {
            RefKind::All => true,
            RefKind::Local => !entry.is_remote,
            RefKind::Remote => entry.is_remote,
        })
        .filter(|entry| {
            query
                .as_deref()
                .is_none_or(|query| entry.name.to_lowercase().contains(query))
        })
        .collect();
    let cursor = request.cursor.unwrap_or(0) as usize;
    let limit = request.limit.unwrap_or(DEFAULT_LIST_LIMIT) as usize;
    let total = filtered.len();
    let page: Vec<VcsRef> = filtered.into_iter().skip(cursor).take(limit).collect();
    let next = (cursor + page.len() < total).then(|| (cursor + page.len()) as u32);
    Ok(RefList {
        refs: page,
        is_repo: true,
        has_primary_remote: remotes.iter().any(|name| name == "origin"),
        next_cursor: next,
        total_count: total as u32,
    })
}
/// The local branch that tracks `upstream`.
fn tracking_branch(cwd: &Path, upstream: &str) -> Option<String> {
    stdout(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(upstream:short)",
            "refs/heads",
        ],
    )?
    .lines()
    .filter_map(|line| line.trim().split_once('\t'))
    .map(|(branch, upstream)| (branch.trim(), upstream.trim()))
    .find(|(branch, candidate)| !branch.is_empty() && *candidate == upstream)
    .map(|(branch, _)| branch.to_owned())
}

/// Checks out a branch: an existing local one, the local branch tracking a
/// remote one, or a new local branch tracking it.
pub(super) fn checkout(request: &SwitchRef) -> Result<SwitchedRef> {
    let cwd = Path::new(&request.cwd);
    let name = request.ref_name.as_str();
    let local = ref_exists(cwd, &format!("refs/heads/{name}"));
    let remote = ref_exists(cwd, &format!("refs/remotes/{name}"));
    let tracking = remote.then(|| tracking_branch(cwd, name)).flatten();
    let tracked_name = name
        .split_once('/')
        .map(|(_, branch)| branch.trim())
        .filter(|branch| !branch.is_empty());
    let tracked_exists = remote
        && tracked_name.is_some_and(|branch| ref_exists(cwd, &format!("refs/heads/{branch}")));
    let mut args = match (local, remote, &tracking) {
        (true, _, _) => vec!["checkout", name],
        (false, true, None) if tracked_exists => vec!["checkout", name],
        (false, true, None) => vec!["checkout", "--track", name],
        (false, true, Some(branch)) => vec!["checkout", branch.as_str()],
        (false, false, _) => vec!["checkout", name],
    };
    // A stale ref must not turn into a path checkout that discards local edits.
    args.push("--");
    let run = git(cwd, &args, Options::default())?;
    if !run.ok() {
        return Err(anyhow!(
            "Git command failed in GitVcsDriver.switchRef.checkout ({}): git checkout failed",
            request.cwd
        ));
    }
    Ok(SwitchedRef {
        ref_name: stdout(cwd, &["branch", "--show-current"]).filter(|name| !name.is_empty()),
    })
}

pub(crate) async fn switch_ref(request: SwitchRef) -> Result<SwitchedRef> {
    if request.ref_name.trim().is_empty() || request.ref_name.starts_with('-') {
        return Err(anyhow!("invalid ref name"));
    }
    tokio::task::spawn_blocking(move || checkout(&request)).await?
}

/// Creates a branch at HEAD, then checks it out when asked.
pub(super) fn create(request: &CreateRef) -> Result<SwitchedRef> {
    let cwd = Path::new(&request.cwd);
    let run = git(
        cwd,
        &["branch", "--", &request.ref_name],
        Options::default(),
    )?;
    if !run.ok() {
        return Err(anyhow!(
            "Git command failed in GitVcsDriver.createRef ({}): git branch create failed",
            request.cwd
        ));
    }
    if request.switch_ref {
        checkout(&SwitchRef {
            cwd: request.cwd.clone(),
            ref_name: request.ref_name.clone(),
        })?;
    }
    Ok(SwitchedRef {
        ref_name: Some(request.ref_name.clone()),
    })
}

pub(crate) async fn create_ref(request: CreateRef) -> Result<SwitchedRef> {
    if request.ref_name.trim().is_empty() || request.ref_name.starts_with('-') {
        return Err(anyhow!("invalid ref name"));
    }
    tokio::task::spawn_blocking(move || create(&request)).await?
}

/// The local branch names, as `git branch --list` prints them.
pub(crate) fn list_local_branch_names(cwd: &Path) -> Result<Vec<String>> {
    let run = git(
        cwd,
        &[
            "branch",
            "--list",
            "--no-column",
            "--format=%(refname:short)",
        ],
        Options::default(),
    )?;
    if !run.ok() {
        return Err(anyhow!("Could not list the local branches."));
    }
    Ok(run
        .text()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

pub(crate) async fn refs(request: ListRefs) -> Result<RefList> {
    if request
        .limit
        .is_some_and(|limit| !(1..=200).contains(&limit))
        || request
            .query
            .as_deref()
            .is_some_and(|query| query.chars().count() > 256)
    {
        return Err(anyhow!("invalid ref list request"));
    }
    tokio::task::spawn_blocking(move || list_refs(&request)).await?
}
