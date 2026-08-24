use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;
const TRANSPORT_CERTIFICATE_HASH_BYTES: usize = 32;
const PAIRING_TOKEN_BYTES: usize = 32;
const AUTHENTICATION_CHALLENGE_BYTES: usize = 32;
const CONNECTION_NONCE_BYTES: usize = 32;
const PAIRING_PROOF_CONTEXT: &[u8] = b"remote-agent pairing proof v1\0";
const AUTHENTICATION_PROOF_CONTEXT: &[u8] = b"remote-agent authentication proof v1\0";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairingToken([u8; PAIRING_TOKEN_BYTES]);

impl PairingToken {
    pub const fn from_bytes(bytes: [u8; PAIRING_TOKEN_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; PAIRING_TOKEN_BYTES] {
        &self.0
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
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for PairingToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "pairing token must be {PAIRING_TOKEN_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
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
}

impl Serialize for Ed25519PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Ed25519PublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "Ed25519 public key must be {ED25519_PUBLIC_KEY_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ConnectionNonce([u8; CONNECTION_NONCE_BYTES]);

impl ConnectionNonce {
    pub const fn from_bytes(bytes: [u8; CONNECTION_NONCE_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; CONNECTION_NONCE_BYTES] {
        &self.0
    }
}

impl fmt::Debug for ConnectionNonce {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ConnectionNonce([redacted])")
    }
}

impl Serialize for ConnectionNonce {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for ConnectionNonce {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "connection nonce must be {CONNECTION_NONCE_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ed25519Signature([u8; ED25519_SIGNATURE_BYTES]);

impl Ed25519Signature {
    pub const fn from_bytes(bytes: [u8; ED25519_SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; ED25519_SIGNATURE_BYTES] {
        &self.0
    }
}

impl fmt::Debug for Ed25519Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Ed25519Signature([redacted])")
    }
}

impl Serialize for Ed25519Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Ed25519Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "Ed25519 signature must be {ED25519_SIGNATURE_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportCertificateHash([u8; TRANSPORT_CERTIFICATE_HASH_BYTES]);

impl TransportCertificateHash {
    pub const fn from_bytes(bytes: [u8; TRANSPORT_CERTIFICATE_HASH_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; TRANSPORT_CERTIFICATE_HASH_BYTES] {
        &self.0
    }
}

impl Serialize for TransportCertificateHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for TransportCertificateHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "transport certificate hash must be {TRANSPORT_CERTIFICATE_HASH_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuthenticationChallengeToken([u8; AUTHENTICATION_CHALLENGE_BYTES]);

impl AuthenticationChallengeToken {
    pub const fn from_bytes(bytes: [u8; AUTHENTICATION_CHALLENGE_BYTES]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; AUTHENTICATION_CHALLENGE_BYTES] {
        &self.0
    }
}

impl fmt::Debug for AuthenticationChallengeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthenticationChallengeToken([redacted])")
    }
}

impl Serialize for AuthenticationChallengeToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for AuthenticationChallengeToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        let bytes = bytes.try_into().map_err(|bytes: Vec<u8>| {
            serde::de::Error::custom(format!(
                "authentication challenge must be {AUTHENTICATION_CHALLENGE_BYTES} bytes, got {}",
                bytes.len()
            ))
        })?;
        Ok(Self(bytes))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingRequest {
    pub ticket: PairingToken,
    pub device_identity: Ed25519PublicKey,
    pub device_name: String,
    pub signature: Ed25519Signature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationChallenge {
    pub token: AuthenticationChallengeToken,
    pub host_identity: Ed25519PublicKey,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationProof {
    pub token: AuthenticationChallengeToken,
    pub signature: Ed25519Signature,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeviceAuthenticationStart {
    Pair { request: PairingRequest },
    Authenticate { device_identity: Ed25519PublicKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeviceAuthenticationReply {
    Challenge { challenge: AuthenticationChallenge },
    Accepted { device_identity: Ed25519PublicKey },
}

pub fn pairing_proof_message(
    host_identity: Ed25519PublicKey,
    ticket: PairingToken,
    device_identity: Ed25519PublicKey,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(PAIRING_PROOF_CONTEXT.len() + 32 * 3);
    message.extend_from_slice(PAIRING_PROOF_CONTEXT);
    message.extend_from_slice(host_identity.as_bytes());
    message.extend_from_slice(ticket.as_bytes());
    message.extend_from_slice(device_identity.as_bytes());
    message
}

pub fn authentication_proof_message(
    host_identity: Ed25519PublicKey,
    token: AuthenticationChallengeToken,
    device_identity: Ed25519PublicKey,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(AUTHENTICATION_PROOF_CONTEXT.len() + 32 * 3);
    message.extend_from_slice(AUTHENTICATION_PROOF_CONTEXT);
    message.extend_from_slice(host_identity.as_bytes());
    message.extend_from_slice(token.as_bytes());
    message.extend_from_slice(device_identity.as_bytes());
    message
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

        assert!(error.to_string().contains("public key must be 32 bytes"));
    }

    #[test]
    fn transport_certificate_hash_is_fixed_length_base64url() {
        let hash = TransportCertificateHash::from_bytes([5; TRANSPORT_CERTIFICATE_HASH_BYTES]);
        let encoded = serde_json::to_string(&hash).unwrap();
        assert!(!encoded.contains('='));
        assert_eq!(
            serde_json::from_str::<TransportCertificateHash>(&encoded).unwrap(),
            hash
        );

        let error = serde_json::from_value::<TransportCertificateHash>(json!(
            URL_SAFE_NO_PAD.encode([5; TRANSPORT_CERTIFICATE_HASH_BYTES - 1])
        ))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("transport certificate hash must be 32 bytes")
        );
    }
}
