//! A project's repository identity from its Git remotes, so clients group
//! checkouts of one repository across Hosts.
mod enrichment;
mod forgejo;

pub(crate) use enrichment::ProjectIdentities;

use agent_protocol::models::{RepositoryIdentity, RepositoryLocator};
use futures_util::future::BoxFuture;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CACHE_CAPACITY: usize = 512;
/// Identities change rarely; listing projects re-reads each one often.
const POSITIVE_TTL: Duration = Duration::from_secs(15 * 60);
/// Short, so a folder that gains a repository or a remote shows up quickly.
const NEGATIVE_TTL: Duration = Duration::from_secs(60);

/// The time caches expire by. In tests it stands still until moved forward.
#[derive(Clone)]
#[cfg_attr(not(test), derive(Default))]
pub(crate) struct Clock {
    #[cfg(test)]
    start: Instant,
    #[cfg(test)]
    offset: Arc<std::sync::atomic::AtomicU64>,
}
#[cfg(test)]
impl Default for Clock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            offset: Arc::default(),
        }
    }
}
impl Clock {
    pub(crate) fn now(&self) -> Instant {
        #[cfg(test)]
        {
            self.start
                + Duration::from_millis(self.offset.load(std::sync::atomic::Ordering::SeqCst))
        }
        #[cfg(not(test))]
        Instant::now()
    }
    #[cfg(test)]
    pub(crate) fn advance(&self, by: Duration) {
        self.offset
            .fetch_add(by.as_millis() as u64, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `git` in a directory: its standard output, or `None` when it fails.
pub(crate) type Git = Arc<dyn Fn(&Path, &[&str]) -> Option<String> + Send + Sync>;
/// Completes an identity from the provider's own configuration; a failure keeps
/// the identity as resolved from the remote.
pub(crate) type Refine = Arc<
    dyn Fn(RepositoryIdentity) -> BoxFuture<'static, Result<RepositoryIdentity, String>>
        + Send
        + Sync,
>;

/// `git remote -v` fetch URLs by remote name.
fn fetch_urls(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (name, url, direction) = (parts.next()?, parts.next()?, parts.next()?);
            (parts.next().is_none() && direction == "(fetch)")
                .then(|| (name.to_owned(), url.to_owned()))
        })
        .collect()
}

/// `upstream`, then `origin`, then the first remote in locale order.
fn primary_remote(remotes: &BTreeMap<String, String>) -> Option<(&str, &str)> {
    let collator =
        icu_collator::Collator::try_new(&Default::default(), icu_collator::CollatorOptions::new())
            .expect("compiled root collation");
    ["upstream", "origin"]
        .iter()
        .find_map(|name| remotes.get_key_value(*name))
        .or_else(|| {
            remotes
                .iter()
                .min_by(|(left, _), (right, _)| collator.compare(left, right))
        })
        .map(|(name, url)| (name.as_str(), url.as_str()))
}

/// The host a remote URL points at: SCP-style and SSH remotes give the host
/// name, other URLs keep an explicit port.
fn remote_host(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    if let Some((user, rest)) = trimmed.split_once('@')
        && !user.is_empty()
        && user
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && let Some((host, _)) = rest.split_once(':')
        && !host.is_empty()
        && !host.contains('/')
    {
        return Some(host.to_lowercase());
    }
    let url = url::Url::parse(trimmed).ok()?;
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(port) if url.scheme() != "ssh" => format!("{host}:{port}"),
        _ => host,
    })
}

/// The provider kind.
fn provider_kind(remote: &str) -> Option<&'static str> {
    let host = remote_host(remote)?;
    let name = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host.as_str(), |(name, _)| name);
    let label = |label: &str| name.split('.').any(|part| part == label);
    Some(
        if name == "codeberg.org" || label("forgejo") || label("gitea") {
            "forgejo"
        } else if name == "github.com" || label("github") {
            "github"
        } else if name == "gitlab.com" || label("gitlab") {
            "gitlab"
        } else if name == "dev.azure.com"
            || name.ends_with(".dev.azure.com")
            || name.ends_with(".visualstudio.com")
        {
            "azure-devops"
        } else if name == "bitbucket.org" || label("bitbucket") {
            "bitbucket"
        } else {
            "unknown"
        },
    )
}

pub(crate) fn identity(remote_name: &str, remote_url: &str, root_path: &str) -> RepositoryIdentity {
    let canonical_key = agent_runtime::normalize_remote_url(remote_url);
    let path = canonical_key
        .split('/')
        .skip(1)
        .collect::<Vec<_>>()
        .join("/");
    let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    RepositoryIdentity {
        locator: RepositoryLocator {
            source: "git-remote".into(),
            remote_name: remote_name.into(),
            remote_url: remote_url.into(),
        },
        web_url: None,
        root_path: Some(root_path.into()),
        display_name: (!path.is_empty()).then(|| path.clone()),
        provider: provider_kind(remote_url).map(str::to_owned),
        owner: segments.first().map(|owner| (*owner).to_owned()),
        name: segments.last().map(|name| (*name).to_owned()),
        canonical_key,
    }
}

/// Values that expire, evicting the least recently used beyond the capacity.
struct Expiring<V> {
    capacity: usize,
    state: Mutex<ExpiringState<V>>,
}
struct ExpiringState<V> {
    uses: u64,
    entries: HashMap<String, Expires<V>>,
}
struct Expires<V> {
    value: V,
    at: Instant,
    used: u64,
}
impl<V: Clone> Expiring<V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(ExpiringState {
                uses: 0,
                entries: HashMap::new(),
            }),
        }
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, ExpiringState<V>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
    fn get(&self, key: &str, now: Instant) -> Option<V> {
        let mut state = self.lock();
        state.uses += 1;
        let uses = state.uses;
        let entry = state.entries.get_mut(key)?;
        if entry.at <= now {
            state.entries.remove(key);
            return None;
        }
        entry.used = uses;
        Some(entry.value.clone())
    }
    fn insert(&self, key: String, value: V, at: Instant) {
        let mut state = self.lock();
        if !state.entries.contains_key(&key)
            && state.entries.len() >= self.capacity
            && let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
        {
            state.entries.remove(&oldest);
        }
        state.uses += 1;
        let used = state.uses;
        state.entries.insert(key, Expires { value, at, used });
    }
    fn invalidate(&self, key: &str) {
        self.lock().entries.remove(key);
    }
}

pub(crate) struct ResolverOptions {
    pub(crate) capacity: usize,
    pub(crate) positive_ttl: Duration,
    pub(crate) negative_ttl: Duration,
    pub(crate) refine: Option<Refine>,
    pub(crate) git: Git,
    pub(crate) clock: Clock,
}
impl ResolverOptions {
    pub(crate) fn new(clock: Clock) -> Self {
        Self {
            capacity: CACHE_CAPACITY,
            positive_ttl: POSITIVE_TTL,
            negative_ttl: NEGATIVE_TTL,
            refine: None,
            git: Arc::new(|cwd, args| crate::git::text(cwd, args).ok()),
            clock,
        }
    }
}

/// Resolves the repository containing a folder: its Git top level, then that
/// root's primary remote. Both answers are cached; a found one for longer than
/// a missing one.
pub(crate) struct Resolver {
    roots: Expiring<Option<String>>,
    identities: Expiring<Option<RepositoryIdentity>>,
    options: ResolverOptions,
}

impl Resolver {
    pub(crate) fn new(options: ResolverOptions) -> Self {
        Self {
            roots: Expiring::new(options.capacity),
            identities: Expiring::new(options.capacity),
            options,
        }
    }

    /// The identity of the repository containing `cwd`, or `None` for a folder
    /// outside Git or a repository without remotes. `refresh` rereads both.
    /// Fails only when the lookup itself could not run.
    pub(crate) async fn resolve(
        &self,
        cwd: &str,
        refresh: bool,
    ) -> Result<Option<RepositoryIdentity>, String> {
        if refresh {
            self.roots.invalidate(cwd);
        }
        let root = match self.roots.get(cwd, self.options.clock.now()) {
            Some(root) => root,
            None => {
                let (git, cwd_path) = (self.options.git.clone(), cwd.to_owned());
                let root = tokio::task::spawn_blocking(move || {
                    let top = git(Path::new(&cwd_path), &["rev-parse", "--show-toplevel"])?;
                    let top = top.trim();
                    (!top.is_empty()).then(|| top.to_owned())
                })
                .await
                .map_err(|error| error.to_string())?;
                self.roots
                    .insert(cwd.to_owned(), root.clone(), self.expires(root.is_some()));
                root
            }
        };
        let Some(root) = root else {
            return Ok(None);
        };
        if refresh {
            self.identities.invalidate(&root);
        }
        if let Some(identity) = self.identities.get(&root, self.options.clock.now()) {
            return Ok(identity);
        }
        let (git, top) = (self.options.git.clone(), root.clone());
        let resolved = tokio::task::spawn_blocking(move || {
            let remotes = fetch_urls(&git(Path::new(&top), &["remote", "-v"])?);
            let (name, url) = primary_remote(&remotes)?;
            Some(identity(name, url, &top))
        })
        .await
        .map_err(|error| error.to_string())?;
        let resolved = match (resolved, &self.options.refine) {
            (Some(identity), Some(refine)) => {
                Some(refine(identity.clone()).await.unwrap_or(identity))
            }
            (resolved, _) => resolved,
        };
        self.identities
            .insert(root, resolved.clone(), self.expires(resolved.is_some()));
        Ok(resolved)
    }

    fn expires(&self, found: bool) -> Instant {
        self.options.clock.now()
            + if found {
                self.options.positive_ttl
            } else {
                self.options.negative_ttl
            }
    }
}

/// The Host's resolver: Git, refined by the configured Forgejo and Gitea logins.
pub(crate) fn system_resolver(clock: Clock) -> Resolver {
    let logins: Arc<dyn forgejo::Logins> = Arc::new(forgejo::CliLogins::system());
    let mut options = ResolverOptions::new(clock);
    options.refine = Some(Arc::new(move |identity| {
        let logins = logins.clone();
        Box::pin(async move { Ok(forgejo::refine(identity, logins.as_ref()).await) })
    }));
    Resolver::new(options)
}

#[cfg(test)]
mod tests;
