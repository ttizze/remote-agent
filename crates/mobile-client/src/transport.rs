use std::{
    net::IpAddr,
    sync::{Arc, Mutex as StdMutex},
};

use host_protocol::{
    AuthenticationProof, ClientHello, ConnectionLimits, ConnectionNonce, DeviceAuthenticationReply,
    DeviceAuthenticationStart, Ed25519PublicKey, Ed25519Signature, PairingRequest, ProtocolRange,
    ServerHello, TransportCertificateHash, authentication_proof_message, pairing_proof_message,
    read_frame, server_hello_proof_message, write_frame,
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey},
};
use rustls::{
    ClientConfig as TlsClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};

use crate::{ConnectedHost, MobileClientConfig, MobileClientError};

const HANDSHAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;
const MAX_IN_FLIGHT_REQUESTS: u32 = 1_024;
const MAX_OUTBOUND_QUEUE_MESSAGES: u32 = 4_096;

pub(crate) struct AuthenticatedChannel {
    pub(crate) endpoint: Endpoint,
    pub(crate) connection: Connection,
    pub(crate) send: SendStream,
    pub(crate) receive: RecvStream,
    pub(crate) host: ConnectedHost,
}

/// Establishes an encrypted QUIC channel, verifies the pinned Host proof, and
/// authenticates the supplied device before returning streams for RPC.
pub(crate) async fn establish(
    config: &MobileClientConfig,
    device_key: &Ed25519KeyPair,
    device_identity: Ed25519PublicKey,
) -> Result<AuthenticatedChannel, MobileClientError> {
    let bind = match config.address.ip() {
        IpAddr::V4(_) => "0.0.0.0:0".parse().expect("valid IPv4 socket address"),
        IpAddr::V6(_) => "[::]:0".parse().expect("valid IPv6 socket address"),
    };
    let mut endpoint = Endpoint::client(bind).map_err(MobileClientError::Endpoint)?;
    let (tls, observed_certificate) = tls_client_config()?;
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(tls)));
    let connection = endpoint
        .connect(config.address, &config.server_name)
        .map_err(MobileClientError::Connect)?
        .await
        .map_err(MobileClientError::Connection)?;

    let nonce = random_nonce()?;
    let hello = ClientHello {
        versions: ProtocolRange::CURRENT,
        max_frame_bytes: config.max_frame_bytes,
        nonce,
    };
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(MobileClientError::Connection)?;
    write_frame(&mut send, &hello, HANDSHAKE_MAX_FRAME_BYTES).await?;
    let server_hello: ServerHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES).await?;
    verify_server_hello(
        &hello,
        &server_hello,
        config.host_identity,
        observed_certificate.get()?,
    )?;

    authenticate(
        &mut send,
        &mut receive,
        server_hello.limits.max_frame_bytes,
        config.host_identity,
        device_key,
        device_identity,
        config,
    )
    .await?;

    let host = ConnectedHost {
        version: server_hello.version,
        limits: server_hello.limits,
        supported_methods: server_hello.supported_methods,
    };
    Ok(AuthenticatedChannel {
        endpoint,
        connection,
        send,
        receive,
        host,
    })
}

async fn authenticate(
    send: &mut SendStream,
    receive: &mut RecvStream,
    max_frame_bytes: u32,
    host_identity: Ed25519PublicKey,
    device_key: &Ed25519KeyPair,
    device_identity: Ed25519PublicKey,
    config: &MobileClientConfig,
) -> Result<(), MobileClientError> {
    if let Some(ticket) = config.pairing_ticket {
        let signature = sign(
            device_key,
            &pairing_proof_message(host_identity, ticket, device_identity),
        );
        let start = DeviceAuthenticationStart::Pair {
            request: PairingRequest {
                ticket,
                device_identity,
                device_name: config.device_name.clone(),
                signature,
            },
        };
        write_frame(send, &start, max_frame_bytes).await?;
        match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Accepted {
                device_identity: accepted,
            } if accepted == device_identity => Ok(()),
            _ => Err(MobileClientError::AuthenticationRejected),
        }
    } else {
        write_frame(
            send,
            &DeviceAuthenticationStart::Authenticate { device_identity },
            max_frame_bytes,
        )
        .await?;
        let challenge = match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Challenge { challenge }
                if challenge.host_identity == host_identity =>
            {
                challenge
            }
            _ => return Err(MobileClientError::AuthenticationRejected),
        };
        let proof = AuthenticationProof {
            token: challenge.token,
            signature: sign(
                device_key,
                &authentication_proof_message(host_identity, challenge.token, device_identity),
            ),
        };
        write_frame(send, &proof, max_frame_bytes).await?;
        match read_frame(receive, max_frame_bytes).await? {
            DeviceAuthenticationReply::Accepted {
                device_identity: accepted,
            } if accepted == device_identity => Ok(()),
            _ => Err(MobileClientError::AuthenticationRejected),
        }
    }
}

fn verify_server_hello(
    hello: &ClientHello,
    server: &ServerHello,
    expected_identity: Ed25519PublicKey,
    observed_certificate_hash: TransportCertificateHash,
) -> Result<(), MobileClientError> {
    validate_limits(&server.limits)?;
    if server.version < hello.versions.min || server.version > hello.versions.max {
        return Err(MobileClientError::InvalidHandshake(
            "Host selected an unsupported protocol version",
        ));
    }
    if server.limits.max_frame_bytes > hello.max_frame_bytes {
        return Err(MobileClientError::InvalidHandshake(
            "Host exceeded the offered maximum frame size",
        ));
    }
    if server.host_identity != expected_identity {
        return Err(MobileClientError::HostIdentityMismatch);
    }
    if server.transport_certificate_hash != observed_certificate_hash {
        return Err(MobileClientError::TransportCertificateMismatch);
    }
    let message = server_hello_proof_message(
        hello.nonce,
        server.version,
        server.limits.max_frame_bytes,
        server.host_identity,
        server.transport_certificate_hash,
    );
    UnparsedPublicKey::new(&ED25519, server.host_identity.as_bytes())
        .verify(&message, server.host_signature.as_bytes())
        .map_err(|_| MobileClientError::InvalidHostProof)
}

fn validate_limits(limits: &ConnectionLimits) -> Result<(), MobileClientError> {
    if limits.max_frame_bytes == 0
        || limits.max_in_flight_requests == 0
        || limits.outbound_queue_messages == 0
        || limits.request_timeout_ms == 0
    {
        return Err(MobileClientError::InvalidHandshake(
            "Host advertised a zero limit",
        ));
    }
    if limits.max_in_flight_requests > MAX_IN_FLIGHT_REQUESTS {
        return Err(MobileClientError::InvalidHandshake(
            "Host advertised too many in-flight requests",
        ));
    }
    if limits.outbound_queue_messages > MAX_OUTBOUND_QUEUE_MESSAGES {
        return Err(MobileClientError::InvalidHandshake(
            "Host advertised an outbound queue larger than the mobile limit",
        ));
    }
    Ok(())
}

fn tls_client_config()
-> Result<(quinn::crypto::rustls::QuicClientConfig, ObservedCertificate), MobileClientError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let observed_certificate = ObservedCertificate::default();
    let verifier = ProofOnlyServerCertVerifier {
        algorithms: provider.signature_verification_algorithms,
        observed_certificate: observed_certificate.clone(),
    };
    let tls = TlsClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| MobileClientError::Tls(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|error| MobileClientError::Tls(error.to_string()))?;
    Ok((quic, observed_certificate))
}

/// A rotating transport certificate is accepted only to form encrypted QUIC.
/// Its TLS handshake signature is still verified. Before a device proof or any
/// RPC is sent, the ServerHello signature is verified with the pinned Ed25519
/// Host identity from pairing; that application proof authenticates the Host.
#[derive(Debug)]
struct ProofOnlyServerCertVerifier {
    algorithms: WebPkiSupportedAlgorithms,
    observed_certificate: ObservedCertificate,
}

impl ServerCertVerifier for ProofOnlyServerCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        self.observed_certificate.record(end_entity);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[derive(Debug, Clone, Default)]
struct ObservedCertificate(Arc<StdMutex<Option<TransportCertificateHash>>>);

impl ObservedCertificate {
    fn record(&self, certificate: &CertificateDer<'_>) {
        let digest = ring::digest::digest(&ring::digest::SHA256, certificate.as_ref());
        let hash = TransportCertificateHash::from_bytes(
            digest
                .as_ref()
                .try_into()
                .expect("SHA-256 digests have 32 bytes"),
        );
        *self.0.lock().expect("certificate observer mutex poisoned") = Some(hash);
    }

    fn get(&self) -> Result<TransportCertificateHash, MobileClientError> {
        self.0
            .lock()
            .map_err(|_| MobileClientError::Tls("certificate observer mutex poisoned".to_owned()))?
            .ok_or(MobileClientError::MissingTransportCertificate)
    }
}

fn random_nonce() -> Result<ConnectionNonce, MobileClientError> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| MobileClientError::Random)?;
    Ok(ConnectionNonce::from_bytes(bytes))
}

fn sign(key: &Ed25519KeyPair, message: &[u8]) -> Ed25519Signature {
    Ed25519Signature::from_bytes(
        key.sign(message)
            .as_ref()
            .try_into()
            .expect("ring Ed25519 signatures have 64 bytes"),
    )
}

#[cfg(test)]
mod tests {
    use host_protocol::{ConnectionLimits, DEFAULT_MAX_FRAME_BYTES};

    use super::*;

    #[test]
    fn handshake_and_data_frame_limits_are_distinct() {
        assert_eq!(HANDSHAKE_MAX_FRAME_BYTES, 64 * 1024);
        assert_eq!(DEFAULT_MAX_FRAME_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn validate_limits_rejects_zero_advertised_limit() {
        let limits = ConnectionLimits {
            max_frame_bytes: 0,
            max_in_flight_requests: 1,
            outbound_queue_messages: 1,
            request_timeout_ms: 1,
        };
        assert!(matches!(
            validate_limits(&limits),
            Err(MobileClientError::InvalidHandshake(
                "Host advertised a zero limit"
            ))
        ));
    }

    #[test]
    fn validate_limits_accepts_mobile_resource_budget_boundaries() {
        let in_flight = ConnectionLimits {
            max_frame_bytes: 1,
            max_in_flight_requests: MAX_IN_FLIGHT_REQUESTS,
            outbound_queue_messages: 1,
            request_timeout_ms: 1,
        };
        assert!(validate_limits(&in_flight).is_ok());

        let queue = ConnectionLimits {
            max_frame_bytes: 1,
            max_in_flight_requests: 1,
            outbound_queue_messages: MAX_OUTBOUND_QUEUE_MESSAGES,
            request_timeout_ms: 1,
        };
        assert!(validate_limits(&queue).is_ok());
    }

    #[test]
    fn validate_limits_rejects_in_flight_budget_overflow() {
        let limits = ConnectionLimits {
            max_frame_bytes: 1,
            max_in_flight_requests: MAX_IN_FLIGHT_REQUESTS + 1,
            outbound_queue_messages: 1,
            request_timeout_ms: 1,
        };
        assert!(matches!(
            validate_limits(&limits),
            Err(MobileClientError::InvalidHandshake(
                "Host advertised too many in-flight requests"
            ))
        ));
    }

    #[test]
    fn validate_limits_rejects_queue_budget_overflow() {
        let limits = ConnectionLimits {
            max_frame_bytes: 1,
            max_in_flight_requests: 1,
            outbound_queue_messages: MAX_OUTBOUND_QUEUE_MESSAGES + 1,
            request_timeout_ms: 1,
        };
        assert!(matches!(
            validate_limits(&limits),
            Err(MobileClientError::InvalidHandshake(
                "Host advertised an outbound queue larger than the mobile limit"
            ))
        ));
    }
}
