//! Local discovery shares one normal Host across credential directories. The
//! short registry lock serializes discovery/startup; host.lock owns the process
//! lifetime. A stale registry survives crashes without reviving a stale ticket.
use crate::KeyStorage;
use agent_core::transport::{Identity, Ticket};
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
    _lock: File,
}

impl LocalHostRegistry {
    /// A scoped registry for explicitly isolated development/test instances.
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            legacy_directory: None,
        }
    }

    pub fn for_user() -> Result<Self, String> {
        let directory = directories::ProjectDirs::from("app", "bex", "BEX")
            .ok_or("application data directory unavailable")?
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
    pub fn resolve(&self, preferred: &Path) -> Result<LocalHost, String> {
        let _coordination = self.coordinate()?;
        self.resolve_locked(preferred)
    }

    pub fn acquire(
        &self,
        directory: &Path,
        key_storage: Option<KeyStorage>,
    ) -> Result<HostLease, String> {
        let _coordination = self.coordinate()?;
        let current = self.resolve_locked(directory)?;
        if !matches!(current.state, LocalHostState::Stopped) {
            return Err(format!(
                "Host is already running in {}; use the existing Host",
                current.directory.display()
            ));
        }
        crate::platform::create_state_directory(directory).map_err(|e| e.to_string())?;
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
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
            .map_err(|e| format!("Host is already running or its lock is unavailable: {e}"))?;
        self.save(&Registration {
            directory: directory.clone(),
            key_storage: Some(key_storage),
            ready: false,
        })?;
        Ok(HostLease {
            directory,
            key_storage,
            registry: self.clone(),
            _lock: lock,
        })
    }

    fn coordinate(&self) -> Result<File, String> {
        crate::platform::create_state_directory(&self.directory).map_err(|e| e.to_string())?;
        let lock = open_lock(&self.directory.join("host-instance.lock"))?;
        lock.lock().map_err(|e| e.to_string())?;
        Ok(lock)
    }

    fn resolve_locked(&self, preferred: &Path) -> Result<LocalHost, String> {
        let registration = match fs::read(self.directory.join("host-instance.json")) {
            Ok(bytes) => {
                let registration: Registration = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("invalid local Host registration: {e}"))?;
                if !registration.directory.is_absolute() {
                    return Err("local Host registration requires an absolute directory".into());
                }
                Some(registration)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
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
                let directory = directory.canonicalize().map_err(|e| e.to_string())?;
                if !active.contains(&directory) {
                    active.push(directory);
                }
            }
        }
        if active.len() > 1 {
            return Err(format!(
                "Multiple local Hosts are running in {}. Keep one Host after its work finishes; Bex will not choose or stop one automatically.",
                active
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
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

    fn save(&self, registration: &Registration) -> Result<(), String> {
        crate::platform::save_private_json(&self.directory.join("host-instance.json"), registration)
    }
}

impl Registration {
    fn location(&self) -> Result<LocalHost, String> {
        let state = if self.ready {
            match fs::read_to_string(self.directory.join("host.ticket")) {
                Ok(ticket) => LocalHostState::Ready(
                    ticket.trim().parse::<Ticket>().map_err(|e| e.to_string())?,
                ),
                Err(error) if error.kind() == io::ErrorKind::NotFound => LocalHostState::Starting,
                Err(error) => return Err(error.to_string()),
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
    pub fn isolated(directory: &Path, key_storage: Option<KeyStorage>) -> Result<Self, String> {
        LocalHostRegistry::new(directory.to_owned()).acquire(directory, key_storage)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn key_storage(&self) -> KeyStorage {
        self.key_storage
    }

    pub fn publish(&self, ticket: &Ticket) -> Result<(), String> {
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
        )
        .map_err(|error| error.to_string())?;
        self.registry.save(&Registration {
            directory: self.directory.clone(),
            key_storage: Some(self.key_storage),
            ready: true,
        })
    }
}

impl LocalHost {
    /// Read the discovered Host's identity without provisioning a second one.
    pub fn load_identity(&self) -> Result<Identity, String> {
        let store = self.key_storage.unwrap_or_default().open(&self.directory)?;
        crate::load_local_identity(store.as_ref())
    }
}

fn existing_key_storage(directory: &Path) -> Result<Option<KeyStorage>, String> {
    if directory
        .join("identity.keys")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        Ok(Some(KeyStorage::File))
    } else if directory
        .join("trust.json")
        .try_exists()
        .map_err(|error| error.to_string())?
    {
        Ok(Some(KeyStorage::Keyring))
    } else {
        Ok(None)
    }
}

fn open_lock(path: &Path) -> Result<File, String> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())
}

fn running(directory: &Path) -> Result<bool, String> {
    let lock = match File::options()
        .read(true)
        .write(true)
        .open(directory.join("host.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(fs::TryLockError::WouldBlock) => Ok(true),
        Err(fs::TryLockError::Error(error)) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::transport::{Endpoint, Identity, Relays};

    #[tokio::test]
    async fn discovery_restores_the_hosts_file_backend_and_identity_after_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let host = fixture.path().join("mobile-host");
        let desktop = fixture.path().join("desktop");
        let lease = registry.acquire(&host, Some(KeyStorage::File)).unwrap();
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
        let lease = registry.acquire(&location.directory, None).unwrap();
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
                .contains("Multiple local Hosts")
        );
        assert!(
            registry
                .acquire(&fixture.path().join("third"), None)
                .is_err()
        );
        assert!(!fixture.path().join("third").exists());
    }

    #[test]
    fn different_state_directories_cannot_start_two_normal_hosts() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let first = fixture.path().join("first");
        let second = fixture.path().join("second");
        let _running = registry.acquire(&first, None).unwrap();
        assert!(
            registry.acquire(&second, None).is_err(),
            "a different credential directory must not create a second normal Host"
        );
        assert!(
            !second.exists(),
            "reject before provisioning another identity"
        );
    }

    #[tokio::test]
    async fn startup_hides_stale_tickets_and_restart_remembers_the_credential_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let custom = fixture.path().join("custom");
        let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let lease = registry.acquire(&custom, None).unwrap();
        fs::write(custom.join("host.ticket"), "stale invalid ticket").unwrap();
        assert!(matches!(
            registry.resolve(registry.directory()).unwrap().state,
            LocalHostState::Starting
        ));
        lease.publish(&endpoint.ticket()).unwrap();
        let location = registry.resolve(registry.directory()).unwrap();
        assert_eq!(location.directory, custom.canonicalize().unwrap());
        assert!(
            matches!(location.state, LocalHostState::Ready(ticket) if ticket == endpoint.ticket())
        );
        drop(lease);
        let registry = LocalHostRegistry::new(registry.directory().to_owned());
        let location = registry.resolve(registry.directory()).unwrap();
        assert!(matches!(location.state, LocalHostState::Stopped));
        assert_eq!(location.directory, custom.canonicalize().unwrap());
        let lease = registry.acquire(&location.directory, None).unwrap();
        assert!(matches!(
            registry.resolve(registry.directory()).unwrap().state,
            LocalHostState::Starting
        ));
        drop(lease);
        endpoint.close().await;
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
                .contains("Multiple local Hosts")
        );
        assert!(!registry.directory.join("host-instance.json").exists());
        drop(other);
        let location = registry.resolve(registry.directory()).unwrap();
        assert_eq!(location.directory, legacy.canonicalize().unwrap());
        assert!(
            matches!(location.state, LocalHostState::Ready(ticket) if ticket == endpoint.ticket())
        );
        assert!(registry.acquire(registry.directory(), None).is_err());
        assert_eq!(fs::read(legacy.join("identity.keys")).unwrap(), keys);
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
