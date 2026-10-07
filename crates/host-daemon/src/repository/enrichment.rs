//! Project roots' repository identities, resolved in the background so reading
//! a project never waits on Git. A resolved identity is reused for a minute and
//! a failed lookup is retried after five seconds; until a lookup completes the
//! previous identity stays readable. Each completion that changes a root's
//! identity is announced so shells can show it.
use super::{Clock, RepositoryIdentity};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, broadcast};

/// Looks up one root's identity; an error is a lookup that could not run.
pub(crate) type Lookup = Arc<
    dyn Fn(String) -> BoxFuture<'static, Result<Option<RepositoryIdentity>, String>> + Send + Sync,
>;

pub(crate) struct Options {
    pub(crate) capacity: usize,
    /// Roots being or waiting to be resolved; further requests are dropped.
    pub(crate) max_pending: usize,
    pub(crate) concurrency: usize,
    pub(crate) success_ttl: Duration,
    pub(crate) failure_ttl: Duration,
    pub(crate) clock: Clock,
}
impl Options {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            capacity: 512,
            max_pending: 512,
            concurrency: 4,
            success_ttl: Duration::from_secs(60),
            failure_ttl: Duration::from_secs(5),
            clock,
        }
    }
}

struct Entry {
    /// The last successful lookup.
    identity: Option<Option<RepositoryIdentity>>,
    expires: Instant,
    used: u64,
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Roots being resolved, and whether they were invalidated meanwhile.
    pending: HashMap<String, bool>,
    used: u64,
}

struct Inner {
    lookup: Lookup,
    options: Options,
    state: Mutex<State>,
    permits: Arc<Semaphore>,
    changes: broadcast::Sender<String>,
}

pub(crate) struct ProjectIdentities {
    inner: Arc<Inner>,
}

impl ProjectIdentities {
    /// Identities from the Host's resolver, all on one clock.
    pub(crate) fn system(clock: Clock) -> Self {
        let resolver = Arc::new(super::system_resolver(clock.clone()));
        Self::new(
            Arc::new(move |root: String| {
                let resolver = resolver.clone();
                Box::pin(async move { resolver.resolve(&root, false).await })
            }),
            Options::new(clock),
        )
    }

    pub(crate) fn new(lookup: Lookup, options: Options) -> Self {
        let options = Options {
            capacity: options.capacity.max(1),
            max_pending: options.max_pending.max(1),
            concurrency: options.concurrency.max(1),
            ..options
        };
        Self {
            inner: Arc::new(Inner {
                lookup,
                permits: Arc::new(Semaphore::new(options.concurrency)),
                options,
                state: Mutex::default(),
                changes: broadcast::channel(256).0,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn clock(&self) -> &Clock {
        &self.inner.options.clock
    }

    /// The root's last resolved identity, without starting a lookup.
    pub(crate) fn peek(&self, root: &str) -> Option<RepositoryIdentity> {
        let mut state = self.inner.lock();
        state.used += 1;
        let used = state.used;
        let entry = state.entries.get_mut(root)?;
        entry.used = used;
        entry.identity.clone().flatten()
    }

    /// Schedules a lookup unless the root's identity is fresh or being resolved.
    pub(crate) fn request(&self, root: &str) {
        let now = self.inner.options.clock.now();
        {
            let mut state = self.inner.lock();
            if state
                .entries
                .get(root)
                .is_some_and(|entry| entry.expires > now)
                || state.pending.contains_key(root)
                || state.pending.len() >= self.inner.options.max_pending
            {
                return;
            }
            state.pending.insert(root.to_owned(), false);
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.inner.lock().pending.remove(root);
            return;
        };
        runtime.spawn(self.inner.clone().resolve(root.to_owned()));
    }

    /// The root's last resolved identity; a missing or expired one is looked up
    /// in the background.
    pub(crate) fn available(&self, root: &str) -> Option<RepositoryIdentity> {
        let available = self.peek(root);
        self.request(root);
        available
    }

    /// Forgets the roots' identities. A lookup already running is repeated.
    pub(crate) fn invalidate<'a>(&self, roots: impl IntoIterator<Item = &'a str>) {
        let mut state = self.inner.lock();
        for root in roots {
            state.entries.remove(root);
            if let Some(invalidated) = state.pending.get_mut(root) {
                *invalidated = true;
            }
        }
    }

    /// Roots whose identity changed. A slow receiver misses the oldest.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<String> {
        self.inner.changes.subscribe()
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    async fn resolve(self: Arc<Self>, root: String) {
        let Ok(_permit) = self.permits.clone().acquire_owned().await else {
            self.lock().pending.remove(&root);
            return;
        };
        loop {
            let result = AssertUnwindSafe((self.lookup)(root.clone()))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("the lookup panicked".into()));
            let changed = {
                let mut state = self.lock();
                if state.pending.get(&root) == Some(&true) {
                    state.pending.insert(root.clone(), false);
                    continue;
                }
                state.pending.remove(&root);
                self.store(&mut state, &root, result)
            };
            if changed {
                let _ = self.changes.send(root);
            }
            return;
        }
    }

    /// Records a lookup; true when the root's readable identity changed.
    fn store(
        &self,
        state: &mut State,
        root: &str,
        result: Result<Option<RepositoryIdentity>, String>,
    ) -> bool {
        let now = self.options.clock.now();
        let previous = state.entries.remove(root).and_then(|entry| entry.identity);
        let (identity, expires, changed) = match result {
            Ok(identity) => {
                let changed = previous.clone().flatten() != identity;
                (Some(identity), now + self.options.success_ttl, changed)
            }
            Err(error) => {
                tracing::warn!(
                    operation = "project.repository_identity",
                    root,
                    message = %error
                );
                (previous, now + self.options.failure_ttl, false)
            }
        };
        if state.entries.len() >= self.options.capacity
            && let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(root, _)| root.clone())
        {
            state.entries.remove(&oldest);
        }
        state.used += 1;
        let used = state.used;
        state.entries.insert(
            root.to_owned(),
            Entry {
                identity,
                expires,
                used,
            },
        );
        changed
    }
}

#[cfg(test)]
mod tests;
