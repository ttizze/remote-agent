use agent_transport::transport::{Identity, NodeId, Trust};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};
use zeroize::Zeroizing;

use agent_protocol::models::RemoteHost;

/// Stores only the two identity keys (64 bytes), never growing Host metadata.
/// Fixtures inject isolated storage at this boundary.
pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn save(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum KeyStorage {
    #[default]
    Keyring,
    File,
}

impl KeyStorage {
    pub fn open(self, directory: &std::path::Path) -> Result<Arc<dyn CredentialStore>> {
        match self {
            Self::Keyring => Ok(Arc::new(KeyringStore::new(
                directory.to_str().context("state directory is not UTF-8")?,
            )?)),
            Self::File => Ok(Arc::new(FileKeyStore(directory.join("identity.keys")))),
        }
    }
}

pub struct KeyringStore(keyring::Entry);
impl KeyringStore {
    pub fn new(account: &str) -> Result<Self> {
        Ok(Self(keyring::Entry::new("app.bex.host", account)?))
    }
}
impl CredentialStore for KeyringStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match self.0.get_secret() {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<()> {
        self.0.set_secret(bytes).map_err(Into::into)
    }
}

/// Read only the local identity already provisioned by the daemon.
/// OS-backed stores must be read on a blocking worker.
pub fn load_local_identity(store: &dyn CredentialStore) -> Result<Identity> {
    let bytes = store
        .load()?
        .context("local Host credentials are not provisioned")?;
    ensure!(
        bytes.len() == 64,
        "saved Host keys must contain exactly 64 bytes"
    );
    Ok(Identity::from_bytes(bytes[32..].try_into().unwrap()))
}

/// Explicit headless-server alternative to the OS keyring. Keep the containing
/// directory private; Unix files are 0600, Windows inherits its user DACL.
pub struct FileKeyStore(pub PathBuf);
impl CredentialStore for FileKeyStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        match std::fs::read(&self.0) {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<()> {
        atomicwrites::AtomicFile::new(&self.0, atomicwrites::DisallowOverwrite)
            .write_with_options(
                |file| file.write_all(bytes),
                crate::platform::private_file_options(),
            )
            .map_err(Into::into)
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Record {
    pub trust: Trust,
    pub remotes: BTreeMap<NodeId, RemoteHost>,
}

pub struct HostCredentials {
    keys: Zeroizing<[u8; 64]>,
    record_path: PathBuf,
    pub(crate) record: tokio::sync::Mutex<Record>,
}
impl HostCredentials {
    /// Keyring access can block for an OS dialog. Never run it on a Tokio worker.
    pub async fn load(store: Arc<dyn CredentialStore>, directory: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            crate::platform::create_state_directory(&directory)?;
            let record_path = directory.join("trust.json");
            let saved_record = match std::fs::read(&record_path) {
                Ok(bytes) => Some(
                    serde_json::from_slice::<Record>(&bytes)
                        .context("saved Host trust is invalid")?,
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let keys = match store.load()? {
                Some(bytes) => Zeroizing::new(
                    bytes
                        .as_slice()
                        .try_into()
                        .context("saved Host keys must contain exactly 64 bytes")?,
                ),
                None => {
                    ensure!(
                        saved_record.is_none(),
                        "Host trust exists but identity keys are missing"
                    );
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
                    crate::platform::save_private_json(&record_path, &record)?;
                    record
                }
            };
            Ok(Self {
                keys,
                record_path,
                record: tokio::sync::Mutex::new(record),
            })
        })
        .await?
    }
    pub async fn host_identity(&self) -> Identity {
        Identity::from_bytes(self.keys[..32].try_into().unwrap())
    }
    pub async fn local_identity(&self) -> Identity {
        Identity::from_bytes(self.keys[32..].try_into().unwrap())
    }
    /// Persist before publishing. Failed writes leave the live allowlist unchanged.
    pub(crate) async fn persist(&self, record: Record) -> Result<Record> {
        let path = self.record_path.clone();
        tokio::task::spawn_blocking(move || {
            crate::platform::save_private_json(&path, &record)?;
            Ok(record)
        })
        .await?
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
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
            assert_ne!(
                std::thread::current().id(),
                self.worker,
                "keyring access must leave the async worker"
            );
            Ok(self.bytes.lock().unwrap().clone())
        }
        fn save(&self, bytes: &[u8]) -> Result<()> {
            assert_ne!(std::thread::current().id(), self.worker);
            if bytes.len() > 2560 {
                return Err(anyhow::anyhow!("Windows credential limit"));
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
            HostCredentials::load(store.clone(), directory.path().to_owned())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("keys are missing")
        );
        std::fs::write(directory.path().join("trust.json"), "invalid trust record").unwrap();
        let error = HostCredentials::load(store, directory.path().to_owned())
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "saved Host trust is invalid");
        assert!(
            error.downcast_ref::<serde_json::Error>().is_some(),
            "{error:#}"
        );
    }
}
