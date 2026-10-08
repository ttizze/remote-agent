//! The checkout's status in two halves: what `git status` and the local refs
//! say, and what the upstream says once it is fetched.
use super::{
    BASE_CANDIDATES, Options, base_branch_with, config_value, default_branch, diff, git,
    primary_remote, remote_names, repository, split_remote_ref, stdout, unborn_head,
};
use crate::vcs::process::{Execute, NON_INTERACTIVE_ENV, execute};
use agent_protocol::models::RepositoryIdentity;
use agent_protocol::vcs::{SourceControlProviderInfo, SourceControlProviderKind, VcsStatusLocal};
use agent_protocol::workspace::{FileChangeTotals, WorkingTreeChanges};
use anyhow::{Result, anyhow};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Background fetches of one upstream are this far apart.
const UPSTREAM_REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const UPSTREAM_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);
const UPSTREAM_REFRESH_FAILURE_BASE_COOLDOWN: Duration = Duration::from_secs(30);
const UPSTREAM_REFRESH_FAILURE_MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// The checked-out branch, its upstream and the repository they belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchHead {
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    /// `remote/branch`.
    pub upstream: Option<String>,
    /// The branch's remote, else `origin`.
    pub remote: Option<String>,
    pub repository_identity: Option<RepositoryIdentity>,
}

/// The checkout's head and the repository its remote points at.
pub(crate) fn branch_head(cwd: &Path) -> Option<BranchHead> {
    let repository = repository(cwd)?;
    let upstream = current_upstream(cwd);
    let remote = upstream
        .as_ref()
        .map(|upstream| upstream.remote.clone())
        .or_else(|| {
            repository
                .branch
                .as_deref()
                .and_then(|branch| config_value(cwd, &format!("branch.{branch}.remote")))
        })
        .or_else(|| primary_remote(cwd));
    let repository_identity = remote.as_deref().and_then(|remote| {
        let url = config_value(cwd, &format!("remote.{remote}.url"))?;
        Some(crate::repository::identity(
            remote,
            &url,
            &repository.root.to_string_lossy(),
        ))
    });
    Some(BranchHead {
        branch: repository.branch,
        upstream: upstream.map(|upstream| upstream.reference),
        remote,
        repository_identity,
    })
}

/// The hosting provider a remote URL points at.
pub(crate) fn detect_provider(remote_url: &str) -> Option<SourceControlProviderInfo> {
    let host = crate::repository::remote_host(remote_url)?;
    let hostname = host
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host.as_str(), |(name, _)| name);
    let base_url = |host: &str| format!("https://{host}");
    let (kind, name) = match crate::repository::provider_kind(remote_url)? {
        "forgejo" => {
            let trimmed = remote_url.trim();
            let origin = if trimmed.to_ascii_lowercase().starts_with("http") {
                url::Url::parse(trimmed)
                    .ok()
                    .map(|url| url.origin().ascii_serialization())
                    .unwrap_or_else(|| base_url(&host))
            } else {
                base_url(&host)
            };
            return Some(SourceControlProviderInfo {
                kind: SourceControlProviderKind::Forgejo,
                name: "Forgejo".into(),
                base_url: origin,
            });
        }
        "github" => (
            SourceControlProviderKind::Github,
            if hostname == "github.com" {
                "GitHub"
            } else {
                "GitHub Self-Hosted"
            },
        ),
        "gitlab" => (
            SourceControlProviderKind::Gitlab,
            if hostname == "gitlab.com" {
                "GitLab"
            } else {
                "GitLab Self-Hosted"
            },
        ),
        "azure-devops" => (SourceControlProviderKind::AzureDevops, "Azure DevOps"),
        "bitbucket" => (
            SourceControlProviderKind::Bitbucket,
            if hostname == "bitbucket.org" {
                "Bitbucket"
            } else {
                "Bitbucket Self-Hosted"
            },
        ),
        _ => {
            return Some(SourceControlProviderInfo {
                kind: SourceControlProviderKind::Unknown,
                name: host.clone(),
                base_url: base_url(&host),
            });
        }
    };
    Some(SourceControlProviderInfo {
        kind,
        name: name.into(),
        base_url: base_url(&host),
    })
}

/// The provider of the branch's remote, else origin's.
fn hosting_provider(cwd: &Path, branch: Option<&str>) -> Option<SourceControlProviderInfo> {
    let preferred = branch
        .and_then(|branch| config_value(cwd, &format!("branch.{branch}.remote")))
        .or_else(|| primary_remote(cwd))
        .unwrap_or_else(|| "origin".into());
    let url = config_value(cwd, &format!("remote.{preferred}.url"))
        .or_else(|| config_value(cwd, "remote.origin.url"))?;
    detect_provider(&url)
}

/// The path of one `--porcelain=2` entry.
fn porcelain_path(line: &str) -> Option<String> {
    if let Some(path) = line.strip_prefix("? ").or_else(|| line.strip_prefix("! ")) {
        return Some(path.trim().to_owned());
    }
    let fields: usize = match line.chars().next()? {
        '1' => 8,
        '2' => 9,
        'u' => 10,
        _ => return None,
    };
    let path = line.splitn(fields + 1, ' ').nth(fields)?;
    Some(path.split('\t').next().unwrap_or(path).trim().to_owned())
}

/// Insertions and deletions by path from `--numstat` lines; a rename counts
/// toward its new path.
fn numstat_totals(numstat: &str) -> BTreeMap<String, (u64, u64)> {
    let mut totals: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = path.trim();
        let path = match path.split_once(" => ") {
            Some((_, renamed)) if !renamed.trim().is_empty() => renamed.trim(),
            _ => path,
        };
        let entry = totals.entry(path.to_owned()).or_default();
        entry.0 += added.parse::<u64>().unwrap_or(0);
        entry.1 += deleted.parse::<u64>().unwrap_or(0);
    }
    totals
}

fn is_default(branch: Option<&str>, default: Option<&str>) -> bool {
    branch.is_some_and(|name| {
        Some(name) == default || (default.is_none() && BASE_CANDIDATES.contains(&name))
    })
}

/// What `git status` and the local refs say; the Changes totals are read when
/// asked (they walk history).
pub(crate) fn local_status(cwd: &Path, include_branch_changes: bool) -> Result<VcsStatusLocal> {
    if !cwd.is_dir() {
        return Ok(VcsStatusLocal::not_repository());
    }
    if let Some(index) = stdout(cwd, &["rev-parse", "--git-path", "index"])
        && cwd.join(format!("{index}.lock")).exists()
    {
        return Err(anyhow!(
            "Git index is locked. Status will resume when the index lock is removed."
        ));
    }
    let porcelain = git(
        cwd,
        &["status", "--porcelain=2", "--branch", "--no-ahead-behind"],
        Options::default(),
    )?;
    if !porcelain.ok() {
        if super::not_a_repository(&porcelain.stderr) {
            return Ok(VcsStatusLocal::not_repository());
        }
        return Err(anyhow!("Git status failed."));
    }
    let mut ref_name = None;
    let mut changed = vec![];
    let text = porcelain.text();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            let head = head.trim();
            ref_name = (!head.starts_with('(')).then(|| head.to_owned());
        } else if !line.trim().is_empty() && !line.starts_with('#') {
            changed.push(porcelain_path(line));
        }
    }
    let numstat = git(
        cwd,
        &["diff", "HEAD", "--numstat", "--"],
        Options::default(),
    )?;
    let numstat = if numstat.ok() {
        numstat.text()
    } else if unborn_head(&numstat.stderr) {
        let staged = stdout(cwd, &["diff", "--cached", "--numstat"]).unwrap_or_default();
        let unstaged = stdout(cwd, &["diff", "--numstat"]).unwrap_or_default();
        format!("{staged}\n{unstaged}")
    } else {
        return Err(anyhow!("git diff HEAD --numstat failed."));
    };
    let mut totals = numstat_totals(&numstat);
    for path in changed.into_iter().flatten() {
        totals.entry(path).or_default();
    }
    let files: Vec<FileChangeTotals> = totals
        .into_iter()
        .map(|(path, (insertions, deletions))| FileChangeTotals {
            path,
            insertions,
            deletions,
        })
        .collect();
    let primary = primary_remote(cwd);
    let default = primary
        .as_deref()
        .and_then(|remote| default_branch(cwd, remote));
    let root = repository(cwd).map(|repository| repository.root);
    let branch_changes = include_branch_changes
        .then(|| diff::branch_changes(root.as_deref().unwrap_or(cwd), ref_name.as_deref()).ok())
        .flatten();
    Ok(VcsStatusLocal {
        is_repo: true,
        source_control_provider: hosting_provider(cwd, ref_name.as_deref()),
        has_primary_remote: primary.is_some(),
        is_default_ref: is_default(ref_name.as_deref(), default.as_deref()),
        has_working_tree_changes: text
            .lines()
            .any(|line| !line.starts_with('#') && !line.trim().is_empty()),
        working_tree: WorkingTreeChanges {
            insertions: files.iter().map(|file| file.insertions).sum(),
            deletions: files.iter().map(|file| file.deletions).sum(),
            files,
        },
        branch_changes,
        ref_name,
    })
}

/// The branch's upstream: `remote/branch` split at the remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Upstream {
    pub reference: String,
    pub remote: String,
    pub branch: String,
}

pub(super) fn current_upstream(cwd: &Path) -> Option<Upstream> {
    let reference = stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    if reference.is_empty() || reference == "@{upstream}" {
        return None;
    }
    let (remote, branch) = split_remote_ref(&reference, &remote_names(cwd)).or_else(|| {
        let index = reference.find('/')?;
        let (remote, branch) = (reference[..index].trim(), reference[index + 1..].trim());
        (!remote.is_empty() && !branch.is_empty()).then(|| (remote.to_owned(), branch.to_owned()))
    })?;
    Some(Upstream {
        reference,
        remote,
        branch,
    })
}

/// Commits on HEAD that the base does not have; zero without a base.
pub(super) fn ahead_count_against_base(cwd: &Path, branch: &str) -> u64 {
    let Some(base) = base_branch_with(cwd, branch, false) else {
        return 0;
    };
    stdout(cwd, &["rev-list", "--count", &format!("{base}..HEAD")])
        .and_then(|count| count.parse().ok())
        .unwrap_or(0)
}

/// What the upstream says about the checked-out branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteDetails {
    pub default_branch: Option<String>,
    pub is_default_branch: bool,
    pub branch: Option<String>,
    pub upstream_ref: Option<String>,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    pub ahead_of_default_count: u64,
}

/// `None` outside a repository.
pub(crate) fn remote_details(cwd: &Path) -> Result<Option<RemoteDetails>> {
    if !cwd.is_dir() {
        return Ok(None);
    }
    let head = git(
        cwd,
        &["rev-parse", "--abbrev-ref", "HEAD"],
        Options::default(),
    )?;
    let branch = if head.ok() {
        let value = head.text().trim().to_owned();
        (!value.is_empty() && value != "HEAD").then_some(value)
    } else if super::not_a_repository(&head.stderr) {
        return Ok(None);
    } else if unborn_head(&head.stderr) {
        stdout(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).filter(|b| !b.is_empty())
    } else {
        return Err(anyhow!("Git branch lookup failed."));
    };
    let upstream = current_upstream(cwd);
    let (mut ahead_count, mut behind_count) = (0, 0);
    if let Some(upstream) = &upstream {
        if let Some(counts) = stdout(
            cwd,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("HEAD...{}", upstream.reference),
            ],
        ) {
            let mut parts = counts.split_whitespace();
            ahead_count = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
            behind_count = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        }
    } else if let Some(branch) = &branch {
        ahead_count = ahead_count_against_base(cwd, branch);
    }
    let default = primary_remote(cwd)
        .as_deref()
        .and_then(|remote| default_branch(cwd, remote));
    let is_default_branch = is_default(branch.as_deref(), default.as_deref());
    let ahead_of_default_count = match &branch {
        Some(branch) if !is_default_branch => {
            if upstream.is_none() {
                ahead_count
            } else {
                ahead_count_against_base(cwd, branch)
            }
        }
        _ => 0,
    };
    Ok(Some(RemoteDetails {
        default_branch: default,
        is_default_branch,
        branch,
        upstream_ref: upstream.as_ref().map(|u| u.reference.clone()),
        has_upstream: upstream.is_some(),
        ahead_count,
        behind_count,
        ahead_of_default_count,
    }))
}

/// Both halves without a fetch or a pull request lookup.
#[cfg(test)]
pub(super) fn status(cwd: &Path) -> Result<agent_protocol::workspace::VcsStatus> {
    use agent_protocol::workspace::VcsStatus;
    let local = local_status(cwd, true)?;
    let remote = remote_details(cwd)?.map(|details| agent_protocol::vcs::VcsStatusRemote {
        has_upstream: details.has_upstream,
        ahead_count: details.ahead_count,
        behind_count: details.behind_count,
        ahead_of_default_count: Some(details.ahead_of_default_count),
        pr: None,
    });
    Ok(VcsStatus::merge(local, remote))
}

/// The cooldown after `failures` failed fetches of one upstream in a row.
pub(super) fn upstream_refresh_failure_cooldown(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    UPSTREAM_REFRESH_FAILURE_BASE_COOLDOWN
        .saturating_mul(1u32 << exponent)
        .min(UPSTREAM_REFRESH_FAILURE_MAX_COOLDOWN)
}

struct FetchEntry {
    next_due: Instant,
    failures: u32,
}

/// When each upstream (by repository and remote) was last fetched for the
/// status, so repeated status reads do not fetch again within the interval
/// and a failing remote backs off.
#[derive(Default)]
pub(crate) struct UpstreamFetches {
    entries: Mutex<HashMap<(PathBuf, String), FetchEntry>>,
}

impl UpstreamFetches {
    /// Fetches the checked-out branch's upstream when its last fetch is older
    /// than the interval. A failure keeps the last fetched refs.
    pub(crate) async fn refresh_if_stale(&self, cwd: &Path) {
        let cwd = cwd.to_owned();
        let Some((common_dir, upstream)) = tokio::task::spawn_blocking({
            let cwd = cwd.clone();
            move || {
                let upstream = current_upstream(&cwd)?;
                let repository = repository(&cwd)?;
                Some((repository.common_dir, upstream))
            }
        })
        .await
        .ok()
        .flatten() else {
            return;
        };
        let key = (common_dir, upstream.remote.clone());
        let now = Instant::now();
        {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries.get(&key).is_some_and(|entry| entry.next_due > now) {
                return;
            }
        }
        let args = ["fetch", "--quiet", "--no-tags", "--", &upstream.remote];
        let fetched = execute(Execute {
            env: &NON_INTERACTIVE_ENV,
            timeout: Some(UPSTREAM_REFRESH_TIMEOUT),
            ..Execute::new(&cwd, &args)
        })
        .await;
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let failures = entries.get(&key).map_or(0, |entry| entry.failures);
        let entry = match fetched {
            Ok(run) if run.ok() => FetchEntry {
                next_due: Instant::now() + UPSTREAM_REFRESH_INTERVAL,
                failures: 0,
            },
            _ => {
                let failures = failures + 1;
                tracing::warn!(
                    operation = "host.vcs.fetch",
                    remote = %upstream.remote,
                    failures,
                    "background upstream fetch failed"
                );
                FetchEntry {
                    next_due: Instant::now() + upstream_refresh_failure_cooldown(failures),
                    failures,
                }
            }
        };
        if entries.len() >= 2_048 && !entries.contains_key(&key) {
            entries.clear();
        }
        entries.insert(key, entry);
    }
}

/// The remote half with the upstream fetched first when asked.
pub(crate) async fn remote_status(
    cwd: &Path,
    fetches: &UpstreamFetches,
    refresh_upstream: bool,
) -> Result<Option<RemoteDetails>> {
    if refresh_upstream {
        fetches.refresh_if_stale(cwd).await;
    }
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || remote_details(&cwd)).await?
}
