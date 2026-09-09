use std::time::{Duration, Instant};

use host_protocol::{Ed25519PublicKey, RelayEndpoint};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use tokio::net::TcpListener;

use crate::client::*;

fn valid_config() -> MobileClientConfig {
    MobileClientConfig {
        relay: RelayEndpoint {
            relay_url: "ws://127.0.0.1:49152/socket/websocket".into(),
            runner_id: "runner-test".into(),
            relay_token: "test-relay-token".into(),
        },
        host_identity: Ed25519PublicKey::from_bytes([1; 32]),
        device_name: "iPhone".into(),
        pairing_ticket: None,
        request_timeout: Duration::from_secs(1),
    }
}

#[test]
fn pairing_config_rejects_invalid_names_and_missing_relay_identity() {
    let mut config = valid_config();
    assert!(config.validate().is_ok());
    config.relay.runner_id.clear();
    assert!(config.validate().is_err());
    config = valid_config();
    config.device_name = "phone\nother".into();
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn connection_attempt_is_bounded_by_the_configured_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let stalled = tokio::spawn(async move {
        let (_connection, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let mut config = valid_config();
    config.relay.relay_url = format!("ws://{address}/socket/websocket");
    config.request_timeout = Duration::from_millis(50);
    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let started = Instant::now();
    let result = MobileClient::connect(config, key.as_ref()).await;
    assert!(matches!(result, Err(MobileClientError::ConnectionTimeout)));
    assert!(started.elapsed() < Duration::from_millis(500));
    stalled.abort();
    let _ = stalled.await;
}

#[tokio::test]
async fn invalid_private_key_is_rejected_before_attempting_a_connection() {
    assert!(matches!(
        MobileClient::connect(valid_config(), b"not a private key").await,
        Err(MobileClientError::InvalidDeviceKey)
    ));
}

#[tokio::test]
async fn replayed_requests_and_initial_events_survive_until_the_first_subscriber() {
    use host_protocol::{JsonlReader, JsonlWriter};
    use serde_json::json;
    let (client, server) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(client);
    let peer = crate::peer::RpcPeer::open(
        host_protocol::JsonlReader::with_max_message_bytes(reader, 8192),
        writer,
        Duration::from_secs(1),
        1024,
        crate::peer::EventDelivery::SplitRequests,
    )
    .unwrap();
    let host = tokio::spawn(async move {
        let (read, write) = tokio::io::split(server);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        writer.write_line(r#"{"id":"approval-on-reconnect","method":"item/fileChange/requestApproval","params":{"threadId":"thread"}}"#).await.unwrap();
        writer
            .write_line(r#"{"method":"turn/started","params":{"turn":{"id":"turn"}}}"#)
            .await
            .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        writer
            .write_line(&json!({"id":request["id"],"result":{}}).to_string())
            .await
            .unwrap();
        // Keep the connection open until the assertion finishes.
        let _ = reader.read_line().await;
    });
    // Receiving this response proves the read loop already consumed both
    // earlier events. No scheduling sleep or timing assumption is involved.
    peer.request("thread/list", &json!({})).await.unwrap();
    let mut requests = peer.subscribe_server_requests();
    let mut notifications = peer.subscribe();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &requests
                .try_recv()
                .expect("pending approval lost before subscribing")
        )
        .unwrap()["id"],
        "approval-on-reconnect"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &notifications
                .try_recv()
                .expect("initial event lost before subscribing")
        )
        .unwrap()["method"],
        "turn/started"
    );
    drop(peer);
    host.abort();
    let _ = host.await;
}
