//! Local discovery shares one normal Host across credential directories. The
//! short registry lock serializes discovery/startup; host.lock owns the process
//! lifetime. A stale registry survives crashes without reviving a stale ticket.
use crate::KeyStorage;
use agent_core::transport::{Identity, Ticket};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct LocalHostRegistry {
    directory: PathBuf,
    legacy_directory: Option<PathBuf>,
}

#[derive(Clone)]
pub struct LocalHost {
    pub directory: PathBuf,
    pub key_storage: Option<KeyStorage>,
    pub state: LocalHostState,
}

#[derive(Clone)]
pub enum LocalHostState {
    Stopped,
    Starting,
    Ready(Ticket),
}

#[derive(Serialize, Deserialize)]
struct Registration {
    directory: PathBuf,
    #[serde(default)]
    key_storage: Option<KeyStorage>,
    ready: bool,
}

/// Keep this lease until Codex and the Host have both stopped.
pub struct HostLease {
    directory: PathBuf,
    key_storage: KeyStorage,
    registry: LocalHostRegistry,
    _lock: FileLock,
}

struct FileLock(File);
impl Drop for FileLock {
    fn drop(&mut self) {
        // A concurrently forked child can retain this open file description
        // until exec. Releasing ownership must not wait for its descriptor.
        if let Err(error) = self.0.unlock() {
            agent_core::diagnostics::error("release local Host lock", &error.to_string());
        }
    }
}

impl LocalHostRegistry {
    /// A scoped registry for explicitly isolated development/test instances.
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            legacy_directory: None,
        }
    }

    pub fn for_user() -> Result<Self> {
        let directory = directories::ProjectDirs::from("app", "bex", "BEX")
            .context("application data directory unavailable")?
            .data_local_dir()
            .to_owned();
        Ok(Self {
            directory,
            legacy_directory: directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".bex")),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reuse a running registration, adopting a single pre-registry Host when
    /// upgrading. Remember its credential directory even after it stops.
    pub fn resolve(&self, preferred: &Path) -> Result<LocalHost> {
        let _coordination = self.coordinate()?;
        self.resolve_locked(preferred)
    }

    pub fn acquire(&self, directory: &Path, key_storage: Option<KeyStorage>) -> Result<HostLease> {
        let _coordination = self.coordinate()?;
        let current = self.resolve_locked(directory)?;
        if !matches!(current.state, LocalHostState::Stopped) {
            anyhow::bail!(
                "Host is already running in {}; use the existing Host",
                current.directory.display()
            );
        }
        crate::platform::create_state_directory(directory)?;
        let directory = directory.canonicalize()?;
        let key_storage = key_storage
            .or_else(|| {
                (current.directory == directory)
                    .then_some(current.key_storage)
                    .flatten()
            })
            .or(existing_key_storage(&directory)?)
            .unwrap_or_default();
        let lock = open_lock(&directory.join("host.lock"))?;
        lock.try_lock()
            .context("Host is already running or its lock is unavailable")?;
        self.save(&Registration {
            directory: directory.clone(),
            key_storage: Some(key_storage),
            ready: false,
        })?;
        Ok(HostLease {
            directory,
            key_storage,
            registry: self.clone(),
            _lock: FileLock(lock),
        })
    }

    fn coordinate(&self) -> Result<FileLock> {
        crate::platform::create_state_directory(&self.directory)?;
        let lock = open_lock(&self.directory.join("host-instance.lock"))?;
        lock.lock()?;
        Ok(FileLock(lock))
    }

    fn resolve_locked(&self, preferred: &Path) -> Result<LocalHost> {
        let registration = match fs::read(self.directory.join("host-instance.json")) {
            Ok(bytes) => {
                let registration: Registration =
                    serde_json::from_slice(&bytes).context("invalid local Host registration")?;
                if !registration.directory.is_absolute() {
                    return Err(anyhow::anyhow!(
                        "local Host registration requires an absolute directory"
                    ));
                }
                Some(registration)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut active = Vec::new();
        for directory in [
            registration
                .as_ref()
                .map(|registered| registered.directory.as_path()),
            Some(self.directory.as_path()),
            self.legacy_directory.as_deref(),
            Some(preferred),
        ]
        .into_iter()
        .flatten()
        {
            if running(directory)? {
                let directory = directory.canonicalize()?;
                if !active.contains(&directory) {
                    active.push(directory);
                }
            }
        }
        if active.len() > 1 {
            anyhow::bail!(
                "Multiple local Hosts are running in {}. Keep one Host after its work finishes; Bex will not choose or stop one automatically.",
                active
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if let Some(directory) = active.pop() {
            if let Some(registration) = &registration
                && registration.directory == directory
            {
                return registration.location();
            }
            let registration = Registration {
                key_storage: existing_key_storage(&directory)?,
                directory,
                ready: true,
            };
            let location = registration.location()?;
            self.save(&registration)?;
            return Ok(location);
        }
        let directory = registration
            .as_ref()
            .map_or(preferred, |entry| &entry.directory);
        Ok(LocalHost {
            directory: directory.to_owned(),
            key_storage: registration
                .as_ref()
                .and_then(|entry| entry.key_storage)
                .or(existing_key_storage(directory)?),
            state: LocalHostState::Stopped,
        })
    }

    fn save(&self, registration: &Registration) -> Result<()> {
        crate::platform::save_private_json(&self.directory.join("host-instance.json"), registration)
    }
}

impl Registration {
    fn location(&self) -> Result<LocalHost> {
        let state = if self.ready {
            match fs::read_to_string(self.directory.join("host.ticket")) {
                Ok(ticket) => LocalHostState::Ready(ticket.trim().parse::<Ticket>()?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => LocalHostState::Starting,
                Err(error) => return Err(error.into()),
            }
        } else {
            LocalHostState::Starting
        };
        Ok(LocalHost {
            directory: self.directory.clone(),
            key_storage: self.key_storage.or(existing_key_storage(&self.directory)?),
            state,
        })
    }
}

impl HostLease {
    pub fn isolated(directory: &Path, key_storage: Option<KeyStorage>) -> Result<Self> {
        LocalHostRegistry::new(directory.to_owned()).acquire(directory, key_storage)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn key_storage(&self) -> KeyStorage {
        self.key_storage
    }

    pub fn publish(&self, ticket: &Ticket) -> Result<()> {
        let _coordination = self.registry.coordinate()?;
        // No credentials are written here: tickets contain only endpoint addresses.
        atomicwrites::AtomicFile::new(
            self.directory.join("host.ticket"),
            atomicwrites::AllowOverwrite,
        )
        .write_with_options(
            |file| {
                file.write_all(ticket.to_string().as_bytes())?;
                file.sync_all()
            },
            crate::platform::private_file_options(),
        )?;
        self.registry.save(&Registration {
            directory: self.directory.clone(),
            key_storage: Some(self.key_storage),
            ready: true,
        })
    }
}

impl LocalHost {
    /// Read the discovered Host's identity without provisioning a second one.
    pub fn load_identity(&self) -> Result<Identity> {
        let store = self.key_storage.unwrap_or_default().open(&self.directory)?;
        crate::load_local_identity(store.as_ref())
    }
}

fn existing_key_storage(directory: &Path) -> Result<Option<KeyStorage>> {
    if directory.join("identity.keys").try_exists()? {
        Ok(Some(KeyStorage::File))
    } else if directory.join("trust.json").try_exists()? {
        Ok(Some(KeyStorage::Keyring))
    } else {
        Ok(None)
    }
}

fn open_lock(path: &Path) -> Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(Into::into)
}

fn running(directory: &Path) -> Result<bool> {
    let lock = match File::options()
        .read(true)
        .write(true)
        .open(directory.join("host.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match lock.try_lock() {
        Ok(()) => {
            lock.unlock()?;
            Ok(false)
        }
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::transport::{Endpoint, Identity, Relays};

    #[cfg(unix)]
    #[test]
    fn stopping_host_releases_lock_even_with_an_inherited_descriptor() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let host = fixture.path().join("host");
        let lease = registry.acquire(&host, Some(KeyStorage::File)).unwrap();
        let inherited = lease._lock.0.try_clone().unwrap();
        let mut child = scopeguard::guard(
            std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::from(inherited))
                .spawn()
                .unwrap(),
            |mut child| {
                let _ = child.kill();
                let _ = child.wait();
            },
        );
        assert!(child.try_wait().unwrap().is_none());
        drop(lease);
        assert!(matches!(
            registry.resolve(&host).unwrap().state,
            LocalHostState::Stopped
        ));
        let replacement = registry.acquire(&host, None).unwrap();
        assert!(registry.acquire(&host, None).is_err());
        drop(replacement);
    }

    #[tokio::test]
    async fn discovery_restores_the_hosts_file_backend_and_identity_after_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let host = fixture.path().join("mobile-host");
        let desktop = fixture.path().join("desktop");
        let lease = registry.acquire(&host, Some(KeyStorage::File)).unwrap();
        fs::write(host.join("host.ticket"), "stale invalid ticket").unwrap();
        assert!(matches!(
            registry.resolve(&desktop).unwrap().state,
            LocalHostState::Starting
        ));
        let credentials = crate::HostCredentials::load(
            lease.key_storage().open(lease.directory()).unwrap(),
            lease.directory().to_owned(),
        )
        .await
        .unwrap();
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
            .await
            .unwrap();
        lease.publish(&endpoint.ticket()).unwrap();
        let location = registry.resolve(&desktop).unwrap();
        assert_eq!(location.directory, host.canonicalize().unwrap());
        assert!(
            matches!(location.state, LocalHostState::Ready(ref ticket) if *ticket == endpoint.ticket())
        );
        assert_eq!(location.key_storage, Some(KeyStorage::File));
        assert_eq!(
            location.load_identity().unwrap().node_id(),
            credentials.local_identity().await.node_id()
        );
        assert!(
            !desktop.exists(),
            "discovery must not provision desktop credentials"
        );
        endpoint.close().await;
        drop(lease);
        let registry = LocalHostRegistry::new(registry.directory().to_owned());
        let location = registry.resolve(&desktop).unwrap();
        assert!(matches!(location.state, LocalHostState::Stopped));
        assert_eq!(location.key_storage, Some(KeyStorage::File));
        assert_eq!(location.directory, host.canonicalize().unwrap());
        let lease = registry.acquire(&location.directory, None).unwrap();
        assert!(matches!(
            registry.resolve(&desktop).unwrap().state,
            LocalHostState::Starting
        ));
        assert_eq!(lease.key_storage(), KeyStorage::File);
        let restored = crate::HostCredentials::load(
            lease.key_storage().open(lease.directory()).unwrap(),
            lease.directory().to_owned(),
        )
        .await
        .unwrap();
        assert_eq!(
            restored.host_identity().await.node_id(),
            credentials.host_identity().await.node_id()
        );
        assert_eq!(
            restored.local_identity().await.node_id(),
            credentials.local_identity().await.node_id()
        );
    }

    #[test]
    fn a_registration_does_not_hide_a_second_running_legacy_host() {
        let fixture = tempfile::tempdir().unwrap();
        let legacy = fixture.path().join("legacy");
        let mut registry = LocalHostRegistry::new(fixture.path().join("registry"));
        registry.legacy_directory = Some(legacy.clone());
        let _lease = registry
            .acquire(&fixture.path().join("first"), None)
            .unwrap();
        crate::platform::create_state_directory(&legacy).unwrap();
        let lock = open_lock(&legacy.join("host.lock")).unwrap();
        lock.lock().unwrap();
        assert!(
            registry
                .resolve(registry.directory())
                .err()
                .unwrap()
                .to_string()
                .contains("Multiple local Hosts")
        );
        assert!(
            registry
                .acquire(&fixture.path().join("third"), None)
                .is_err()
        );
        assert!(!fixture.path().join("third").exists());
    }

    #[tokio::test]
    async fn adoption_preserves_the_old_hosts_identity_and_rejects_ambiguous_legacy_hosts() {
        let fixture = tempfile::tempdir().unwrap();
        let mut registry = LocalHostRegistry::new(fixture.path().join("default"));
        let legacy = fixture.path().join("legacy");
        registry.legacy_directory = Some(legacy.clone());
        crate::platform::create_state_directory(&legacy).unwrap();
        let legacy_lock = open_lock(&legacy.join("host.lock")).unwrap();
        legacy_lock.lock().unwrap();
        let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        fs::write(legacy.join("host.ticket"), endpoint.ticket().to_string()).unwrap();
        let keys = b"existing identity must be untouched";
        fs::write(legacy.join("identity.keys"), keys).unwrap();
        crate::platform::create_state_directory(registry.directory()).unwrap();
        let other = open_lock(&registry.directory.join("host.lock")).unwrap();
        other.lock().unwrap();
        assert!(
            registry
                .resolve(registry.directory())
                .err()
                .unwrap()
                .to_string()
                .contains("Multiple local Hosts")
        );
        assert!(!registry.directory.join("host-instance.json").exists());
        other.unlock().unwrap();
        drop(other);
        let location = registry.resolve(registry.directory()).unwrap();
        assert_eq!(location.directory, legacy.canonicalize().unwrap());
        assert!(
            matches!(location.state, LocalHostState::Ready(ticket) if ticket == endpoint.ticket())
        );
        assert!(registry.acquire(registry.directory(), None).is_err());
        assert_eq!(fs::read(legacy.join("identity.keys")).unwrap(), keys);
        legacy_lock.unlock().unwrap();
        drop(legacy_lock);
        let location = registry.resolve(registry.directory()).unwrap();
        assert_eq!(location.directory, legacy.canonicalize().unwrap());
        assert!(matches!(location.state, LocalHostState::Stopped));
        endpoint.close().await;
    }

    #[test]
    fn simultaneous_starts_reserve_only_one_host() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|name| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                let directory = fixture.path().join(name);
                std::thread::spawn(move || {
                    barrier.wait();
                    registry.acquire(&directory, None)
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let loser = if results[0].is_err() {
            "first"
        } else {
            "second"
        };
        let loser = fixture.path().join(loser);
        assert!(
            !loser.exists(),
            "reject before provisioning another identity"
        );
        assert!(
            registry.acquire(&loser, None).is_err(),
            "the winner remains exclusive after startup"
        );
        assert!(!loser.exists());
        assert!(matches!(
            registry.resolve(registry.directory()).unwrap().state,
            LocalHostState::Starting
        ));
    }

    #[test]
    fn explicit_isolation_keeps_separate_hosts_and_still_locks_each_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        let _a = HostLease::isolated(&first, Some(KeyStorage::File)).unwrap();
        let _b = HostLease::isolated(&second, Some(KeyStorage::File)).unwrap();
        assert!(HostLease::isolated(&first, Some(KeyStorage::File)).is_err());
        assert!(matches!(
            LocalHostRegistry::new(second.clone())
                .resolve(&second)
                .unwrap()
                .state,
            LocalHostState::Starting
        ));
    }

    #[test]
    fn corrupt_registration_does_not_provision_a_replacement_host() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().to_owned());
        let other = fixture.path().join("other");
        fs::write(fixture.path().join("host-instance.json"), "invalid").unwrap();
        assert!(registry.acquire(&other, None).is_err());
        assert!(!other.exists());
    }

    #[tokio::test]
    async fn a_process_crash_releases_the_reservation_without_reusing_its_ticket() {
        const CHILD_ROOT: &str = "BEX_LOCAL_HOST_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let registry = LocalHostRegistry::new(root.join("registry"));
            let _lease = registry.acquire(&root.join("first"), None).unwrap();
            fs::write(root.join("acquired"), "").unwrap();
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).unwrap();
            return;
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path();
            let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "local_host::tests::a_process_crash_releases_the_reservation_without_reusing_its_ticket"])
                .env(CHILD_ROOT, root).stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null()).kill_on_drop(true).spawn().unwrap();
            while !root.join("acquired").exists() {
                assert!(child.try_wait().unwrap().is_none(), "child exited before reserving Host");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let registry = LocalHostRegistry::new(root.join("registry"));
            assert!(registry.acquire(&root.join("second"), None).is_err());
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            assert!(matches!(registry.resolve(&root.join("second")).unwrap().state, LocalHostState::Stopped));
            let _lease = registry.acquire(&root.join("second"), None).unwrap();
            assert!(matches!(registry.resolve(&root.join("first")).unwrap().state, LocalHostState::Starting));
        }).await.expect("process lock recovery deadline");
    }
}
