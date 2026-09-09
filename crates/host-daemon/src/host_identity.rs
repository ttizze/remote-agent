use agent_core::transport::{Identity, NodeId, Trust};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::{Zeroize, Zeroizing};

use crate::remote_hosts::RemoteHostProfile;

/// One atomic secure record; fixtures inject isolated storage at this boundary.
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

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub host_key: [u8; 32],
    pub local_key: [u8; 32],
    pub trust: Trust,
    pub remotes: BTreeMap<NodeId, RemoteHostProfile>,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.host_key.zeroize();
        self.local_key.zeroize();
    }
}

pub struct HostCredentials {
    store: Arc<dyn CredentialStore>,
    pub(crate) record: tokio::sync::Mutex<Record>,
}
impl HostCredentials {
    pub fn load(store: Arc<dyn CredentialStore>) -> Result<Self, String> {
        let record = match store.load()? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| "saved Host credentials are invalid")?
            }
            None => {
                let local = Identity::generate();
                let mut trust = Trust::default();
                trust.allowed.insert(local.node_id());
                let record = Record {
                    host_key: Identity::generate().to_bytes(),
                    local_key: local.to_bytes(),
                    trust,
                    remotes: BTreeMap::new(),
                };
                store.save(&Zeroizing::new(
                    serde_json::to_vec(&record).map_err(|error| error.to_string())?,
                ))?;
                record
            }
        };
        Ok(Self {
            store,
            record: tokio::sync::Mutex::new(record),
        })
    }
    pub async fn host_identity(&self) -> Identity {
        Identity::from_bytes(self.record.lock().await.host_key)
    }
    pub async fn local_identity(&self) -> Identity {
        Identity::from_bytes(self.record.lock().await.local_key)
    }
    /// Persist before publishing. Failed writes leave the live allowlist unchanged.
    pub(crate) fn persist(&self, record: &Record) -> Result<(), String> {
        self.store.save(&Zeroizing::new(
            serde_json::to_vec(record).map_err(|error| error.to_string())?,
        ))
    }
}
