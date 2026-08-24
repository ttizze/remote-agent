use std::{net::SocketAddr, time::Duration};

use host_protocol::{
    ClientHello, ConnectionLimits, DEFAULT_MAX_FRAME_BYTES, DeviceAuthenticationReply,
    DeviceAuthenticationStart, Ed25519PublicKey, Ed25519Signature, PairingToken, RpcId, RpcMessage,
    RpcNotification, RpcOutcome, RpcRequest, RpcResponse, ServerHello, TransportCertificateHash,
    pairing_proof_message, read_frame, server_hello_proof_message, write_frame,
};
use quinn::{Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use ring::{
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use serde_json::json;
use tokio::time::timeout;

use super::*;

const HANDSHAKE_MAX_FRAME_BYTES: u32 = 64 * 1024;

fn valid_config(max_frame_bytes: u32) -> MobileClientConfig {
    MobileClientConfig {
        address: "127.0.0.1:0".parse().unwrap(),
        server_name: "host.local".to_owned(),
        host_identity: Ed25519PublicKey::from_bytes([1; 32]),
        device_name: "test phone".to_owned(),
        pairing_ticket: None,
        max_frame_bytes,
        request_timeout: Duration::from_secs(1),
    }
}

#[test]
fn accepts_host_data_frame_limit_even_though_it_exceeds_handshake_limit() {
    assert!(valid_config(DEFAULT_MAX_FRAME_BYTES).validate().is_ok());
}

#[test]
fn rejects_data_frame_limit_above_host_protocol_maximum() {
    assert!(matches!(
        valid_config(DEFAULT_MAX_FRAME_BYTES + 1).validate(),
        Err(MobileClientError::InvalidConfig(
            "max_frame_bytes exceeds data-frame maximum"
        ))
    ));
}

#[tokio::test]
async fn pairs_then_correlates_concurrent_requests_and_receives_notifications() {
    let (endpoint, address, certificate_hash) = server_endpoint();
    let (host_key, _) = key_pair();
    let host_identity = public_key(&host_key);
    let ticket = PairingToken::from_bytes([7; 32]);
    let server = tokio::spawn(async move {
        let incoming = endpoint.accept().await.unwrap();
        let connection = incoming.await.unwrap();
        let (mut send, mut receive) = connection.accept_bi().await.unwrap();
        let hello: ClientHello = read_frame(&mut receive, HANDSHAKE_MAX_FRAME_BYTES)
            .await
            .unwrap();
        let limits = ConnectionLimits {
            max_frame_bytes: 4096,
            max_in_flight_requests: 4,
            outbound_queue_messages: 8,
            request_timeout_ms: 1_000,
        };
        let server_hello = ServerHello {
            version: 2,
            limits: limits.clone(),
            supported_methods: vec!["thread/list".to_owned(), "thread/read".to_owned()],
            host_identity,
            transport_certificate_hash: certificate_hash,
            host_signature: sign(
                &host_key,
                &server_hello_proof_message(
                    hello.nonce,
                    2,
                    limits.max_frame_bytes,
                    host_identity,
                    certificate_hash,
                ),
            ),
        };
        write_frame(&mut send, &server_hello, HANDSHAKE_MAX_FRAME_BYTES)
            .await
            .unwrap();
        let pairing: DeviceAuthenticationStart = read_frame(&mut receive, limits.max_frame_bytes)
            .await
            .unwrap();
        let DeviceAuthenticationStart::Pair { request } = pairing else {
            panic!("expected pairing")
        };
        assert_eq!(request.ticket, ticket);
        UnparsedPublicKey::new(&ED25519, request.device_identity.as_bytes())
            .verify(
                &pairing_proof_message(host_identity, ticket, request.device_identity),
                request.signature.as_bytes(),
            )
            .unwrap();
        write_frame(
            &mut send,
            &DeviceAuthenticationReply::Accepted {
                device_identity: request.device_identity,
            },
            limits.max_frame_bytes,
        )
        .await
        .unwrap();

        let first: RpcMessage = read_frame(&mut receive, limits.max_frame_bytes)
            .await
            .unwrap();
        let second: RpcMessage = read_frame(&mut receive, limits.max_frame_bytes)
            .await
            .unwrap();
        let RpcMessage::Request(first) = first else {
            panic!("expected first request")
        };
        let RpcMessage::Request(second) = second else {
            panic!("expected second request")
        };
        write_frame(&mut send, &response(&second), limits.max_frame_bytes)
            .await
            .unwrap();
        write_frame(
            &mut send,
            &RpcMessage::Notification(RpcNotification {
                method: "turn/started".to_owned(),
                params: json!({
                    "type":"turnStarted",
                    "threadId":"t1",
                    "turnId":"turn-1",
                    "status":"inProgress"
                }),
                extensions: Default::default(),
            }),
            limits.max_frame_bytes,
        )
        .await
        .unwrap();
        write_frame(
            &mut send,
            &RpcMessage::Notification(RpcNotification {
                method: "codex/futureEvent".to_owned(),
                params: json!({"futureField": {"preserve": true}}),
                extensions: Default::default(),
            }),
            limits.max_frame_bytes,
        )
        .await
        .unwrap();
        write_frame(
            &mut send,
            &RpcMessage::Request(RpcRequest {
                id: RpcId::String("approval-7".to_owned()),
                method: "item/commandExecution/requestApproval".to_owned(),
                params: json!({"command": "cargo test"}),
                extensions: Default::default(),
            }),
            limits.max_frame_bytes,
        )
        .await
        .unwrap();
        write_frame(&mut send, &response(&first), limits.max_frame_bytes)
            .await
            .unwrap();
        let response: RpcMessage = timeout(
            Duration::from_secs(1),
            read_frame(&mut receive, limits.max_frame_bytes),
        )
        .await
        .expect("Host response timed out")
        .unwrap();
        let RpcMessage::Response(response) = response else {
            panic!("expected response to Host request")
        };
        assert_eq!(response.id, RpcId::String("approval-7".to_owned()));
        let RpcOutcome::Success { result } = response.outcome else {
            panic!("expected successful Host request response")
        };
        assert_eq!(result, json!({"approved": true}));
        write_frame(
            &mut send,
            &RpcMessage::Notification(RpcNotification {
                method: "test/responseReceived".to_owned(),
                params: json!({}),
                extensions: Default::default(),
            }),
            limits.max_frame_bytes,
        )
        .await
        .unwrap();
        connection.closed().await;
    });

    let (_device, pkcs8) = key_pair();
    let client = MobileClient::connect(
        MobileClientConfig {
            address,
            server_name: "ignored-by-proof-verifier".to_owned(),
            host_identity,
            device_name: "test phone".to_owned(),
            pairing_ticket: Some(ticket),
            max_frame_bytes: 4096,
            request_timeout: Duration::from_secs(1),
        },
        &pkcs8,
    )
    .await
    .unwrap();
    let mut notifications = client.subscribe();
    let mut server_requests = client.subscribe_server_requests();
    let (list, read) = tokio::join!(
        client.request("codex/customMethod", json!({"arbitrary": true})),
        client.request("thread/experimental", json!({"id":"t1"}))
    );
    assert_eq!(list.unwrap()["method"], "codex/customMethod");
    assert_eq!(read.unwrap()["method"], "thread/experimental");
    assert_eq!(
        notifications.recv().await.unwrap(),
        Notification {
            method: "turn/started".to_owned(),
            params: json!({
                "type":"turnStarted",
                "threadId":"t1",
                "turnId":"turn-1",
                "status":"inProgress"
            }),
            extensions: Default::default(),
        }
    );
    assert_eq!(
        notifications.recv().await.unwrap(),
        Notification {
            method: "codex/futureEvent".to_owned(),
            params: json!({"futureField": {"preserve": true}}),
            extensions: Default::default(),
        }
    );
    let server_request = server_requests.recv().await.unwrap();
    assert_eq!(server_request.id, RpcId::String("approval-7".to_owned()));
    assert_eq!(
        server_request.method,
        "item/commandExecution/requestApproval"
    );
    assert_eq!(server_request.params, json!({"command": "cargo test"}));
    client
        .respond_result(server_request.id, json!({"approved": true}))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(1), notifications.recv())
            .await
            .expect("response acknowledgement timed out")
            .unwrap()
            .method,
        "test/responseReceived"
    );
    client.close();
    server.await.unwrap();
}

fn response(request: &RpcRequest) -> RpcMessage {
    RpcMessage::Response(RpcResponse {
        id: request.id.clone(),
        outcome: RpcOutcome::Success {
            result: json!({"method": request.method}),
        },
        extensions: Default::default(),
    })
}

fn server_endpoint() -> (Endpoint, SocketAddr, TransportCertificateHash) {
    let certificate = generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let key = PrivatePkcs8KeyDer::from(certificate.key_pair.serialize_der());
    let certificate_der = certificate.cert.der().clone();
    let certificate_hash = TransportCertificateHash::from_bytes(
        ring::digest::digest(&ring::digest::SHA256, certificate_der.as_ref())
            .as_ref()
            .try_into()
            .unwrap(),
    );
    let config = ServerConfig::with_single_cert(vec![certificate_der], key.into()).unwrap();
    let endpoint = Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = endpoint.local_addr().unwrap();
    (endpoint, address, certificate_hash)
}

fn key_pair() -> (Ed25519KeyPair, Vec<u8>) {
    let bytes = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let bytes = bytes.as_ref().to_vec();
    (Ed25519KeyPair::from_pkcs8(&bytes).unwrap(), bytes)
}

fn public_key(key: &Ed25519KeyPair) -> Ed25519PublicKey {
    Ed25519PublicKey::from_bytes(key.public_key().as_ref().try_into().unwrap())
}

fn sign(key: &Ed25519KeyPair, message: &[u8]) -> Ed25519Signature {
    Ed25519Signature::from_bytes(key.sign(message).as_ref().try_into().unwrap())
}
