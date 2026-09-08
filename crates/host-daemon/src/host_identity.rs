use host_protocol::Ed25519PublicKey;
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use russh::keys::{Algorithm, PrivateKey};
use zeroize::Zeroizing;

/// One long-lived Host key. Cloning its SSH representation is necessary only
/// when constructing the one server configuration shared by all connections.
pub struct HostIdentity {
    key: PrivateKey,
    public: Ed25519PublicKey,
}

impl HostIdentity {
    pub fn public_key(&self) -> Ed25519PublicKey {
        self.public
    }
    pub fn server_key(&self) -> PrivateKey {
        self.key.clone()
    }

    /// In-memory construction also permits isolated fixture identities without
    /// reading or changing the developer's real Keychain.
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, HostIdentityError> {
        let key = russh::keys::pkcs8::decode_pkcs8(bytes, None)
            .map_err(|_| HostIdentityError::InvalidKey)?;
        if key.algorithm() != Algorithm::Ed25519 {
            return Err(HostIdentityError::InvalidKey);
        }
        let public = key
            .public_key()
            .key_data()
            .ed25519()
            .ok_or(HostIdentityError::InvalidKey)?;
        let public = Ed25519PublicKey::from_bytes(*public.as_ref());
        Ok(Self { key, public })
    }

    #[cfg(target_os = "macos")]
    pub fn load_or_create(service: &str, account: &str) -> Result<Self, HostIdentityError> {
        use security_framework::passwords::{get_generic_password, set_generic_password};
        use security_framework_sys::base::errSecItemNotFound;
        match get_generic_password(service, account) {
            Ok(bytes) => Self::from_pkcs8(&Zeroizing::new(bytes)),
            Err(error) if error.code() == errSecItemNotFound => {
                let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                    .map_err(|_| HostIdentityError::Random)?;
                let identity = Self::from_pkcs8(document.as_ref())?;
                set_generic_password(service, account, document.as_ref())
                    .map_err(|error| HostIdentityError::Keychain(error.code()))?;
                Ok(identity)
            }
            Err(error) => Err(HostIdentityError::Keychain(error.code())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostIdentityError {
    #[error("stored Host identity is not a valid Ed25519 PKCS#8 key")]
    InvalidKey,
    #[error("secure random generation failed")]
    Random,
    #[error("macOS Keychain operation failed with status {0}")]
    Keychain(i32),
}
