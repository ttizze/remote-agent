//! The pull request of a branch, looked up on GitHub through `gh` and cached
//! on its own cadence: open answers for a minute, settled ones for five, and
//! failures with a backoff, keeping the last known answer when a lookup
//! fails.
use super::{config_value, remote_names, split_remote_ref, stdout};
use crate::github::cli::{
    GhError, GitHubCli, HEAD_BRANCH_PROBE_LIMIT, PullRequestListState, PullRequestRecord,
};
use agent_protocol::vcs::{ChangeRequestState, VcsStatusChangeRequest};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Matches the settlement sweep cadence so every sweep reads fresh state.
const PR_LOOKUP_CACHE_TTL: Duration = Duration::from_secs(60);
/// Answers without an open PR change only when someone opens one; the paths
/// that do that in-app bypass the cache.
const PR_LOOKUP_NO_OPEN_PR_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const PR_LOOKUP_FAILURE_BASE_TTL: Duration = Duration::from_secs(20);
const PR_LOOKUP_FAILURE_MAX_TTL: Duration = Duration::from_secs(15 * 60);
const PR_LOOKUP_CACHE_CAPACITY: usize = 2_048;

/// How long a failed lookup is cached after `failures` consecutive failures:
/// a throttled host must not be asked more often than a healthy one.
pub(crate) fn pr_lookup_failure_ttl(failures: u32) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    PR_LOOKUP_FAILURE_BASE_TTL
        .saturating_mul(1u32 << exponent)
        .min(PR_LOOKUP_FAILURE_MAX_TTL)
}

/// `owner/repo` (or a nested path) from a remote URL.
pub(crate) fn repository_name_with_owner(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = if let Some((_, rest)) = trimmed
        .split_once("://")
        .filter(|(scheme, _)| matches!(scheme.to_ascii_lowercase().as_str(), "ssh" | "http" | "https" | "git"))
    {
        rest.split_once('/').map(|(_, path)| path)?
    } else if let Some((user_host, path)) = trimmed.split_once(':')
        && user_host.contains('@')
        && !user_host.contains('/')
    {
        path
    } else {
        return None;
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    (segments.len() >= 2).then(|| segments.join("/"))
}

fn owner_login(name_with_owner: Option<&str>) -> Option<String> {
    name_with_owner?
        .split('/')
        .next()
        .map(str::trim)
        .filter(|owner| !owner.is_empty())
        .map(str::to_owned)
}

/// What a lookup compares pull request heads with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeadContext {
    pub local_branch: String,
    pub head_branch: String,
    pub head_selectors: Vec<String>,
    pub preferred_head_selector: String,
    pub remote_name: Option<String>,
    pub head_remote_url_key: Option<String>,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
    pub is_cross_repository: bool,
}

struct RemoteRepository {
    url_key: Option<String>,
    name_with_owner: Option<String>,
    owner_login: Option<String>,
}

fn remote_repository(cwd: &Path, remote: Option<&str>) -> RemoteRepository {
    let Some(remote) = remote else {
        return RemoteRepository {
            url_key: None,
            name_with_owner: None,
            owner_login: None,
        };
    };
    let url = config_value(cwd, &format!("remote.{remote}.url"));
    let name_with_owner = url.as_deref().and_then(repository_name_with_owner);
    RemoteRepository {
        url_key: url
            .as_deref()
            .map(agent_runtime::normalize_remote_url),
        owner_login: owner_login(name_with_owner.as_deref()),
        name_with_owner,
    }
}

fn push_unique(values: &mut Vec<String>, next: Option<String>) {
    if let Some(next) = next.map(|value| value.trim().to_owned())
        && !next.is_empty()
        && !values.contains(&next)
    {
        values.push(next);
    }
}

/// GitManager's resolveBranchHeadContext.
pub(crate) fn branch_head_context(
    cwd: &Path,
    branch: &str,
    upstream_ref: Option<&str>,
    remote_name: Option<String>,
) -> HeadContext {
    let remote_name =
        remote_name.or_else(|| config_value(cwd, &format!("branch.{branch}.remote")));
    let remotes = remote_names(cwd);
    let head_from_upstream = upstream_ref
        .map(|upstream| {
            let scoped: Vec<String> = remote_name.iter().cloned().collect();
            super::branch_of_remote_ref(upstream, if scoped.is_empty() { &remotes } else { &scoped })
        })
        .unwrap_or_default();
    let head_branch = if head_from_upstream.is_empty() {
        branch.to_owned()
    } else {
        head_from_upstream.clone()
    };
    let probe_local = head_from_upstream.is_empty() || head_branch == branch;
    let remote = remote_repository(cwd, remote_name.as_deref());
    let origin = remote_repository(cwd, Some("origin"));
    let is_cross_repository = match (&remote.name_with_owner, &origin.name_with_owner) {
        (Some(remote), Some(origin)) => !remote.eq_ignore_ascii_case(origin),
        _ => {
            remote_name.as_deref().is_some_and(|name| name != "origin")
                && remote.name_with_owner.is_some()
        }
    };
    let owner_selector = remote
        .owner_login
        .as_deref()
        .filter(|_| !head_branch.is_empty())
        .map(|owner| format!("{owner}:{head_branch}"));
    let alias_selector = remote_name
        .as_deref()
        .filter(|_| !head_branch.is_empty())
        .map(|remote| format!("{remote}:{head_branch}"));
    let probe_remote_owned =
        is_cross_repository || remote_name.as_deref().is_some_and(|name| name != "origin");
    let mut selectors = vec![];
    if is_cross_repository && probe_remote_owned {
        push_unique(&mut selectors, owner_selector.clone());
        if alias_selector != owner_selector {
            push_unique(&mut selectors, alias_selector.clone());
        }
    }
    if probe_local {
        push_unique(&mut selectors, Some(branch.to_owned()));
    }
    if head_branch != branch {
        push_unique(&mut selectors, Some(head_branch.clone()));
    }
    if !is_cross_repository && probe_remote_owned {
        push_unique(&mut selectors, owner_selector.clone());
        if alias_selector != owner_selector {
            push_unique(&mut selectors, alias_selector);
        }
    }
    HeadContext {
        local_branch: branch.to_owned(),
        preferred_head_selector: match (&owner_selector, is_cross_repository) {
            (Some(selector), true) => selector.clone(),
            _ => head_branch.clone(),
        },
        head_branch,
        head_selectors: selectors,
        head_remote_url_key: remote
            .url_key
            .or_else(|| remote_name.is_none().then(|| origin.url_key.clone()).flatten()),
        remote_name,
        head_repository_name_with_owner: remote.name_with_owner,
        head_repository_owner_login: remote.owner_login,
        is_cross_repository,
    }
}

/// The remote holding a ref named after the local branch, preferring
/// `preferred`, then origin, then the first remote.
fn remote_tracking_remote(cwd: &Path, branch: &str, preferred: Option<&str>) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    let remotes = super::remote_names_in_order(cwd);
    if remotes.is_empty() {
        return None;
    }
    let patterns: Vec<String> = remotes
        .iter()
        .map(|remote| format!("refs/remotes/{remote}/{branch}"))
        .collect();
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(patterns.iter().map(String::as_str));
    let refs: Vec<String> = stdout(cwd, &args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    let matching: Vec<&String> = remotes
        .iter()
        .filter(|remote| refs.contains(&format!("refs/remotes/{remote}/{branch}")))
        .collect();
    if let Some(preferred) = preferred
        && matching.iter().any(|remote| remote.as_str() == preferred)
    {
        return Some(preferred.to_owned());
    }
    if matching.iter().any(|remote| remote.as_str() == "origin") {
        return Some("origin".into());
    }
    matching.first().map(|remote| (*remote).clone())
}

/// The context a lookup uses, and whether to look up at all: a branch cut
/// from the default branch tracks its base, not its published head, so its
/// PR is looked up under its own name on a remote holding it, or not at all.
pub(crate) fn lookup_head_context(
    cwd: &Path,
    branch: &str,
    upstream_ref: Option<&str>,
    default_branch: Option<&str>,
    remote_name: Option<String>,
) -> (HeadContext, bool) {
    let context = branch_head_context(cwd, branch, upstream_ref, remote_name);
    let upstream_is_default = Some(context.head_branch.as_str()) == default_branch
        || (default_branch.is_none() && super::BASE_CANDIDATES.contains(&context.head_branch.as_str()));
    if context.head_branch == branch || !upstream_is_default || context.is_cross_repository {
        return (context, true);
    }
    match remote_tracking_remote(cwd, branch, context.remote_name.as_deref()) {
        None => (context, false),
        Some(remote) => (branch_head_context(cwd, branch, None, Some(remote)), true),
    }
}

/// Git has no record of the branch on any remote, so no PR can exist and the
/// lookup would be a guaranteed-empty call. A repository without any remote
/// ref cannot tell, and keeps the lookup.
pub(crate) fn is_unpublished_branch(cwd: &Path, context: &HeadContext) -> bool {
    if context.head_branch.is_empty() {
        return false;
    }
    let configured_remote = config_value(cwd, &format!("branch.{}.remote", context.local_branch));
    let configured_merge = config_value(cwd, &format!("branch.{}.merge", context.local_branch));
    if configured_remote.is_some() && configured_merge.is_some() {
        return false;
    }
    let matches = |pattern: &str| {
        stdout(
            cwd,
            &["for-each-ref", "--count=1", "--format=%(refname)", pattern],
        )
        .is_some_and(|out| !out.trim().is_empty())
    };
    matches("refs/remotes") && !matches(&format!("refs/remotes/*/{}", context.head_branch))
}

fn normalize_lower(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

/// `owner/repo` of a pull request's URL (`https://host/owner/repo/pull/N`).
fn repository_name_from_pr_url(url: &str) -> Option<String> {
    let url = url::Url::parse(url.trim()).ok()?;
    let segments: Vec<&str> = url.path_segments()?.collect();
    match segments.as_slice() {
        [_, name, "pull", number, ..] if number.chars().all(|c| c.is_ascii_digit()) => {
            Some((*name).to_owned()).filter(|name| !name.is_empty())
        }
        _ => None,
    }
}

fn pr_head_identity(pr: &PullRequestRecord) -> (Option<String>, Option<String>) {
    let name_with_owner = normalize_lower(pr.head_repository_name_with_owner.as_deref()).or_else(|| {
        if pr.is_cross_repository != Some(true) {
            return None;
        }
        let owner = normalize_lower(pr.head_repository_owner_login.as_deref())?;
        let name = repository_name_from_pr_url(&pr.url)?;
        Some(format!("{owner}/{}", name.to_lowercase()))
    });
    let owner = normalize_lower(pr.head_repository_owner_login.as_deref())
        .or_else(|| owner_login(name_with_owner.as_deref()));
    (name_with_owner, owner)
}

/// GitManager's matchesBranchHeadContext: the PR's head branch and
/// repository match the context's.
pub(crate) fn matches_branch_head_context(pr: &PullRequestRecord, context: &HeadContext) -> bool {
    if pr.head_ref_name != context.head_branch {
        return false;
    }
    let expected_repository = normalize_lower(context.head_repository_name_with_owner.as_deref());
    let expected_owner = normalize_lower(context.head_repository_owner_login.as_deref())
        .or_else(|| owner_login(expected_repository.as_deref()));
    let (pr_repository, pr_owner) = pr_head_identity(pr);
    if let Some(expected) = &expected_repository {
        if let Some(actual) = &pr_repository
            && expected != actual
        {
            return false;
        }
        if let (Some(expected), Some(actual)) = (&expected_owner, &pr_owner)
            && expected != actual
        {
            return false;
        }
    }
    if let (Some(expected), Some(actual)) = (&expected_owner, &pr_owner)
        && expected != actual
    {
        return false;
    }
    if context.is_cross_repository {
        if pr.is_cross_repository == Some(false) {
            return false;
        }
        if (expected_repository.is_some() || expected_owner.is_some())
            && pr_repository.is_none()
            && pr_owner.is_none()
        {
            return false;
        }
        return true;
    }
    if pr.is_cross_repository == Some(true)
        && ((expected_repository.is_none() && expected_owner.is_none())
            || (pr_repository.is_none() && pr_owner.is_none()))
    {
        return false;
    }
    true
}

/// `owner:branch` selectors list nothing on GitHub, so probes use the bare
/// branch names and leave the owner check to the head match.
fn probeable_selectors(context: &HeadContext) -> Vec<&str> {
    context
        .head_selectors
        .iter()
        .map(String::as_str)
        .filter(|selector| !selector.contains(':'))
        .collect()
}

pub(crate) fn to_status_pr(pr: &PullRequestRecord) -> VcsStatusChangeRequest {
    VcsStatusChangeRequest {
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        base_ref: pr.base_ref_name.clone(),
        head_ref: pr.head_ref_name.clone(),
        state: pr.state,
        is_draft: pr.is_draft,
        updated_at: pr.updated_at.clone(),
    }
}

/// The error a failed provider call reads as (`SourceControlProviderError`).
pub(crate) fn provider_error(operation: &str, error: &GhError) -> anyhow::Error {
    anyhow!("Source control provider github failed in {operation}: {error}")
}

/// The open PR of the head, probing each selector in order.
pub(crate) async fn find_open_pr(
    github: &GitHubCli,
    cwd: &Path,
    context: &HeadContext,
) -> Result<Option<PullRequestRecord>> {
    for selector in probeable_selectors(context) {
        let rows = github
            .list_pull_requests_by_head(
                cwd,
                selector,
                PullRequestListState::Open,
                HEAD_BRANCH_PROBE_LIMIT,
                None,
                context.head_repository_name_with_owner.as_deref(),
            )
            .await
            .map_err(|error| provider_error("listChangeRequests", &error))?;
        if let Some(pr) = rows
            .into_iter()
            .find(|pr| matches_branch_head_context(pr, context))
        {
            return Ok(Some(PullRequestRecord {
                state: ChangeRequestState::Open,
                updated_at: None,
                ..pr
            }));
        }
    }
    Ok(None)
}

/// The newest PR of the head, open ones first.
async fn find_latest_pr(
    github: &GitHubCli,
    cwd: &Path,
    context: &HeadContext,
) -> Result<Option<PullRequestRecord>> {
    let mut by_number: HashMap<u64, PullRequestRecord> = HashMap::new();
    for selector in probeable_selectors(context) {
        let rows = github
            .list_pull_requests_by_head(
                cwd,
                selector,
                PullRequestListState::All,
                HEAD_BRANCH_PROBE_LIMIT,
                None,
                context.head_repository_name_with_owner.as_deref(),
            )
            .await
            .map_err(|error| provider_error("listChangeRequests", &error))?;
        for pr in rows
            .into_iter()
            .filter(|pr| matches_branch_head_context(pr, context))
        {
            by_number.insert(pr.number, pr);
        }
    }
    let mut all: Vec<PullRequestRecord> = by_number.into_values().collect();
    // Newest activity first; rows without a time last.
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.number.cmp(&a.number)));
    Ok(all
        .iter()
        .find(|pr| pr.state == ChangeRequestState::Open)
        .or(all.first())
        .cloned())
}

/// What a lookup needs to know about the branch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BranchDetails {
    pub branch: String,
    pub upstream_ref: Option<String>,
    pub default_branch: Option<String>,
    pub is_default_branch: bool,
}

#[derive(Clone)]
struct CachedLookup {
    latest: Option<PullRequestRecord>,
    context: HeadContext,
    expires: Instant,
}

#[derive(Clone)]
struct LastKnown {
    pr: Option<VcsStatusChangeRequest>,
    upstream_ref: Option<String>,
    head_branch: String,
    remote_name: Option<String>,
    head_remote_url_key: Option<String>,
}

#[derive(Default)]
struct State {
    lookups: HashMap<String, Result<CachedLookup, Instant>>,
    failure_streaks: HashMap<String, u32>,
    epochs: HashMap<String, u64>,
    last_known: HashMap<String, LastKnown>,
}

/// The pull request lookups of every checkout, with their caches.
pub(crate) struct PullRequestLookup {
    github: Option<GitHubCli>,
    state: Mutex<State>,
}

impl PullRequestLookup {
    pub(crate) fn new(github: Option<GitHubCli>) -> Self {
        Self {
            github,
            state: Mutex::new(State::default()),
        }
    }

    pub(crate) fn github(&self) -> Option<&GitHubCli> {
        self.github.as_ref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn epoch(&self, cwd: &str) -> u64 {
        self.lock().epochs.get(cwd).copied().unwrap_or(0)
    }

    /// Explicit freshness: the next lookup bypasses the cache.
    pub(crate) fn bump_epoch(&self, cwd: &str) {
        let mut state = self.lock();
        *state.epochs.entry(cwd.to_owned()).or_default() += 1;
    }

    fn cache_key(&self, cwd: &str, details: &BranchDetails) -> String {
        [
            cwd,
            details.branch.as_str(),
            details.upstream_ref.as_deref().unwrap_or(""),
            details.default_branch.as_deref().unwrap_or(""),
            &self.epoch(cwd).to_string(),
        ]
        .join("\0")
    }

    /// Drops a cached "no pull request" so the next read asks again, keeping
    /// known PRs and failed lookups' backoff.
    fn forget_missing(&self, key: &str) {
        let mut state = self.lock();
        if matches!(state.lookups.get(key), Some(Ok(cached)) if cached.latest.is_none()) {
            state.lookups.remove(key);
        }
    }

    async fn lookup(
        &self,
        cwd: &Path,
        details: &BranchDetails,
    ) -> Result<(Option<PullRequestRecord>, HeadContext)> {
        let (context, lookup) = tokio::task::spawn_blocking({
            let (cwd, details) = (cwd.to_owned(), details.clone());
            move || {
                lookup_head_context(
                    &cwd,
                    &details.branch,
                    details.upstream_ref.as_deref(),
                    details.default_branch.as_deref(),
                    None,
                )
            }
        })
        .await?;
        if !lookup {
            return Ok((None, context));
        }
        if details.upstream_ref.is_none() {
            let unpublished = tokio::task::spawn_blocking({
                let (cwd, context) = (cwd.to_owned(), context.clone());
                move || is_unpublished_branch(&cwd, &context)
            })
            .await?;
            if unpublished {
                return Ok((None, context));
            }
        }
        let Some(github) = &self.github else {
            return Err(provider_error("listChangeRequests", &GhError::Unavailable));
        };
        let latest = find_latest_pr(github, cwd, &context).await?;
        Ok((latest, context))
    }

    async fn cached_lookup(
        &self,
        cwd: &Path,
        key: &str,
        details: &BranchDetails,
    ) -> Result<(Option<PullRequestRecord>, HeadContext)> {
        let now = Instant::now();
        let cached = self.lock().lookups.get(key).cloned();
        match cached {
            Some(Ok(cached)) if cached.expires > now => return Ok((cached.latest, cached.context)),
            Some(Err(expires)) if expires > now => {
                return Err(anyhow!("PR lookup recently failed; retrying later."));
            }
            _ => {}
        }
        match self.lookup(cwd, details).await {
            Ok((latest, context)) => {
                let mut state = self.lock();
                state.failure_streaks.remove(key);
                let ttl = if latest.as_ref().is_some_and(|pr| pr.state == ChangeRequestState::Open)
                {
                    PR_LOOKUP_CACHE_TTL
                } else {
                    PR_LOOKUP_NO_OPEN_PR_CACHE_TTL
                };
                if state.lookups.len() >= PR_LOOKUP_CACHE_CAPACITY {
                    state.lookups.clear();
                }
                state.lookups.insert(
                    key.to_owned(),
                    Ok(CachedLookup {
                        latest: latest.clone(),
                        context: context.clone(),
                        expires: Instant::now() + ttl,
                    }),
                );
                Ok((latest, context))
            }
            Err(error) => {
                let mut state = self.lock();
                let streak = state.failure_streaks.entry(key.to_owned()).or_default();
                *streak += 1;
                let ttl = pr_lookup_failure_ttl(*streak);
                state
                    .lookups
                    .insert(key.to_owned(), Err(Instant::now() + ttl));
                Err(error)
            }
        }
    }

    /// The branch's PR for the status: the cached answer, the host's, or the
    /// last known one when the lookup fails. On the default branch only open
    /// PRs show; merged ones there are reverse-merge history.
    pub(crate) async fn status_pr(
        &self,
        cwd: &str,
        details: &BranchDetails,
        refresh_missing: bool,
    ) -> Option<VcsStatusChangeRequest> {
        let branch_key = format!("{cwd}\0{}", details.branch);
        let key = self.cache_key(cwd, details);
        if refresh_missing {
            self.forget_missing(&key);
        }
        let path = Path::new(cwd);
        match self.cached_lookup(path, &key, details).await {
            Ok((latest, context)) => {
                let pr = latest
                    .filter(|pr| !(details.is_default_branch && pr.state != ChangeRequestState::Open))
                    .map(|pr| to_status_pr(&pr));
                let mut state = self.lock();
                if state.last_known.len() >= PR_LOOKUP_CACHE_CAPACITY {
                    state.last_known.clear();
                }
                state.last_known.insert(
                    branch_key,
                    LastKnown {
                        pr: pr.clone(),
                        upstream_ref: details.upstream_ref.clone(),
                        head_branch: context.head_branch,
                        remote_name: context.remote_name,
                        head_remote_url_key: context.head_remote_url_key,
                    },
                );
                pr
            }
            Err(error) => {
                tracing::warn!(
                    operation = "lookupStatusPr",
                    branch = %details.branch,
                    detail = %error,
                    "PR lookup failed; keeping last known PR state."
                );
                let context = tokio::task::spawn_blocking({
                    let (cwd, details) = (path.to_owned(), details.clone());
                    move || {
                        lookup_head_context(
                            &cwd,
                            &details.branch,
                            details.upstream_ref.as_deref(),
                            details.default_branch.as_deref(),
                            None,
                        )
                        .0
                    }
                })
                .await
                .ok()?;
                let state = self.lock();
                let last = state.last_known.get(&branch_key)?;
                if last.head_branch != context.head_branch {
                    return None;
                }
                // Both sides must be known before a mismatch counts: a
                // transient failure to read the current URL must not drop a
                // known badge.
                if let (Some(previous), Some(current)) =
                    (&last.head_remote_url_key, &context.head_remote_url_key)
                {
                    return (previous == current).then(|| last.pr.clone()).flatten();
                }
                if let (Some(_), Some(_), Some(previous), Some(current)) = (
                    &last.upstream_ref,
                    &details.upstream_ref,
                    &last.remote_name,
                    &context.remote_name,
                ) {
                    return (previous == current).then(|| last.pr.clone()).flatten();
                }
                last.pr.clone()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(head: &str) -> PullRequestRecord {
        PullRequestRecord {
            number: 1,
            title: "t".into(),
            url: "https://github.com/team/repository/pull/1".into(),
            base_ref_name: "main".into(),
            head_ref_name: head.into(),
            state: ChangeRequestState::Open,
            is_draft: false,
            closed_at: None,
            merged_at: None,
            updated_at: None,
            is_cross_repository: None,
            head_repository_name_with_owner: None,
            head_repository_owner_login: None,
        }
    }

    fn context(head: &str) -> HeadContext {
        HeadContext {
            local_branch: head.into(),
            head_branch: head.into(),
            head_selectors: vec![head.into()],
            preferred_head_selector: head.into(),
            remote_name: Some("origin".into()),
            head_remote_url_key: None,
            head_repository_name_with_owner: None,
            head_repository_owner_login: None,
            is_cross_repository: false,
        }
    }

    // GitManager.test.ts "backs off repeated PR lookup failures past the
    // healthy refresh cadence".
    #[test]
    fn failed_lookups_back_off_past_the_healthy_cadence() {
        assert_eq!(pr_lookup_failure_ttl(1), Duration::from_secs(20));
        assert_eq!(pr_lookup_failure_ttl(2), Duration::from_secs(40));
        assert!(pr_lookup_failure_ttl(4) > Duration::from_secs(120));
        assert_eq!(pr_lookup_failure_ttl(20), Duration::from_secs(900));
    }

    // "distinguishes Enterprise forks with the same head branch", "rejects
    // same-repo PR metadata when matching a cross-repo head context", "accepts
    // fork PR metadata when origin is the fork checkout remote".
    #[test]
    fn head_matching_tells_forks_apart() {
        let mut fork = context("feature");
        fork.is_cross_repository = true;
        fork.head_repository_name_with_owner = Some("alice/repository".into());
        fork.head_repository_owner_login = Some("alice".into());
        let mut bob = record("feature");
        bob.is_cross_repository = Some(true);
        bob.head_repository_name_with_owner = Some("bob/repository".into());
        bob.head_repository_owner_login = Some("bob".into());
        let mut alice = record("feature");
        alice.is_cross_repository = Some(true);
        alice.head_repository_name_with_owner = Some("Alice/Repository".into());
        alice.head_repository_owner_login = Some("Alice".into());
        assert!(!matches_branch_head_context(&bob, &fork));
        assert!(matches_branch_head_context(&alice, &fork));
        let mut same_repo = record("feature");
        same_repo.is_cross_repository = Some(false);
        assert!(!matches_branch_head_context(&same_repo, &fork));
        // Origin is the fork itself: a same-repo context accepts a PR GitHub
        // calls cross-repository as long as the owners agree.
        let mut origin_fork = context("feature");
        origin_fork.head_repository_name_with_owner = Some("alice/repository".into());
        origin_fork.head_repository_owner_login = Some("alice".into());
        assert!(matches_branch_head_context(&alice, &origin_fork));
        assert!(!matches_branch_head_context(&record("other"), &context("feature")));
        let mut unknown_fork = record("feature");
        unknown_fork.is_cross_repository = Some(true);
        assert!(!matches_branch_head_context(&unknown_fork, &context("feature")));
    }

    // "derives fork repository identity from PR URL when GitHub omits
    // nameWithOwner".
    #[test]
    fn a_fork_repository_comes_from_the_url_when_github_omits_it() {
        let mut pr = record("feature");
        pr.is_cross_repository = Some(true);
        pr.head_repository_owner_login = Some("alice".into());
        assert_eq!(
            pr_head_identity(&pr),
            (Some("alice/repository".into()), Some("alice".into()))
        );
        assert_eq!(
            repository_name_with_owner("git@github.com:Team/Repository.git").as_deref(),
            Some("Team/Repository")
        );
        assert_eq!(
            repository_name_with_owner("https://gitlab.com/group/sub/project/").as_deref(),
            Some("group/sub/project")
        );
        assert_eq!(repository_name_with_owner("/local/path"), None);
    }
}
