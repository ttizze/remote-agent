use host_protocol::Ed25519PublicKey;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use zeroize::Zeroizing;

/// Secure persistence seam for the stable SSH host identity.
pub trait HostIdentityKeyStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError>;
    fn save(&self, pkcs8: &[u8]) -> Result<(), KeyStoreError>;
}

/// The Ed25519 key is both the SSH host key and the identity printed in QR.
pub struct HostIdentity {
    key_pair: Ed25519KeyPair,
    ssh_key: russh::keys::PrivateKey,
}

impl HostIdentity {
    pub fn public_key(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key_pair.public_key().as_ref().try_into().expect(
            "ring Ed25519 public keys always have the protocol's fixed 32-byte representation",
        ))
    }

    pub fn server_key(&self) -> russh::keys::PrivateKey {
        self.ssh_key.clone()
    }
}

pub fn load_or_create_host_identity(
    store: &impl HostIdentityKeyStore,
) -> Result<HostIdentity, HostIdentityError> {
    if let Some(document) = store.load()? {
        return parse_identity(document.as_ref());
    }

    let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| HostIdentityError::Random)?;
    store.save(document.as_ref())?;
    parse_identity(document.as_ref())
}

fn parse_identity(document: &[u8]) -> Result<HostIdentity, HostIdentityError> {
    let key_pair = Ed25519KeyPair::from_pkcs8(document)
        .map_err(|_| HostIdentityError::InvalidStoredIdentity)?;
    let ssh_key = russh::keys::pkcs8::decode_pkcs8(document, None)
        .map_err(|_| HostIdentityError::InvalidStoredIdentity)?;
    if ssh_key.algorithm() != russh::keys::Algorithm::Ed25519 {
        return Err(HostIdentityError::InvalidStoredIdentity);
    }
    let ssh_public = ssh_key
        .public_key()
        .key_data()
        .ed25519()
        .ok_or(HostIdentityError::InvalidStoredIdentity)?;
    if ssh_public.as_ref() != key_pair.public_key().as_ref() {
        return Err(HostIdentityError::InvalidStoredIdentity);
    }
    Ok(HostIdentity { key_pair, ssh_key })
}

#[derive(Debug, thiserror::Error)]
pub enum HostIdentityError {
    #[error("host identity secure store failed: {0}")]
    KeyStore(#[from] KeyStoreError),
    #[error("stored host identity is not a valid Ed25519 PKCS#8 document")]
    InvalidStoredIdentity,
    #[error("secure random generation failed")]
    Random,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct KeyStoreError {
    message: String,
}

impl KeyStoreError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(target_os = "macos")]
pub struct MacOsKeychainHostIdentityStore {
    service: String,
    account: String,
}

#[cfg(target_os = "macos")]
impl MacOsKeychainHostIdentityStore {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
}

#[cfg(target_os = "macos")]
impl HostIdentityKeyStore for MacOsKeychainHostIdentityStore {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        use security_framework::passwords::get_generic_password;
        use security_framework_sys::base::errSecItemNotFound;

        match get_generic_password(&self.service, &self.account) {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(error) if error.code() == errSecItemNotFound => Ok(None),
            Err(error) => Err(KeyStoreError::new(format!(
                "macOS Keychain read failed with status {}",
                error.code()
            ))),
        }
    }

    fn save(&self, pkcs8: &[u8]) -> Result<(), KeyStoreError> {
        security_framework::passwords::set_generic_password(&self.service, &self.account, pkcs8)
            .map_err(|error| {
                KeyStoreError::new(format!(
                    "macOS Keychain write failed with status {}",
                    error.code()
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct MemoryKeyStore {
        value: Mutex<Option<Vec<u8>>>,
    }

    impl HostIdentityKeyStore for MemoryKeyStore {
        fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
            Ok(self.value.lock().unwrap().clone().map(Zeroizing::new))
        }

        fn save(&self, pkcs8: &[u8]) -> Result<(), KeyStoreError> {
            *self.value.lock().unwrap() = Some(pkcs8.to_vec());
            Ok(())
        }
    }

    #[test]
    fn creates_once_then_loads_the_same_ssh_host_identity() {
        let store = MemoryKeyStore::default();
        let first = load_or_create_host_identity(&store).unwrap();
        let second = load_or_create_host_identity(&store).unwrap();

        assert_eq!(first.public_key(), second.public_key());
        assert_eq!(
            first
                .server_key()
                .public_key()
                .key_data()
                .ed25519()
                .unwrap()
                .as_ref(),
            first.public_key().as_bytes()
        );
    }

    #[test]
    fn refuses_to_replace_a_corrupt_stored_identity() {
        let store = MemoryKeyStore {
            value: Mutex::new(Some(vec![1, 2, 3])),
        };
        assert!(matches!(
            load_or_create_host_identity(&store),
            Err(HostIdentityError::InvalidStoredIdentity)
        ));
        assert_eq!(*store.value.lock().unwrap(), Some(vec![1, 2, 3]));
    }
}
