use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

use crate::RelayEndpoint;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected a canonical base64url-encoded 32-byte {0}")]
pub struct Base64UrlError(&'static str);

fn decode_key(encoded: &str, kind: &'static str) -> Result<[u8; 32], Base64UrlError> {
    // Fixed-size decoding bounds work and avoids allocating for untrusted input.
    if encoded.len() != 43 {
        return Err(Base64UrlError(kind));
    }
    let mut bytes = [0; 32];
    match URL_SAFE_NO_PAD.decode_slice(encoded, &mut bytes) {
        Ok(32) => Ok(bytes),
        _ => Err(Base64UrlError(kind)),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairingToken([u8; 32]);

impl PairingToken {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
    pub fn to_base64url(&self) -> String { URL_SAFE_NO_PAD.encode(self.0) }
    pub fn from_base64url(encoded: &str) -> Result<Self, Base64UrlError> {
        decode_key(encoded, "pairing token").map(Self)
    }
}

impl fmt::Debug for PairingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("PairingToken([redacted])") }
}

impl Serialize for PairingToken {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for PairingToken {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_base64url(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ed25519PublicKey([u8; 32]);

impl Ed25519PublicKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
    pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
    pub fn to_base64url(&self) -> String { URL_SAFE_NO_PAD.encode(self.0) }
    pub fn from_base64url(encoded: &str) -> Result<Self, Base64UrlError> {
        decode_key(encoded, "Ed25519 public key").map(Self)
    }
}

impl Serialize for Ed25519PublicKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for Ed25519PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_base64url(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingQrPayload {
    pub protocol_version: u16,
    pub host_identity: Ed25519PublicKey,
    pub host_name: String,
    #[serde(flatten)]
    pub relay: RelayEndpoint,
    pub ticket: PairingToken,
    pub expires_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_keys_reject_wrong_lengths_padding_and_noncanonical_trailing_bits() {
        let key = Ed25519PublicKey::from_bytes([7; 32]);
        assert_eq!(Ed25519PublicKey::from_base64url(&key.to_base64url()).unwrap(), key);
        assert!(Ed25519PublicKey::from_base64url(&URL_SAFE_NO_PAD.encode([7; 31])).is_err());
        assert!(Ed25519PublicKey::from_base64url(&(key.to_base64url() + "=")).is_err());
        let mut invalid = URL_SAFE_NO_PAD.encode([0; 32]);
        invalid.pop();
        invalid.push('B');
        assert!(Ed25519PublicKey::from_base64url(&invalid).is_err());
        let ticket = PairingToken::from_bytes([9; 32]);
        assert_eq!(PairingToken::from_base64url(&ticket.to_base64url()).unwrap(), ticket);
        assert!(!format!("{ticket:?}").contains(&ticket.to_base64url()));
    }
}
