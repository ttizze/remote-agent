//! Local discovery shares one normal Host across credential directories. The
//! short registry lock serializes discovery/startup; host.lock owns the process
//! lifetime. A stale registry survives crashes without reviving a stale ticket.
use agent_transport::transport::Ticket;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub struct LocalHostRegistry {
    directory: PathBuf,
}

pub struct LocalHost {
    pub directory: PathBuf,
    pub state: LocalHostState,
}

pub enum LocalHostState {
    Stopped,
    Starting,
    Ready(Ticket),
}

#[derive(Serialize, Deserialize)]
struct Registration {
    directory: PathBuf,
    ready: bool,
}

/// Keep this lease until Codex and the Host have both stopped.
pub struct HostLease {
    directory: PathBuf,
    registry: LocalHostRegistry,
    _lock: FileLock,
}

struct FileLock(File);
impl Drop for FileLock {
    fn drop(&mut self) {
        // A concurrently forked child can retain this open file description
        // until exec. Releasing ownership must not wait for its descriptor.
        if let Err(error) = self.0.unlock() {
            tracing::error!(target: "bex", operation = "release local Host lock", message = %error);
        }
    }
}

impl LocalHostRegistry {
    /// A scoped registry for explicitly isolated development/test instances.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn for_user() -> Result<Self> {
        let directory = directories::ProjectDirs::from("app", "bex", "BEX")
            .context("application data directory unavailable")?
            .data_local_dir()
            .to_owned();
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reuse the registered Host and remember its directory after it stops.
    pub fn resolve(&self, preferred: &Path) -> Result<LocalHost> {
        let _coordination = self.coordinate()?;
        self.resolve_locked(preferred)
    }

    pub fn acquire(&self, directory: &Path) -> Result<HostLease> {
        let _coordination = self.coordinate()?;
        let current = self.resolve_locked(directory)?;
        if !matches!(current.state, LocalHostState::Stopped) {
            anyhow::bail!(
                "Host is already running in {}; use the existing Host",
                current.directory.display()
            );
        }
        crate::platform::create_state_directory(directory)?;
        let directory = dunce::canonicalize(directory)?;
        let lock = open_lock(&directory.join("host.lock"))?;
        lock.try_lock()
            .context("Host is already running or its lock is unavailable")?;
        self.save(&Registration {
            directory: directory.clone(),
            ready: false,
        })?;
        Ok(HostLease {
            directory,
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
        if let Some(registration) = &registration
            && running(&registration.directory)?
        {
            return registration.location();
        }
        let directory = registration
            .as_ref()
            .map_or(preferred, |entry| &entry.directory);
        Ok(LocalHost {
            directory: directory.to_owned(),
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
            state,
        })
    }
}

impl HostLease {
    pub fn isolated(directory: &Path) -> Result<Self> {
        LocalHostRegistry::new(directory.to_owned()).acquire(directory)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
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
            ready: true,
        })
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
    use agent_transport::transport::{Endpoint, Relays};

    #[cfg(unix)]
    #[test]
    fn stopping_host_releases_lock_even_with_an_inherited_descriptor() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let host = fixture.path().join("host");
        let lease = registry.acquire(&host).unwrap();
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
        let replacement = registry.acquire(&host).unwrap();
        assert!(registry.acquire(&host).is_err());
        drop(replacement);
    }

    #[tokio::test]
    async fn discovery_restores_the_hosts_identity_after_restart() {
        let fixture = tempfile::tempdir().unwrap();
        let registry = LocalHostRegistry::new(fixture.path().join("registry"));
        let host = fixture.path().join("mobile-host");
        let desktop = fixture.path().join("desktop");
        let lease = registry.acquire(&host).unwrap();
        fs::write(host.join("host.ticket"), "stale invalid ticket").unwrap();
        assert!(matches!(
            registry.resolve(&desktop).unwrap().state,
            LocalHostState::Starting
        ));
        let credentials = crate::HostCredentials::load(
            std::sync::Arc::new(crate::FileKeyStore(lease.directory().join("identity.keys"))),
            lease.directory().to_owned(),
        )
        .await
        .unwrap();
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
            .await
            .unwrap();
        lease.publish(&endpoint.local_ticket()).unwrap();
        let location = registry.resolve(&desktop).unwrap();
        assert_eq!(location.directory, dunce::canonicalize(&host).unwrap());
        assert!(
            matches!(location.state, LocalHostState::Ready(ref ticket) if *ticket == endpoint.local_ticket())
        );
        assert_eq!(
            crate::load_local_identity(&location.directory)
                .unwrap()
                .node_id(),
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
        assert_eq!(location.directory, dunce::canonicalize(&host).unwrap());
        let lease = registry.acquire(&location.directory).unwrap();
        assert!(matches!(
            registry.resolve(&desktop).unwrap().state,
            LocalHostState::Starting
        ));
        let restored = crate::HostCredentials::load(
            std::sync::Arc::new(crate::FileKeyStore(lease.directory().join("identity.keys"))),
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
                    registry.acquire(&directory)
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
            registry.acquire(&loser).is_err(),
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
        let _a = HostLease::isolated(&first).unwrap();
        let _b = HostLease::isolated(&second).unwrap();
        assert!(HostLease::isolated(&first).is_err());
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
        assert!(registry.acquire(&other).is_err());
        assert!(!other.exists());
    }

    #[tokio::test]
    async fn a_process_crash_releases_the_reservation_without_reusing_its_ticket() {
        const CHILD_ROOT: &str = "BEX_LOCAL_HOST_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let registry = LocalHostRegistry::new(root.join("registry"));
            let _lease = registry.acquire(&root.join("first")).unwrap();
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
            assert!(registry.acquire(&root.join("second")).is_err());
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            assert!(matches!(registry.resolve(&root.join("second")).unwrap().state, LocalHostState::Stopped));
            let _lease = registry.acquire(&root.join("second")).unwrap();
            assert!(matches!(registry.resolve(&root.join("first")).unwrap().state, LocalHostState::Starting));
        }).await.expect("process lock recovery deadline");
    }
}
