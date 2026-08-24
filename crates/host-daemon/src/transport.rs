use host_protocol::{
    ClientHello, ConnectionLimits, Ed25519PublicKey, FrameError, ProtocolRange, RpcMessage,
    ServerHello, TransportCertificateHash, VersionNegotiationError, negotiate_version, read_frame,
    server_hello_proof_message, write_frame,
};
use quinn::{Connection, RecvStream, SendStream};
use ring::signature::{ED25519, UnparsedPublicKey};

use crate::host_identity::HostIdentity;

const HANDSHAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct RpcServerConfig {
    pub versions: ProtocolRange,
    pub limits: ConnectionLimits,
    pub supported_methods: Vec<String>,
    /// Hash of the leaf transport certificate presented by this listener.
    /// It is signed with the pinned PC Host Identity in ServerHello.
    pub transport_certificate_hash: TransportCertificateHash,
}

pub struct RpcChannel {
    send: SendStream,
    receive: RecvStream,
    max_frame_bytes: u32,
}

pub struct PendingRpcChannel {
    send: SendStream,
    receive: RecvStream,
    max_frame_bytes: u32,
    pub(crate) host_identity: Ed25519PublicKey,
}

impl PendingRpcChannel {
    pub(crate) async fn send<T: serde::Serialize>(
        &mut self,
        message: &T,
    ) -> Result<(), TransportError> {
        write_frame(&mut self.send, message, self.max_frame_bytes).await?;
        Ok(())
    }

    pub(crate) async fn receive<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<T, TransportError> {
        Ok(read_frame(&mut self.receive, self.max_frame_bytes).await?)
    }

    pub(crate) fn authenticate(self) -> RpcChannel {
        RpcChannel {
            send: self.send,
            receive: self.receive,
            max_frame_bytes: self.max_frame_bytes,
        }
    }

    #[cfg(test)]
    fn assume_authenticated_for_transport_test(self) -> RpcChannel {
        self.authenticate()
    }
}

impl RpcChannel {
    pub fn max_frame_bytes(&self) -> u32 {
        self.max_frame_bytes
    }

    pub async fn send(&mut self, message: &RpcMessage) -> Result<(), TransportError> {
        write_frame(&mut self.send, message, self.max_frame_bytes).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> Result<RpcMessage, TransportError> {
        Ok(read_frame(&mut self.receive, self.max_frame_bytes).await?)
    }

    pub(crate) fn into_parts(self) -> (SendStream, RecvStream, u32) {
        (self.send, self.receive, self.max_frame_bytes)
    }
}

pub async fn accept_rpc_channel(
    connection: &Connection,
    config: &RpcServerConfig,
    host_identity: &HostIdentity,
) -> Result<(PendingRpcChannel, ClientHello), TransportError> {
    validate_limits(&config.limits)?;
    let (mut send, mut receive) = connection.accept_bi().await?;
    let hello: ClientHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES).await?;
    if hello.max_frame_bytes == 0 {
        return Err(TransportError::InvalidHandshake(
            "client maxFrameBytes must be positive".to_owned(),
        ));
    }

    let version = negotiate_version(hello.versions, config.versions)?;
    let max_frame_bytes = hello.max_frame_bytes.min(config.limits.max_frame_bytes);
    let host_public_key = host_identity.public_key();
    let host_signature = host_identity.sign(&server_hello_proof_message(
        hello.nonce,
        version,
        max_frame_bytes,
        host_public_key,
        config.transport_certificate_hash,
    ));
    let server_hello = ServerHello {
        version,
        limits: ConnectionLimits {
            max_frame_bytes,
            ..config.limits.clone()
        },
        supported_methods: config.supported_methods.clone(),
        host_identity: host_public_key,
        transport_certificate_hash: config.transport_certificate_hash,
        host_signature,
    };
    write_frame(&mut send, &server_hello, HANDSHAKE_MAX_FRAME_BYTES).await?;

    Ok((
        PendingRpcChannel {
            send,
            receive,
            max_frame_bytes,
            host_identity: host_public_key,
        },
        hello,
    ))
}

pub async fn connect_rpc_channel(
    connection: &Connection,
    hello: &ClientHello,
    expected_host_identity: Ed25519PublicKey,
    expected_transport_certificate_hash: TransportCertificateHash,
) -> Result<(PendingRpcChannel, ServerHello), TransportError> {
    if hello.max_frame_bytes == 0 {
        return Err(TransportError::InvalidHandshake(
            "client maxFrameBytes must be positive".to_owned(),
        ));
    }

    let (mut send, mut receive) = connection.open_bi().await?;
    write_frame(&mut send, hello, HANDSHAKE_MAX_FRAME_BYTES).await?;
    let server_hello: ServerHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES).await?;
    validate_limits(&server_hello.limits)?;
    if server_hello.version < hello.versions.min || server_hello.version > hello.versions.max {
        return Err(TransportError::InvalidHandshake(format!(
            "server selected unsupported protocol version {}",
            server_hello.version
        )));
    }
    if server_hello.limits.max_frame_bytes > hello.max_frame_bytes {
        return Err(TransportError::InvalidHandshake(format!(
            "server maxFrameBytes {} exceeds client offer {}",
            server_hello.limits.max_frame_bytes, hello.max_frame_bytes
        )));
    }
    if server_hello.host_identity != expected_host_identity {
        return Err(TransportError::HostIdentityMismatch);
    }
    if server_hello.transport_certificate_hash != expected_transport_certificate_hash {
        return Err(TransportError::TransportCertificateMismatch);
    }
    let proof = server_hello_proof_message(
        hello.nonce,
        server_hello.version,
        server_hello.limits.max_frame_bytes,
        server_hello.host_identity,
        server_hello.transport_certificate_hash,
    );
    UnparsedPublicKey::new(&ED25519, server_hello.host_identity.as_bytes())
        .verify(&proof, server_hello.host_signature.as_bytes())
        .map_err(|_| TransportError::InvalidHostSignature)?;

    let channel = PendingRpcChannel {
        send,
        receive,
        max_frame_bytes: server_hello.limits.max_frame_bytes,
        host_identity: server_hello.host_identity,
    };
    Ok((channel, server_hello))
}

pub(crate) fn validate_limits(limits: &ConnectionLimits) -> Result<(), TransportError> {
    if limits.max_frame_bytes == 0 {
        return Err(TransportError::InvalidHandshake(
            "maxFrameBytes must be positive".to_owned(),
        ));
    }
    if limits.max_in_flight_requests == 0 {
        return Err(TransportError::InvalidHandshake(
            "maxInFlightRequests must be positive".to_owned(),
        ));
    }
    if limits.outbound_queue_messages == 0 {
        return Err(TransportError::InvalidHandshake(
            "outboundQueueMessages must be positive".to_owned(),
        ));
    }
    if limits.request_timeout_ms == 0 {
        return Err(TransportError::InvalidHandshake(
            "requestTimeoutMs must be positive".to_owned(),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("QUIC connection failed: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("RPC framing failed: {0}")]
    Frame(#[from] FrameError),
    #[error("protocol version negotiation failed: {0}")]
    Version(#[from] VersionNegotiationError),
    #[error("invalid RPC handshake: {0}")]
    InvalidHandshake(String),
    #[error("PC Host identity does not match the pinned identity")]
    HostIdentityMismatch,
    #[error("PC Host handshake signature is invalid")]
    InvalidHostSignature,
    #[error("transport certificate hash does not match the expected certificate")]
    TransportCertificateMismatch,
    #[error("RPC stream received an unexpected notification from the client")]
    UnexpectedMessage,
    #[error("RPC outbound queue closed")]
    OutboundClosed,
    #[error("RPC outbound queue overflowed while forwarding live updates")]
    OutboundOverflow,
    #[error("live notification source closed; reconnect and request a fresh snapshot")]
    NotificationSourceClosed,
    #[error("RPC request task failed: {0}")]
    RequestTask(#[source] tokio::task::JoinError),
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

    use crate::{
        ConnectionAuthenticationError, DeviceAuthenticationState, HostSettings, PairedDevice,
        authenticate_incoming_channel, authenticate_outgoing_channel, load_settings,
        pair_outgoing_channel, rpc_server::serve_messages,
    };
    use host_protocol::{
        AuthenticationProof, CURRENT_PROTOCOL_VERSION, ConnectionNonce, DeviceAuthenticationReply,
        DeviceAuthenticationStart, PairingRequest, PairingToken, RpcId, RpcNotification,
        RpcOutcome, RpcRequest, RpcResponse, authentication_proof_message, pairing_proof_message,
    };
    use quinn::{ClientConfig, Endpoint, ServerConfig};
    use rcgen::generate_simple_self_signed;
    use rustls::{RootCertStore, pki_types::PrivatePkcs8KeyDer};
    use serde_json::json;

    use super::*;

    struct DirectoryCleanup(PathBuf);

    impl Drop for DirectoryCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn exchanges_request_response_and_notification_over_loopback_quic() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let (received_tx, received_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let config = RpcServerConfig {
                versions: ProtocolRange::CURRENT,
                limits: ConnectionLimits {
                    max_frame_bytes: 4096,
                    max_in_flight_requests: 8,
                    outbound_queue_messages: 16,
                    request_timeout_ms: 1_000,
                },
                supported_methods: vec!["thread/list".to_owned()],
                transport_certificate_hash: test_certificate_hash(),
            };
            let (channel, client_hello) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let mut channel = channel.assume_authenticated_for_transport_test();
            assert_eq!(client_hello.max_frame_bytes, 8192);

            let request = channel.receive().await.unwrap();
            assert_eq!(
                request,
                RpcMessage::Request(RpcRequest {
                    id: RpcId::Integer(1),
                    method: "thread/list".to_owned(),
                    params: json!({}),
                    extensions: Default::default(),
                })
            );
            channel
                .send(&RpcMessage::Response(RpcResponse {
                    id: RpcId::Integer(1),
                    outcome: RpcOutcome::Success {
                        result: json!({ "threads": [] }),
                    },
                    extensions: Default::default(),
                }))
                .await
                .unwrap();
            channel
                .send(&RpcMessage::Notification(RpcNotification {
                    method: "connection.ready".to_owned(),
                    params: json!({}),
                    extensions: Default::default(),
                }))
                .await
                .unwrap();
            received_rx.await.unwrap();
            connection.close(0_u32.into(), b"test complete");
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let hello = ClientHello {
            versions: ProtocolRange::CURRENT,
            max_frame_bytes: 8192,
            nonce: ConnectionNonce::from_bytes([1; 32]),
        };
        let (channel, server_hello) = connect_rpc_channel(
            &connection,
            &hello,
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let mut channel = channel.assume_authenticated_for_transport_test();
        assert_eq!(server_hello.version, CURRENT_PROTOCOL_VERSION);
        assert_eq!(server_hello.limits.max_frame_bytes, 4096);
        assert_eq!(server_hello.supported_methods, ["thread/list"]);
        assert_eq!(
            server_hello.transport_certificate_hash,
            test_certificate_hash()
        );

        channel
            .send(&RpcMessage::Request(RpcRequest {
                id: RpcId::Integer(1),
                method: "thread/list".to_owned(),
                params: json!({}),
                extensions: Default::default(),
            }))
            .await
            .unwrap();
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Response(RpcResponse {
                id: RpcId::Integer(1),
                outcome: RpcOutcome::Success {
                    result: json!({ "threads": [] }),
                },
                extensions: Default::default(),
            })
        );
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Notification(RpcNotification {
                method: "connection.ready".to_owned(),
                params: json!({}),
                extensions: Default::default(),
            })
        );

        received_tx.send(()).unwrap();
        server.await.unwrap();
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;
    }

    #[tokio::test]
    async fn bounds_concurrency_passes_unknown_methods_and_times_out_requests() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let server_started = started.clone();
        let server_release = release.clone();
        let server = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let config = RpcServerConfig {
                versions: ProtocolRange::CURRENT,
                limits: ConnectionLimits {
                    max_frame_bytes: 4096,
                    max_in_flight_requests: 1,
                    outbound_queue_messages: 2,
                    request_timeout_ms: 100,
                },
                supported_methods: vec!["test.run".to_owned()],
                transport_certificate_hash: test_certificate_hash(),
            };
            let (channel, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let channel = channel.assume_authenticated_for_transport_test();
            let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
            let result = serve_messages(
                channel,
                &config,
                move |request| {
                    let started = server_started.clone();
                    let release = server_release.clone();
                    async move {
                        match request.params["mode"].as_str() {
                            Some("block") => {
                                started.notify_one();
                                release.notified().await;
                            }
                            Some("timeout") => std::future::pending::<()>().await,
                            _ => {}
                        }
                        RpcOutcome::Success {
                            result: json!({ "handled": request.id }),
                        }
                    }
                },
                |_response| async {},
                outbound_rx,
            )
            .await;
            assert!(result.is_err());
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let hello = ClientHello {
            versions: ProtocolRange::CURRENT,
            max_frame_bytes: 4096,
            nonce: ConnectionNonce::from_bytes([2; 32]),
        };
        let (channel, _) = connect_rpc_channel(
            &connection,
            &hello,
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let mut channel = channel.assume_authenticated_for_transport_test();

        channel
            .send(&request(1, "test.run", json!({ "mode": "block" })))
            .await
            .unwrap();
        started.notified().await;
        channel
            .send(&request(2, "test.run", json!({ "mode": "block" })))
            .await
            .unwrap();
        assert_failure(channel.receive().await.unwrap(), 2, "too_many_requests");

        release.notify_one();
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Response(RpcResponse {
                id: RpcId::Integer(1),
                outcome: RpcOutcome::Success {
                    result: json!({ "handled": 1 }),
                },
                extensions: Default::default(),
            })
        );

        channel
            .send(&request(3, "test.run", json!({ "mode": "timeout" })))
            .await
            .unwrap();
        assert_failure(channel.receive().await.unwrap(), 3, "request_timeout");

        channel
            .send(&request(4, "not.supported", json!({})))
            .await
            .unwrap();
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Response(RpcResponse {
                id: RpcId::Integer(4),
                outcome: RpcOutcome::Success {
                    result: json!({ "handled": 4 }),
                },
                extensions: Default::default(),
            })
        );

        connection.close(0_u32.into(), b"test disconnect");
        server.await.unwrap();
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;
    }

    #[tokio::test]
    async fn forwards_server_requests_and_routes_client_responses_separately() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let response_seen = Arc::new(tokio::sync::Notify::new());
        let response_seen_server = response_seen.clone();
        let server = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let config = test_server_config();
            let (channel, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let channel = channel.assume_authenticated_for_transport_test();
            let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(4);
            outbound_tx
                .send(RpcMessage::Request(RpcRequest {
                    id: RpcId::String("approval-1".to_owned()),
                    method: "item/commandExecution/requestApproval".to_owned(),
                    params: json!({"command": "cargo test"}),
                    extensions: Default::default(),
                }))
                .await
                .unwrap();

            let result = serve_messages(
                channel,
                &config,
                |request| async move {
                    RpcOutcome::Success {
                        result: json!({"method": request.method}),
                    }
                },
                move |response| {
                    let response_seen = response_seen_server.clone();
                    async move {
                        assert_eq!(response.id, RpcId::String("approval-1".to_owned()));
                        response_seen.notify_one();
                    }
                },
                outbound_rx,
            )
            .await;
            assert!(result.is_err());
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (channel, _) = connect_rpc_channel(
            &connection,
            &client_hello(4),
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let mut channel = channel.assume_authenticated_for_transport_test();
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Request(RpcRequest {
                id: RpcId::String("approval-1".to_owned()),
                method: "item/commandExecution/requestApproval".to_owned(),
                params: json!({"command": "cargo test"}),
                extensions: Default::default(),
            })
        );
        channel
            .send(&RpcMessage::Response(RpcResponse {
                id: RpcId::String("approval-1".to_owned()),
                outcome: RpcOutcome::Success {
                    result: json!({"approved": true}),
                },
                extensions: Default::default(),
            }))
            .await
            .unwrap();
        channel
            .send(&request(9, "future/codexMethod", json!({"x": true})))
            .await
            .unwrap();
        assert_eq!(
            channel.receive().await.unwrap(),
            RpcMessage::Response(RpcResponse {
                id: RpcId::Integer(9),
                outcome: RpcOutcome::Success {
                    result: json!({"method": "future/codexMethod"}),
                },
                extensions: Default::default(),
            })
        );
        response_seen.notified().await;
        connection.close(0_u32.into(), b"test complete");
        server.await.unwrap();
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;
    }

    #[tokio::test]
    async fn disconnect_cancels_in_flight_request_tasks() {
        struct NotifyOnDrop(Arc<tokio::sync::Notify>);

        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }

        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let server_started = started.clone();
        let server_cancelled = cancelled.clone();
        let server = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let config = RpcServerConfig {
                versions: ProtocolRange::CURRENT,
                limits: ConnectionLimits {
                    max_frame_bytes: 4096,
                    max_in_flight_requests: 1,
                    outbound_queue_messages: 2,
                    request_timeout_ms: 10_000,
                },
                supported_methods: vec!["test.run".to_owned()],
                transport_certificate_hash: test_certificate_hash(),
            };
            let (channel, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let channel = channel.assume_authenticated_for_transport_test();
            let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(1);
            let result = serve_messages(
                channel,
                &config,
                move |_| {
                    let started = server_started.clone();
                    let cancelled = server_cancelled.clone();
                    async move {
                        let _notify_on_drop = NotifyOnDrop(cancelled);
                        started.notify_one();
                        std::future::pending::<RpcOutcome>().await
                    }
                },
                |_response| async {},
                outbound_rx,
            )
            .await;
            assert!(result.is_err());
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let hello = ClientHello {
            versions: ProtocolRange::CURRENT,
            max_frame_bytes: 4096,
            nonce: ConnectionNonce::from_bytes([3; 32]),
        };
        let (channel, _) = connect_rpc_channel(
            &connection,
            &hello,
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let mut channel = channel.assume_authenticated_for_transport_test();
        channel
            .send(&request(1, "test.run", json!({})))
            .await
            .unwrap();
        started.notified().await;

        connection.close(0_u32.into(), b"test disconnect");
        server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
            .await
            .expect("request future was not cancelled");
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;
    }

    #[tokio::test]
    async fn failed_pairing_persistence_does_not_leave_device_paired() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let device_identity = HostIdentity::generate_unstored().unwrap();
        let device_public_key = device_identity.public_key();
        let ticket = PairingToken::from_bytes([32; 32]);

        let settings_directory = std::env::temp_dir().join(format!(
            "remote-agent-pairing-failure-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&settings_directory).unwrap();
        let _cleanup = DirectoryCleanup(settings_directory.clone());
        let blocked_parent = settings_directory.join("blocked-parent");
        std::fs::write(&blocked_parent, b"not a directory").unwrap();
        let settings_path = blocked_parent.join("settings.json");

        let server_settings_path = settings_path.clone();
        let mut server = tokio::spawn(async move {
            let config = test_server_config();
            let authentication = Arc::new(tokio::sync::Mutex::new(DeviceAuthenticationState::new(
                HostSettings::default(),
                server_settings_path,
                100,
            )));
            authentication
                .lock()
                .await
                .issue_pairing_ticket_with_token(host_public_key, vec![], 200, ticket)
                .unwrap();

            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let (pending, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let result = authenticate_incoming_channel(
                pending,
                &host_identity,
                authentication.clone(),
                || 100,
            )
            .await;

            assert!(matches!(
                result,
                Err(ConnectionAuthenticationError::Settings(_))
            ));
            assert!(
                authentication
                    .lock()
                    .await
                    .settings()
                    .paired_devices
                    .is_empty()
            );
            connection.close(0_u32.into(), b"settings persistence failed");
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (pending, _) = connect_rpc_channel(
            &connection,
            &client_hello(51),
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let request = PairingRequest {
            ticket,
            device_identity: device_public_key,
            device_name: "test phone".to_owned(),
            signature: device_identity.sign(&pairing_proof_message(
                host_public_key,
                ticket,
                device_public_key,
            )),
        };

        let client_result = tokio::time::timeout(
            Duration::from_secs(1),
            pair_outgoing_channel(pending, request),
        )
        .await
        .expect("pairing failure must close the authentication attempt");
        let client_failed = client_result.is_err();

        connection.close(0_u32.into(), b"test complete");
        let server_completed = match tokio::time::timeout(Duration::from_secs(1), &mut server).await
        {
            Ok(Ok(())) => true,
            Ok(Err(_)) | Err(_) => {
                server.abort();
                let _ = server.await;
                false
            }
        };
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;

        assert!(client_failed);
        assert!(server_completed);
    }

    #[tokio::test]
    async fn authenticates_another_device_while_first_proof_is_pending() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = Arc::new(HostIdentity::generate_unstored().unwrap());
        let host_public_key = host_identity.public_key();
        let device_a_identity = HostIdentity::generate_unstored().unwrap();
        let device_a_public = device_a_identity.public_key();
        let device_b_identity = HostIdentity::generate_unstored().unwrap();
        let device_b_public = device_b_identity.public_key();
        let settings_directory = std::env::temp_dir().join(format!(
            "remote-agent-concurrent-auth-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _cleanup = DirectoryCleanup(settings_directory.clone());
        let settings_path = settings_directory.join("settings.json");
        let settings = HostSettings {
            paired_devices: vec![
                PairedDevice {
                    identity: device_a_public,
                    name: "device a".to_owned(),
                },
                PairedDevice {
                    identity: device_b_public,
                    name: "device b".to_owned(),
                },
            ],
            ..HostSettings::default()
        };
        let authentication = Arc::new(tokio::sync::Mutex::new(DeviceAuthenticationState::new(
            settings,
            settings_path,
            60_000,
        )));
        let server_authentication = authentication.clone();
        let server_host_identity = host_identity.clone();
        let mut server = tokio::spawn(async move {
            let config = test_server_config();

            let incoming = server_endpoint.accept().await.unwrap();
            let connection_a = incoming.await.unwrap();
            let (pending, _) = accept_rpc_channel(&connection_a, &config, &server_host_identity)
                .await
                .unwrap();
            let first_authentication = server_authentication.clone();
            let first_host_identity = server_host_identity.clone();
            let first = tokio::spawn(async move {
                authenticate_incoming_channel(
                    pending,
                    &first_host_identity,
                    first_authentication,
                    || 100,
                )
                .await
                .map(|(_, identity)| identity)
            });

            let incoming = server_endpoint.accept().await.unwrap();
            let connection_b = incoming.await.unwrap();
            let (pending, _) = accept_rpc_channel(&connection_b, &config, &server_host_identity)
                .await
                .unwrap();
            let second_authentication = server_authentication.clone();
            let second_host_identity = server_host_identity.clone();
            let second = tokio::spawn(async move {
                authenticate_incoming_channel(
                    pending,
                    &second_host_identity,
                    second_authentication,
                    || 100,
                )
                .await
                .map(|(_, identity)| identity)
            });

            let results = (first.await.unwrap(), second.await.unwrap());
            drop((connection_a, connection_b));
            results
        });

        let connection_a = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut pending_a, _) = connect_rpc_channel(
            &connection_a,
            &client_hello(61),
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        pending_a
            .send(&DeviceAuthenticationStart::Authenticate {
                device_identity: device_a_public,
            })
            .await
            .unwrap();
        let challenge_a = match tokio::time::timeout(
            Duration::from_secs(1),
            pending_a.receive::<DeviceAuthenticationReply>(),
        )
        .await
        .expect("first device did not receive an authentication challenge")
        .unwrap()
        {
            DeviceAuthenticationReply::Challenge { challenge } => challenge,
            _ => panic!("first device did not receive a challenge"),
        };

        let connection_b = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let (pending_b, _) = connect_rpc_channel(
            &connection_b,
            &client_hello(62),
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let second_client_succeeded = matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                authenticate_outgoing_channel(pending_b, device_b_public, |message| {
                    device_b_identity.sign(message)
                }),
            )
            .await,
            Ok(Ok(_))
        );
        connection_b.close(0_u32.into(), b"second authentication complete");

        let proof_message =
            authentication_proof_message(host_public_key, challenge_a.token, device_a_public);
        pending_a
            .send(&AuthenticationProof {
                token: challenge_a.token,
                signature: device_a_identity.sign(&proof_message),
            })
            .await
            .unwrap();
        let first_client_succeeded = matches!(
            tokio::time::timeout(
                Duration::from_secs(1),
                pending_a.receive::<DeviceAuthenticationReply>(),
            )
            .await
            .expect("first device authentication did not finish")
            .unwrap(),
            DeviceAuthenticationReply::Accepted { device_identity }
                if device_identity == device_a_public
        );
        connection_a.close(0_u32.into(), b"first authentication complete");

        let server_succeeded = match tokio::time::timeout(Duration::from_secs(1), &mut server).await
        {
            Ok(Ok((first, second))) => {
                matches!(first, Ok(identity) if identity == device_a_public)
                    && matches!(second, Ok(identity) if identity == device_b_public)
            }
            Ok(Err(_)) | Err(_) => {
                server.abort();
                let _ = server.await;
                false
            }
        };

        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;

        assert!(first_client_succeeded);
        assert!(second_client_succeeded);
        assert!(server_succeeded);
    }

    #[tokio::test]
    async fn pairs_then_reauthenticates_before_exposing_rpc_channel() {
        let (server_endpoint, client_endpoint, server_addr) = endpoints();
        let host_identity = HostIdentity::generate_unstored().unwrap();
        let host_public_key = host_identity.public_key();
        let device_identity = HostIdentity::generate_unstored().unwrap();
        let device_public_key = device_identity.public_key();
        let ticket = PairingToken::from_bytes([31; 32]);
        let settings_directory = std::env::temp_dir().join(format!(
            "remote-agent-connection-auth-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let settings_path = settings_directory.join("settings.json");
        let server_settings_path = settings_path.clone();

        let server = tokio::spawn(async move {
            let config = test_server_config();
            let authentication = Arc::new(tokio::sync::Mutex::new(DeviceAuthenticationState::new(
                HostSettings::default(),
                server_settings_path,
                100,
            )));
            authentication
                .lock()
                .await
                .issue_pairing_ticket_with_token(host_public_key, vec![], 200, ticket)
                .unwrap();

            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let (pending, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let (_channel, paired_identity) = authenticate_incoming_channel(
                pending,
                &host_identity,
                authentication.clone(),
                || 100,
            )
            .await
            .unwrap();
            assert_eq!(paired_identity, device_public_key);
            connection.closed().await;

            let incoming = server_endpoint.accept().await.unwrap();
            let connection = incoming.await.unwrap();
            let (pending, _) = accept_rpc_channel(&connection, &config, &host_identity)
                .await
                .unwrap();
            let (_channel, authenticated_identity) =
                authenticate_incoming_channel(pending, &host_identity, authentication, || 100)
                    .await
                    .unwrap();
            assert_eq!(authenticated_identity, device_public_key);
            connection.closed().await;
        });

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let hello = client_hello(41);
        let (pending, _) = connect_rpc_channel(
            &connection,
            &hello,
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        let pairing_message = pairing_proof_message(host_public_key, ticket, device_public_key);
        let request = PairingRequest {
            ticket,
            device_identity: device_public_key,
            device_name: "test phone".to_owned(),
            signature: device_identity.sign(&pairing_message),
        };
        pair_outgoing_channel(pending, request).await.unwrap();
        connection.close(0_u32.into(), b"paired");

        let connection = client_endpoint
            .connect(server_addr, "localhost")
            .unwrap()
            .await
            .unwrap();
        let hello = client_hello(42);
        let (pending, _) = connect_rpc_channel(
            &connection,
            &hello,
            host_public_key,
            test_certificate_hash(),
        )
        .await
        .unwrap();
        authenticate_outgoing_channel(pending, device_public_key, |message| {
            device_identity.sign(message)
        })
        .await
        .unwrap();
        connection.close(0_u32.into(), b"authenticated");

        server.await.unwrap();
        let persisted = load_settings(&settings_path).unwrap();
        assert_eq!(persisted.paired_devices.len(), 1);
        assert_eq!(persisted.paired_devices[0].identity, device_public_key);
        std::fs::remove_dir_all(settings_directory).unwrap();
        client_endpoint.close(0_u32.into(), b"test complete");
        client_endpoint.wait_idle().await;
    }

    #[test]
    fn validates_nonzero_connection_limits() {
        let error = validate_limits(&ConnectionLimits {
            max_frame_bytes: 1024,
            max_in_flight_requests: 0,
            outbound_queue_messages: 1,
            request_timeout_ms: 1000,
        })
        .unwrap_err();
        assert!(matches!(error, TransportError::InvalidHandshake(_)));
    }

    fn client_hello(nonce_byte: u8) -> ClientHello {
        ClientHello {
            versions: ProtocolRange::CURRENT,
            max_frame_bytes: 4096,
            nonce: ConnectionNonce::from_bytes([nonce_byte; 32]),
        }
    }

    fn test_server_config() -> RpcServerConfig {
        RpcServerConfig {
            versions: ProtocolRange::CURRENT,
            limits: ConnectionLimits {
                max_frame_bytes: 4096,
                max_in_flight_requests: 8,
                outbound_queue_messages: 16,
                request_timeout_ms: 1_000,
            },
            supported_methods: vec![],
            transport_certificate_hash: test_certificate_hash(),
        }
    }

    fn test_certificate_hash() -> TransportCertificateHash {
        TransportCertificateHash::from_bytes([7; 32])
    }

    fn endpoints() -> (Endpoint, Endpoint, SocketAddr) {
        let certificate = generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
        let certificate = certificate.cert.der().clone();
        let server_config =
            ServerConfig::with_single_cert(vec![certificate.clone()], key.into()).unwrap();
        let server_endpoint =
            Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(certificate).unwrap();
        let client_config = ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
        let mut client_endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(client_config);

        (server_endpoint, client_endpoint, server_addr)
    }

    fn request(id: u64, method: &str, params: serde_json::Value) -> RpcMessage {
        RpcMessage::Request(RpcRequest {
            id: RpcId::Integer(id),
            method: method.to_owned(),
            params,
            extensions: Default::default(),
        })
    }

    fn assert_failure(message: RpcMessage, id: u64, code: &str) {
        assert!(matches!(
            message,
            RpcMessage::Response(RpcResponse {
                id: response_id,
                outcome: RpcOutcome::Failure { error },
                ..
            }) if response_id == RpcId::Integer(id) && error.code == json!(code)
        ));
    }
}
