//! The status every client sees: one cached local and remote half per
//! checkout, a change stream, and a remote poller per checkout while clients
//! subscribe. Local reads are cheap and happen on every refresh; remote reads
//! fetch and ask the host, so they follow the fetch interval, run once per
//! checkout at a time, and back off when they fail.
use super::pull_requests::{BranchDetails, PullRequestLookup};
use super::status::{UpstreamFetches, local_status, remote_status};
use super::canonical;
use crate::github::cli::GitHubCli;
use agent_protocol::vcs::{VcsStatusLocal, VcsStatusRemote, VcsStatusStreamEvent};
use agent_protocol::workspace::VcsStatus;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tokio_util::task::AbortOnDropHandle;

pub(crate) const DEFAULT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const REFRESH_FAILURE_BASE_DELAY: Duration = Duration::from_secs(30);
const REFRESH_FAILURE_MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// Status reads of one checkout within this window share their answer.
const RESULT_CACHE_TTL: Duration = Duration::from_secs(1);

/// The wait after `failures` failed remote refreshes in a row, never shorter
/// than the configured interval.
pub(crate) fn remote_refresh_failure_delay(failures: u32, interval: Duration) -> Duration {
    let exponent = failures.saturating_sub(1).min(16);
    let backoff = REFRESH_FAILURE_BASE_DELAY
        .saturating_mul(1u32 << exponent)
        .min(REFRESH_FAILURE_MAX_DELAY);
    interval.max(backoff)
}

/// One change of a checkout's status.
#[derive(Debug, Clone)]
pub(crate) struct Change {
    pub cwd: String,
    pub event: VcsStatusStreamEvent,
}

#[derive(Default, Clone)]
struct Cached {
    local: Option<VcsStatusLocal>,
    /// `Some(None)` is a read that found no repository.
    remote: Option<Option<VcsStatusRemote>>,
}

struct Poller {
    subscribers: usize,
    _task: AbortOnDropHandle<()>,
}

/// How long a cached result still answers.
struct Recent<T> {
    value: T,
    read_at: Instant,
}

struct Inner {
    lookups: PullRequestLookup,
    fetches: UpstreamFetches,
    cache: Mutex<HashMap<String, Cached>>,
    local_results: Mutex<HashMap<String, Recent<VcsStatusLocal>>>,
    remote_results: Mutex<HashMap<String, Recent<Option<VcsStatusRemote>>>>,
    changes: broadcast::Sender<Change>,
    pollers: Mutex<HashMap<String, Poller>>,
    /// One permit per checkout for remote reads that write the cache, so an
    /// older poll cannot overwrite a fresher explicit refresh.
    write_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    fetch_interval: Arc<dyn Fn() -> Duration + Send + Sync>,
}

#[derive(Clone)]
pub(crate) struct VcsStatusBroadcaster {
    inner: Arc<Inner>,
}

/// Keeps a checkout's remote poller alive while held.
pub(crate) struct Subscription {
    inner: Arc<Inner>,
    cwd: String,
    pub receiver: broadcast::Receiver<Change>,
}

impl Subscription {
    pub(crate) fn cwd(&self) -> &str {
        &self.cwd
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut pollers = self.inner.pollers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(poller) = pollers.get_mut(&self.cwd) {
            poller.subscribers -= 1;
            if poller.subscribers == 0 {
                pollers.remove(&self.cwd);
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl VcsStatusBroadcaster {
    /// `fetch_interval` is read before every automatic remote refresh; zero
    /// disables them after the first remote load.
    pub(crate) fn new(
        github: Option<GitHubCli>,
        fetch_interval: Arc<dyn Fn() -> Duration + Send + Sync>,
    ) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(Inner {
                lookups: PullRequestLookup::new(github),
                fetches: UpstreamFetches::default(),
                cache: Default::default(),
                local_results: Default::default(),
                remote_results: Default::default(),
                changes,
                pollers: Default::default(),
                write_locks: Default::default(),
                fetch_interval,
            }),
        }
    }

    pub(crate) fn github(&self) -> Option<&GitHubCli> {
        self.inner.lookups.github()
    }

    pub(crate) fn lookups(&self) -> &PullRequestLookup {
        &self.inner.lookups
    }

    fn write_lock(&self, cwd: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.inner.write_locks)
            .entry(cwd.to_owned())
            .or_default()
            .clone()
    }

    fn cached(&self, cwd: &str) -> Cached {
        lock(&self.inner.cache).get(cwd).cloned().unwrap_or_default()
    }

    fn publish(&self, cwd: &str, event: VcsStatusStreamEvent) {
        let _ = self.inner.changes.send(Change {
            cwd: cwd.to_owned(),
            event,
        });
    }

    fn store_local(&self, cwd: &str, local: VcsStatusLocal, publish: bool) {
        let changed = {
            let mut cache = lock(&self.inner.cache);
            let entry = cache.entry(cwd.to_owned()).or_default();
            let changed = entry.local.as_ref() != Some(&local);
            entry.local = Some(local.clone());
            changed
        };
        if publish && changed {
            self.publish(cwd, VcsStatusStreamEvent::LocalUpdated { local });
        }
    }

    fn store_remote(&self, cwd: &str, remote: Option<VcsStatusRemote>, publish: bool) {
        let changed = {
            let mut cache = lock(&self.inner.cache);
            let entry = cache.entry(cwd.to_owned()).or_default();
            let changed = entry.remote.as_ref() != Some(&remote);
            entry.remote = Some(remote.clone());
            changed
        };
        if publish && changed {
            self.publish(cwd, VcsStatusStreamEvent::RemoteUpdated { remote });
        }
    }

    fn store_both(
        &self,
        cwd: &str,
        local: VcsStatusLocal,
        remote: Option<VcsStatusRemote>,
        publish: bool,
    ) -> VcsStatus {
        let changed = {
            let mut cache = lock(&self.inner.cache);
            let entry = cache.entry(cwd.to_owned()).or_default();
            let changed =
                entry.local.as_ref() != Some(&local) || entry.remote.as_ref() != Some(&remote);
            entry.local = Some(local.clone());
            entry.remote = Some(remote.clone());
            changed
        };
        if publish && changed {
            self.publish(
                cwd,
                VcsStatusStreamEvent::Snapshot {
                    local: local.clone(),
                    remote: remote.clone(),
                },
            );
        }
        VcsStatus::merge(local, remote)
    }

    /// GitManager.localStatus: the local half, shared within a second.
    async fn read_local(&self, cwd: &str) -> Result<VcsStatusLocal> {
        if let Some(recent) = lock(&self.inner.local_results).get(cwd)
            && recent.read_at.elapsed() < RESULT_CACHE_TTL
        {
            return Ok(recent.value.clone());
        }
        let path = std::path::PathBuf::from(cwd);
        let local = tokio::task::spawn_blocking(move || local_status(&path, true)).await??;
        lock(&self.inner.local_results).insert(
            cwd.to_owned(),
            Recent {
                value: local.clone(),
                read_at: Instant::now(),
            },
        );
        Ok(local)
    }

    /// GitManager.remoteStatus: the remote half with its PR; `None` outside a
    /// repository. Reads that skip the fetch, or retry a missing PR, bypass
    /// the one-second cache.
    async fn read_remote(
        &self,
        cwd: &str,
        refresh_upstream: bool,
        refresh_missing_pr: bool,
    ) -> Result<Option<VcsStatusRemote>> {
        if refresh_upstream
            && !refresh_missing_pr
            && let Some(recent) = lock(&self.inner.remote_results).get(cwd)
            && recent.read_at.elapsed() < RESULT_CACHE_TTL
        {
            return Ok(recent.value.clone());
        }
        let details =
            remote_status(std::path::Path::new(cwd), &self.inner.fetches, refresh_upstream).await?;
        let remote = match details {
            None => None,
            Some(details) => {
                let pr = match &details.branch {
                    Some(branch) => {
                        self.inner
                            .lookups
                            .status_pr(
                                cwd,
                                &BranchDetails {
                                    branch: branch.clone(),
                                    upstream_ref: details.upstream_ref.clone(),
                                    default_branch: details.default_branch.clone(),
                                    is_default_branch: details.is_default_branch,
                                },
                                refresh_missing_pr,
                            )
                            .await
                    }
                    None => None,
                };
                Some(VcsStatusRemote {
                    has_upstream: details.has_upstream,
                    ahead_count: details.ahead_count,
                    behind_count: details.behind_count,
                    ahead_of_default_count: Some(details.ahead_of_default_count),
                    pr,
                })
            }
        };
        if refresh_upstream && !refresh_missing_pr {
            lock(&self.inner.remote_results).insert(
                cwd.to_owned(),
                Recent {
                    value: remote.clone(),
                    read_at: Instant::now(),
                },
            );
        }
        Ok(remote)
    }

    fn invalidate_local(&self, cwd: &str) {
        lock(&self.inner.local_results).remove(cwd);
    }

    fn invalidate_remote(&self, cwd: &str) {
        lock(&self.inner.remote_results).remove(cwd);
    }

    /// Explicit freshness: the next reads bypass every cache, the slow PR
    /// lookup included.
    pub(crate) fn invalidate_status(&self, cwd: &str) {
        self.invalidate_local(cwd);
        self.invalidate_remote(cwd);
        // The merged cache is consulted before either recent-read cache. Drop
        // it as well or a subsequent get_status call could return the stale
        // pre-invalidation halves without doing any reads.
        lock(&self.inner.cache).remove(cwd);
        self.inner.lookups.bump_epoch(cwd);
    }

    /// The status, from the cache when both halves are there.
    pub(crate) async fn get_status(&self, cwd: &str) -> Result<VcsStatus> {
        let cwd = canonical(cwd).await;
        let cached = self.cached(&cwd);
        if let (Some(local), Some(remote)) = (cached.local, cached.remote) {
            return Ok(VcsStatus::merge(local, remote));
        }
        let guard = self.write_lock(&cwd);
        let _permit = guard.lock().await;
        let latest = self.cached(&cwd);
        let local = match latest.local {
            Some(local) => local,
            None => self.read_local(&cwd).await?,
        };
        let remote = match latest.remote {
            Some(remote) => remote,
            None => self.read_remote(&cwd, true, false).await?,
        };
        Ok(self.store_both(&cwd, local, remote, false))
    }

    async fn refresh_local(&self, cwd: &str) -> Result<VcsStatusLocal> {
        self.invalidate_local(cwd);
        let local = self.read_local(cwd).await?;
        self.store_local(cwd, local.clone(), true);
        Ok(local)
    }

    /// Reads the local half again and tells subscribers.
    pub(crate) async fn refresh_local_status(&self, cwd: &str) -> Result<VcsStatusLocal> {
        let cwd = canonical(cwd).await;
        self.refresh_local(&cwd).await
    }

    /// Reads the remote half again; a fetch that moved the divergence re-reads
    /// the local totals too, which compare against remote refs.
    async fn refresh_remote(
        &self,
        cwd: &str,
        refresh_upstream: bool,
    ) -> Result<Option<VcsStatusRemote>> {
        let guard = self.write_lock(cwd);
        let _permit = guard.lock().await;
        if refresh_upstream {
            self.invalidate_remote(cwd);
        }
        let previous = self.cached(cwd).remote.flatten();
        let remote = self.read_remote(cwd, refresh_upstream, false).await?;
        if let Some(remote) = &remote
            && previous.as_ref().is_none_or(|previous| {
                previous.ahead_count != remote.ahead_count
                    || previous.behind_count != remote.behind_count
                    || previous.ahead_of_default_count != remote.ahead_of_default_count
            })
        {
            self.refresh_local(cwd).await?;
        }
        self.store_remote(cwd, remote.clone(), true);
        Ok(remote)
    }

    /// Reads both halves again, remote first since the fetch can move the
    /// base the Changes totals compare with.
    pub(crate) async fn refresh_status(&self, cwd: &str) -> Result<VcsStatus> {
        let cwd = canonical(cwd).await;
        let guard = self.write_lock(&cwd);
        let _permit = guard.lock().await;
        self.invalidate_status(&cwd);
        let remote = self.read_remote(&cwd, true, false).await?;
        let local = self.read_local(&cwd).await?;
        Ok(self.store_both(&cwd, local, remote, true))
    }

    /// After a turn: asks for a missing PR again without fetching, when the
    /// checkout is loaded. Known PRs and failed lookups' backoff stay cached.
    pub(crate) async fn refresh_pull_request_status(
        &self,
        cwd: &str,
    ) -> Result<Option<VcsStatusRemote>> {
        let cwd = canonical(cwd).await;
        let guard = self.write_lock(&cwd);
        let _permit = guard.lock().await;
        if self.cached(&cwd).remote.flatten().is_none() {
            return Ok(None);
        }
        let remote = self.read_remote(&cwd, false, true).await?;
        self.store_remote(&cwd, remote.clone(), true);
        Ok(remote)
    }

    /// Refreshes in the background after one of our own Git operations.
    pub(crate) fn spawn_refresh(&self, cwd: &str) {
        let (broadcaster, cwd) = (self.clone(), cwd.to_owned());
        tokio::spawn(async move {
            if let Err(error) = broadcaster.refresh_status(&cwd).await {
                tracing::warn!(operation = "host.vcs.refresh", message = %format_args!("{error:#}"));
            }
        });
    }

    /// The current status as one snapshot event, for a subscriber that fell
    /// behind.
    pub(crate) async fn snapshot_event(&self, cwd: &str) -> Result<VcsStatusStreamEvent> {
        let cwd = canonical(cwd).await;
        let cached = self.cached(&cwd);
        let local = match cached.local {
            Some(local) => local,
            None => self.refresh_local(&cwd).await?,
        };
        Ok(VcsStatusStreamEvent::Snapshot {
            local,
            remote: cached.remote.flatten(),
        })
    }

    /// Subscribes to a checkout: the local half now, the remote half once the
    /// poller reads it. The poller runs while any subscription is held.
    pub(crate) async fn subscribe(
        &self,
        cwd: &str,
    ) -> Result<(VcsStatusStreamEvent, Subscription)> {
        let cwd = canonical(cwd).await;
        let receiver = self.inner.changes.subscribe();
        let cached = self.cached(&cwd);
        let local = match cached.local {
            Some(local) => local,
            None => self.refresh_local(&cwd).await?,
        };
        let refresh_immediately = cached.remote.is_none();
        {
            let mut pollers = lock(&self.inner.pollers);
            match pollers.get_mut(&cwd) {
                Some(poller) => poller.subscribers += 1,
                None => {
                    let task = tokio::spawn(self.clone().poll(cwd.clone(), refresh_immediately));
                    pollers.insert(
                        cwd.clone(),
                        Poller {
                            subscribers: 1,
                            _task: AbortOnDropHandle::new(task),
                        },
                    );
                }
            }
        }
        Ok((
            VcsStatusStreamEvent::Snapshot {
                local,
                remote: cached.remote.flatten(),
            },
            Subscription {
                inner: self.inner.clone(),
                cwd,
                receiver,
            },
        ))
    }

    /// Refreshes the remote half on the fetch interval; a zero interval reads
    /// it once and then rests. Failures wait longer each time.
    async fn poll(self, cwd: String, refresh_immediately: bool) {
        let mut failures = 0u32;
        let mut needs_initial = refresh_immediately;
        let active = |configured: Duration| {
            if configured.is_zero() {
                DEFAULT_REFRESH_INTERVAL
            } else {
                configured
            }
        };
        if !refresh_immediately {
            tokio::time::sleep(active((self.inner.fetch_interval)())).await;
        }
        loop {
            let configured = (self.inner.fetch_interval)();
            let interval = active(configured);
            let delay = if configured.is_zero() && !needs_initial {
                interval
            } else {
                match self.refresh_remote(&cwd, !configured.is_zero()).await {
                    Ok(_) => {
                        needs_initial = false;
                        failures = 0;
                        interval
                    }
                    Err(error) => {
                        failures += 1;
                        let delay = remote_refresh_failure_delay(failures, interval);
                        tracing::warn!(
                            operation = "host.vcs.remote_refresh",
                            failures,
                            next_delay_ms = delay.as_millis() as u64,
                            message = %format_args!("{error:#}"),
                            "VCS remote status refresh failed"
                        );
                        delay
                    }
                }
            };
            tokio::time::sleep(delay).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // VcsStatusBroadcaster.test.ts "backs off remote refresh failures
    // exponentially and honors larger configured intervals".
    #[test]
    fn remote_refresh_failures_back_off_and_honor_larger_intervals() {
        let second = Duration::from_secs(1);
        assert_eq!(remote_refresh_failure_delay(1, second), Duration::from_secs(30));
        assert_eq!(remote_refresh_failure_delay(2, second), Duration::from_secs(60));
        assert_eq!(remote_refresh_failure_delay(3, second), Duration::from_secs(120));
        assert_eq!(
            remote_refresh_failure_delay(1, Duration::from_secs(300)),
            Duration::from_secs(300)
        );
        assert_eq!(remote_refresh_failure_delay(20, second), Duration::from_secs(900));
    }
}
