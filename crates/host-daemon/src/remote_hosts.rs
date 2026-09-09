use std::{collections::BTreeMap, sync::Arc, time::Duration};

use agent_core::client::{MobileClient, MobileClientConfig};
use host_protocol::{CURRENT_PROTOCOL_VERSION, Ed25519PublicKey, PairingQrPayload, RelayEndpoint};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// Atomic credential record storage. Production uses Keychain; isolated tests
/// supply an in-memory store and never read the developer's credentials.
pub trait RemoteCredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String>;
    fn save(&self, bytes: &[u8]) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
pub struct KeychainRemoteStore {
    account: String,
}

#[cfg(target_os = "macos")]
impl KeychainRemoteStore {
    pub fn new(account: String) -> Self {
        Self { account }
    }
}

#[cfg(target_os = "macos")]
impl RemoteCredentialStore for KeychainRemoteStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        match security_framework::passwords::get_generic_password(
            "app.bex.remote-hosts.v4",
            &self.account,
        ) {
            Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
            Err(error) if error.code() == security_framework_sys::base::errSecItemNotFound => {
                Ok(None)
            }
            Err(error) => Err(format!("Keychain read failed: {}", error.code())),
        }
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        security_framework::passwords::set_generic_password(
            "app.bex.remote-hosts.v4",
            &self.account,
            bytes,
        )
        .map_err(|error| format!("Keychain write failed: {}", error.code()))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHostProfile {
    pub id: String,
    pub host_name: String,
    pub host_identity: Ed25519PublicKey,
    pub relay_url: String,
    pub runner_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedHost {
    profile: RemoteHostProfile,
    relay: RelayEndpoint,
    // Serialized only into the secure credential store, never a UI response.
    key: Vec<u8>,
}

impl Drop for SavedHost {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
    }
}

pub struct RemoteHosts {
    store: Arc<dyn RemoteCredentialStore>,
    hosts: Mutex<BTreeMap<String, SavedHost>>,
}

impl RemoteHosts {
    pub fn load(store: Arc<dyn RemoteCredentialStore>) -> Result<Self, String> {
        let hosts = match store.load()? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| "saved remote credentials are invalid")?,
            None => BTreeMap::new(),
        };
        Ok(Self {
            store,
            hosts: Mutex::new(hosts),
        })
    }

    pub async fn profiles(&self) -> Vec<RemoteHostProfile> {
        self.hosts
            .lock()
            .await
            .values()
            .map(|host| host.profile.clone())
            .collect()
    }

    pub async fn pair(
        &self,
        invitation: PairingQrPayload,
        device_name: String,
    ) -> Result<RemoteHostProfile, String> {
        if invitation.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err("pairing protocol version is unsupported".into());
        }
        if invitation.expires_at_ms <= crate::ssh_gateway::unix_time_millis() {
            return Err("pairing invitation has expired".into());
        }
        invitation
            .relay
            .validate()
            .map_err(|error| error.to_string())?;
        let id = invitation.host_identity.to_base64url();
        let mut hosts = self.hosts.lock().await;
        if hosts.len() >= 64 && !hosts.contains_key(&id) {
            return Err("remote Host limit reached".into());
        }
        let key = match hosts.get(&id) {
            Some(host) => Zeroizing::new(host.key.clone()),
            None => Zeroizing::new(
                Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .map_err(|_| "secure random generation failed")?
                    .as_ref()
                    .to_vec(),
            ),
        };
        let config = MobileClientConfig {
            relay: invitation.relay.clone(),
            host_identity: invitation.host_identity,
            device_name,
            pairing_ticket: Some(invitation.ticket),
            request_timeout: Duration::from_secs(20),
        };
        config.validate().map_err(|error| error.to_string())?;
        let profile = RemoteHostProfile {
            id: id.clone(),
            host_name: invitation.host_name,
            host_identity: invitation.host_identity,
            relay_url: invitation.relay.relay_url.clone(),
            runner_id: invitation.relay.runner_id.clone(),
        };
        // Persist the key before consuming the invitation remotely. If the
        // connection is interrupted after pairing, reconnect uses the saved key.
        let mut updated = hosts.clone();
        updated.insert(
            id,
            SavedHost {
                profile: profile.clone(),
                relay: invitation.relay,
                key: key.to_vec(),
            },
        );
        self.persist(&updated)?;
        *hosts = updated;
        drop(hosts);
        let client = MobileClient::connect(config, &key)
            .await
            .map_err(|error| error.to_string())?;
        client.close();
        Ok(profile)
    }

    pub(crate) async fn transfer(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use std::path::PathBuf;
        #[derive(Deserialize)]
        #[serde(tag = "direction", rename_all = "camelCase")]
        enum Transfer {
            Upload {
                source: PathBuf,
                directory: PathBuf,
                #[serde(rename = "fileName")]
                file_name: String,
            },
            Download {
                source: PathBuf,
                destination: PathBuf,
            },
        }
        let id = params
            .get("profileId")
            .and_then(serde_json::Value::as_str)
            .ok_or("remote Host ID is required")?;
        let (config, key) = self.connection(id).await?;
        let params: Transfer =
            serde_json::from_value(params).map_err(|_| "invalid transfer parameters")?;
        let client = MobileClient::connect(config, &key)
            .await
            .map_err(|error| error.to_string())?;
        match params {
            Transfer::Upload {
                source,
                directory,
                file_name,
            } => client
                .upload_file(&source, &directory, &file_name)
                .await
                .map_err(|error| error.to_string()),
            Transfer::Download {
                source,
                destination,
            } => {
                client
                    .download_file(&source, &destination)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(serde_json::json!({"path":destination}))
            }
        }
    }

    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let mut hosts = self.hosts.lock().await;
        let mut updated = hosts.clone();
        updated.remove(id);
        self.persist(&updated)?;
        *hosts = updated;
        Ok(())
    }

    pub(crate) async fn connection(
        &self,
        id: &str,
    ) -> Result<(MobileClientConfig, Zeroizing<Vec<u8>>), String> {
        let hosts = self.hosts.lock().await;
        let host = hosts.get(id).ok_or("remote Host profile is unavailable")?;
        Ok((
            MobileClientConfig {
                relay: host.relay.clone(),
                host_identity: host.profile.host_identity,
                device_name: "Mac".into(),
                pairing_ticket: None,
                request_timeout: Duration::from_secs(20),
            },
            Zeroizing::new(host.key.clone()),
        ))
    }

    fn persist(&self, hosts: &BTreeMap<String, SavedHost>) -> Result<(), String> {
        let bytes = Zeroizing::new(serde_json::to_vec(hosts).map_err(|error| error.to_string())?);
        self.store.save(&bytes)
    }
}
