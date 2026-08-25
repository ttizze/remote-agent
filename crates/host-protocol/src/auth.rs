use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const PAIRING_TOKEN_BYTES: usize = 32;

/// A stable error returned when an SSH-mapped base64url value is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Base64UrlError {
    #[error("invalid base64url: {0}")]
    InvalidEncoding(String),
    #[error("{kind} must be {expected} bytes, got {actual}")]
    InvalidLength {
        kind: &'static str,
        expected: usize,
        actual: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairingToken([u8; PAIRING_TOKEN_BYTES]);

impl PairingToken {
    pub const fn from_bytes(bytes: [u8; PAIRING_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; PAIRING_TOKEN_BYTES] {
        &self.0
    }

    /// Encodes this token using unpadded URL-safe base64.
    ///
    /// The alphabet is safe for the SSH username mapping used by the host;
    /// unlike Debug, this method intentionally returns the token itself.
    pub fn to_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Decodes an unpadded URL-safe base64 token.
    pub fn from_base64url(encoded: &str) -> Result<Self, Base64UrlError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|error| Base64UrlError::InvalidEncoding(error.to_string()))?;
        let actual = bytes.len();
        let bytes = bytes
            .try_into()
            .map_err(|_: Vec<u8>| Base64UrlError::InvalidLength {
                kind: "pairing token",
                expected: PAIRING_TOKEN_BYTES,
                actual,
            })?;
        Ok(Self(bytes))
    }
}

impl fmt::Debug for PairingToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingToken([redacted])")
    }
}

impl Serialize for PairingToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for PairingToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::from_base64url(&encoded).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ed25519PublicKey([u8; ED25519_PUBLIC_KEY_BYTES]);

impl Ed25519PublicKey {
    pub const fn from_bytes(bytes: [u8; ED25519_PUBLIC_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; ED25519_PUBLIC_KEY_BYTES] {
        &self.0
    }

    /// Encodes this public key using unpadded URL-safe base64.
    pub fn to_base64url(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Decodes an unpadded URL-safe base64 public key.
    pub fn from_base64url(encoded: &str) -> Result<Self, Base64UrlError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|error| Base64UrlError::InvalidEncoding(error.to_string()))?;
        let actual = bytes.len();
        let bytes = bytes
            .try_into()
            .map_err(|_: Vec<u8>| Base64UrlError::InvalidLength {
                kind: "Ed25519 public key",
                expected: ED25519_PUBLIC_KEY_BYTES,
                actual,
            })?;
        Ok(Self(bytes))
    }
}

impl Serialize for Ed25519PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_base64url())
    }
}

impl<'de> Deserialize<'de> for Ed25519PublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::from_base64url(&encoded).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingQrPayload {
    pub protocol_version: u16,
    pub host_identity: Ed25519PublicKey,
    pub addresses: Vec<String>,
    pub ticket: PairingToken,
    pub expires_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CURRENT_PROTOCOL_VERSION;
    use serde_json::json;

    #[test]
    fn pairing_payload_uses_fixed_length_base64url_values() {
        let payload = PairingQrPayload {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            host_identity: Ed25519PublicKey::from_bytes([7; ED25519_PUBLIC_KEY_BYTES]),
            addresses: vec!["192.0.2.1:49152".to_owned()],
            ticket: PairingToken::from_bytes([9; PAIRING_TOKEN_BYTES]),
            expires_at_ms: 1234,
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("[9,9"));
        assert!(!json.contains('='));
        assert_eq!(
            serde_json::from_str::<PairingQrPayload>(&json).unwrap(),
            payload
        );
    }

    #[test]
    fn pairing_payload_rejects_wrong_length_values() {
        let error = serde_json::from_value::<PairingQrPayload>(json!({
            "protocolVersion": CURRENT_PROTOCOL_VERSION,
            "hostIdentity": URL_SAFE_NO_PAD.encode([1; 31]),
            "addresses": [],
            "ticket": URL_SAFE_NO_PAD.encode([2; PAIRING_TOKEN_BYTES]),
            "expiresAtMs": 1
        }))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Ed25519 public key must be 32 bytes")
        );
    }

    #[test]
    fn ssh_mapping_helpers_round_trip_and_reject_bad_lengths() {
        let token = PairingToken::from_bytes([9; PAIRING_TOKEN_BYTES]);
        let key = Ed25519PublicKey::from_bytes([7; ED25519_PUBLIC_KEY_BYTES]);

        assert_eq!(
            PairingToken::from_base64url(&token.to_base64url()),
            Ok(token)
        );
        assert_eq!(
            Ed25519PublicKey::from_base64url(&key.to_base64url()),
            Ok(key)
        );
        assert!(!format!("{:?}", token).contains(&token.to_base64url()));
        assert!(matches!(
            PairingToken::from_base64url(&URL_SAFE_NO_PAD.encode([1; 31])),
            Err(Base64UrlError::InvalidLength { .. })
        ));
        assert!(matches!(
            Ed25519PublicKey::from_base64url("not base64url"),
            Err(Base64UrlError::InvalidEncoding(_))
        ));
    }
}
