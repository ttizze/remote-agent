use agent_transport::{
    diagnostics,
    peer::{JsonlReader, PeerError, RpcPeer},
};
use serde_json::{Value, json};
use std::{fs, sync::Arc, time::Duration};

#[tokio::test]
async fn expected_close_is_quiet_but_disconnects_and_timeouts_are_retained() {
    let directory = tempfile::tempdir().unwrap();
    diagnostics::initialize(
        directory.path(),
        diagnostics::Component::Desktop,
        "test-application",
    )
    .unwrap();
    for cause in ["intentional", "eof", "timeout", "silent"] {
        let (client, server) = tokio::io::duplex(8192);
        let (server, mut server_writer) = tokio::io::split(server);
        let (reader, writer) = tokio::io::split(client);
        let peer = Arc::new(
            RpcPeer::open(
                JsonlReader::new(reader),
                writer,
                Some(if matches!(cause, "timeout" | "silent") {
                    Duration::from_millis(200)
                } else {
                    Duration::from_secs(2)
                }),
                1,
            )
            .unwrap(),
        );
        let request = {
            let peer = peer.clone();
            tokio::spawn(async move { peer.request::<_, Value>(cause, &json!({})).await })
        };
        let mut server = JsonlReader::new(server);
        tokio::time::timeout(Duration::from_secs(3), server.read_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if cause == "timeout" {
            use tokio::io::AsyncWriteExt;
            server_writer
                .write_all(b"{\"method\":\"progress\",\"params\":{}}\n")
                .await
                .unwrap();
        }
        if cause == "intentional" {
            peer.close().await.unwrap();
        }
        if cause == "eof" {
            drop(server);
            drop(server_writer);
        }
        let error = tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        if cause == "timeout" {
            assert!(matches!(error, PeerError::RequestTimeout { .. }));
        } else {
            assert!(matches!(error, PeerError::ConnectionClosed(_)));
        }
        peer.close().await.unwrap();
    }
    let records: Vec<Value> = fs::read_to_string(directory.path().join("logs/desktop.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let failures: Vec<_> = records
        .iter()
        .filter(|record| record["level"] == "error")
        .map(|record| {
            (
                record["operation"].as_str().unwrap(),
                record["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        failures,
        [
            ("eof", "JSONL stream reached EOF"),
            ("timeout", "RPC request timed out waiting for response"),
            ("silent", "no peer traffic before silent timed out"),
        ]
    );
}
