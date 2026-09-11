use agent_core::transport::{Identity, NodeId, Trust};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};
use zeroize::Zeroizing;

use agent_core::models::RemoteHost;

/// Stores only the two identity keys (64 bytes), never growing Host metadata.
/// Fixtures inject isolated storage at this boundary.
pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
}

pub struct KeyringStore(keyring::Entry);
impl KeyringStore {
    pub fn new(account: &str) -> Result<Self, String> {
        keyring::Entry::new("app.bex.host", account)
            .map(Self)
            .map_err(|error| error.to_string())
    }
}
impl CredentialStore for KeyringStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        match self.0.get_secret() {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        self.0.set_secret(bytes).map_err(|error| error.to_string())
    }
}

/// Read only the local identity already provisioned by the daemon.
/// OS-backed stores must be read on a blocking worker.
pub fn load_local_identity(store: &dyn CredentialStore) -> Result<Identity, String> {
    let bytes = store
        .load()?
        .ok_or("local Host credentials are not provisioned")?;
    if bytes.len() != 64 {
        return Err("saved Host keys must contain exactly 64 bytes".into());
    }
    Ok(Identity::from_bytes(bytes[32..].try_into().unwrap()))
}

/// Explicit headless-server alternative to the OS keyring. Keep the containing
/// directory private; Unix files are 0600, Windows inherits its user DACL.
pub struct FileKeyStore(pub PathBuf);
impl CredentialStore for FileKeyStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        match std::fs::read(&self.0) {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        atomicwrites::AtomicFile::new(&self.0, atomicwrites::DisallowOverwrite)
            .write_with_options(
                |file| file.write_all(bytes),
                crate::platform::private_file_options(),
            )
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Record {
    pub trust: Trust,
    pub remotes: BTreeMap<NodeId, RemoteHost>,
}
fn save_record(path: &std::path::Path, record: &Record) -> Result<(), String> {
    crate::platform::save_private_json(path, record)
}

pub struct HostCredentials {
    keys: Zeroizing<[u8; 64]>,
    record_path: PathBuf,
    pub(crate) record: tokio::sync::Mutex<Record>,
}
impl HostCredentials {
    /// Keyring access can block for an OS dialog. Never run it on a Tokio worker.
    pub async fn load(store: Arc<dyn CredentialStore>, directory: PathBuf) -> Result<Self, String> {
        tokio::task::spawn_blocking(move || {
            crate::platform::create_state_directory(&directory).map_err(|e| e.to_string())?;
            let record_path = directory.join("trust.json");
            let saved_record = match std::fs::read(&record_path) {
                Ok(bytes) => Some(
                    serde_json::from_slice::<Record>(&bytes)
                        .map_err(|_| "saved Host trust is invalid")?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            let keys = match store.load()? {
                Some(bytes) => Zeroizing::new(
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| "saved Host keys must contain exactly 64 bytes")?,
                ),
                None => {
                    if saved_record.is_some() {
                        return Err("Host trust exists but identity keys are missing".into());
                    }
                    let mut keys = Zeroizing::new([0; 64]);
                    keys[..32].copy_from_slice(&Identity::generate().to_bytes());
                    keys[32..].copy_from_slice(&Identity::generate().to_bytes());
                    store.save(keys.as_slice())?;
                    keys
                }
            };
            let record = match saved_record {
                Some(record) => record,
                None => {
                    let mut record = Record::default();
                    record
                        .trust
                        .allowed
                        .insert(Identity::from_bytes(keys[32..].try_into().unwrap()).node_id());
                    save_record(&record_path, &record)?;
                    record
                }
            };
            Ok(Self {
                keys,
                record_path,
                record: tokio::sync::Mutex::new(record),
            })
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub async fn host_identity(&self) -> Identity {
        Identity::from_bytes(self.keys[..32].try_into().unwrap())
    }
    pub async fn local_identity(&self) -> Identity {
        Identity::from_bytes(self.keys[32..].try_into().unwrap())
    }
    /// Persist before publishing. Failed writes leave the live allowlist unchanged.
    pub(crate) async fn persist(&self, record: Record) -> Result<Record, String> {
        let path = self.record_path.clone();
        tokio::task::spawn_blocking(move || {
            save_record(&path, &record)?;
            Ok(record)
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct BoundedKeyring {
        worker: std::thread::ThreadId,
        bytes: Mutex<Option<Zeroizing<Vec<u8>>>>,
        writes: AtomicUsize,
    }
    impl CredentialStore for BoundedKeyring {
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
            assert_ne!(
                std::thread::current().id(),
                self.worker,
                "keyring access must leave the async worker"
            );
            Ok(self.bytes.lock().unwrap().clone())
        }
        fn save(&self, bytes: &[u8]) -> Result<(), String> {
            assert_ne!(std::thread::current().id(), self.worker);
            if bytes.len() > 2560 {
                return Err("Windows credential limit".into());
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            *self.bytes.lock().unwrap() = Some(Zeroizing::new(bytes.to_vec()));
            Ok(())
        }
    }
    #[tokio::test]
    async fn trust_growth_never_rewrites_or_enlarges_keyring_record() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(BoundedKeyring {
            worker: std::thread::current().id(),
            bytes: Mutex::new(None),
            writes: AtomicUsize::new(0),
        });
        let credentials = HostCredentials::load(store.clone(), directory.path().to_owned())
            .await
            .unwrap();
        let host = credentials.host_identity().await.node_id();
        let local = credentials.local_identity().await.node_id();
        let mut record = credentials.record.lock().await;
        let mut next = record.clone();
        for _ in 0..200 {
            next.trust.allowed.insert(Identity::generate().node_id());
        }
        *record = credentials.persist(next).await.unwrap();
        assert_eq!(store.bytes.lock().unwrap().as_ref().unwrap().len(), 64);
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        let path = directory.path().join("trust.json");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 2560);
        let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(saved.get("host_key").is_none() && saved.get("local_key").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let restored = HostCredentials::load(store.clone(), directory.path().to_owned())
            .await
            .unwrap();
        assert_eq!(restored.host_identity().await.node_id(), host);
        assert_eq!(restored.local_identity().await.node_id(), local);
        assert_eq!(
            restored.record.lock().await.trust.allowed,
            record.trust.allowed
        );
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn file_keys_restore_identity_without_a_keyring_service() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.keys");
        let store = Arc::new(FileKeyStore(path.clone()));
        let credentials = HostCredentials::load(store.clone(), directory.path().to_owned())
            .await
            .unwrap();
        let restored = HostCredentials::load(store.clone(), directory.path().to_owned())
            .await
            .unwrap();
        assert_eq!(
            credentials.host_identity().await.node_id(),
            restored.host_identity().await.node_id()
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
        assert!(
            HostCredentials::load(store, directory.path().to_owned())
                .await
                .err()
                .unwrap()
                .contains("keys are missing")
        );
    }
}
