use crate::auth::{ConnectionNonce, Ed25519PublicKey, Ed25519Signature, TransportCertificateHash};
use serde::{Deserialize, Serialize};

pub const CURRENT_PROTOCOL_VERSION: u16 = 2;
pub const DEFAULT_MAX_FRAME_BYTES: u32 = 4 * 1024 * 1024;
const SERVER_HELLO_PROOF_CONTEXT: &[u8] = b"remote-agent server hello proof v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRange {
    pub min: u16,
    pub max: u16,
}

impl ProtocolRange {
    pub const CURRENT: Self = Self {
        min: CURRENT_PROTOCOL_VERSION,
        max: CURRENT_PROTOCOL_VERSION,
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionLimits {
    pub max_frame_bytes: u32,
    pub max_in_flight_requests: u32,
    pub outbound_queue_messages: u32,
    pub request_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientHello {
    pub versions: ProtocolRange,
    pub max_frame_bytes: u32,
    pub nonce: ConnectionNonce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerHello {
    pub version: u16,
    pub limits: ConnectionLimits,
    pub supported_methods: Vec<String>,
    pub host_identity: Ed25519PublicKey,
    pub transport_certificate_hash: TransportCertificateHash,
    pub host_signature: Ed25519Signature,
}

pub fn negotiate_version(
    client: ProtocolRange,
    server: ProtocolRange,
) -> Result<u16, VersionNegotiationError> {
    if client.min > client.max {
        return Err(VersionNegotiationError::InvalidRange {
            peer: "client",
            min: client.min,
            max: client.max,
        });
    }
    if server.min > server.max {
        return Err(VersionNegotiationError::InvalidRange {
            peer: "server",
            min: server.min,
            max: server.max,
        });
    }

    let min = client.min.max(server.min);
    let max = client.max.min(server.max);
    if min > max {
        return Err(VersionNegotiationError::NoCommonVersion { client, server });
    }

    Ok(max)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VersionNegotiationError {
    #[error("{peer} protocol range is invalid: {min}..={max}")]
    InvalidRange {
        peer: &'static str,
        min: u16,
        max: u16,
    },
    #[error("no common protocol version between client {client:?} and server {server:?}")]
    NoCommonVersion {
        client: ProtocolRange,
        server: ProtocolRange,
    },
}

pub fn server_hello_proof_message(
    nonce: ConnectionNonce,
    version: u16,
    max_frame_bytes: u32,
    host_identity: Ed25519PublicKey,
    transport_certificate_hash: TransportCertificateHash,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(SERVER_HELLO_PROOF_CONTEXT.len() + 32 + 2 + 4 + 32 + 32);
    message.extend_from_slice(SERVER_HELLO_PROOF_CONTEXT);
    message.extend_from_slice(nonce.as_bytes());
    message.extend_from_slice(&version.to_be_bytes());
    message.extend_from_slice(&max_frame_bytes.to_be_bytes());
    message.extend_from_slice(host_identity.as_bytes());
    message.extend_from_slice(transport_certificate_hash.as_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::json;

    #[test]
    fn negotiates_highest_common_version() {
        assert_eq!(
            negotiate_version(
                ProtocolRange { min: 1, max: 4 },
                ProtocolRange { min: 3, max: 5 },
            ),
            Ok(4)
        );
    }

    #[test]
    fn rejects_disjoint_and_invalid_version_ranges() {
        assert_eq!(
            negotiate_version(
                ProtocolRange { min: 1, max: 2 },
                ProtocolRange { min: 3, max: 4 },
            ),
            Err(VersionNegotiationError::NoCommonVersion {
                client: ProtocolRange { min: 1, max: 2 },
                server: ProtocolRange { min: 3, max: 4 },
            })
        );
        assert!(matches!(
            negotiate_version(ProtocolRange { min: 2, max: 1 }, ProtocolRange::CURRENT,),
            Err(VersionNegotiationError::InvalidRange { peer: "client", .. })
        ));
    }

    #[test]
    fn server_hello_serializes_the_bound_transport_certificate_hash() {
        let server_hello = ServerHello {
            version: CURRENT_PROTOCOL_VERSION,
            limits: ConnectionLimits {
                max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
                max_in_flight_requests: 4,
                outbound_queue_messages: 16,
                request_timeout_ms: 30_000,
            },
            supported_methods: vec![
                "thread/list".to_owned(),
                "item/commandExecution/requestApproval".to_owned(),
            ],
            host_identity: Ed25519PublicKey::from_bytes([2; 32]),
            transport_certificate_hash: TransportCertificateHash::from_bytes([3; 32]),
            host_signature: Ed25519Signature::from_bytes([4; 64]),
        };

        let value = serde_json::to_value(&server_hello).unwrap();
        assert_eq!(
            value["transportCertificateHash"],
            json!(URL_SAFE_NO_PAD.encode([3; 32]))
        );
        assert_eq!(
            serde_json::from_value::<ServerHello>(value).unwrap(),
            server_hello
        );
    }

    #[test]
    fn server_hello_proof_binds_the_transport_certificate_hash() {
        let nonce = ConnectionNonce::from_bytes([1; 32]);
        let identity = Ed25519PublicKey::from_bytes([2; 32]);
        let first = server_hello_proof_message(
            nonce,
            CURRENT_PROTOCOL_VERSION,
            DEFAULT_MAX_FRAME_BYTES,
            identity,
            TransportCertificateHash::from_bytes([3; 32]),
        );
        let second = server_hello_proof_message(
            nonce,
            CURRENT_PROTOCOL_VERSION,
            DEFAULT_MAX_FRAME_BYTES,
            identity,
            TransportCertificateHash::from_bytes([4; 32]),
        );

        assert_ne!(first, second);
        assert_eq!(&first[(first.len() - 32)..], &[3; 32]);
    }
}
