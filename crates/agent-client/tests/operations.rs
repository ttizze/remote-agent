use std::{sync::Arc, time::Duration};

use agent_client::operations::AgentClient;
use host_protocol::RpcPeer;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The same canonical replies are projected by the Kotlin adapter tests.
/// The Rust side exercises the real RPC peer, operation, and intent decoder.
#[tokio::test]
async fn operation_corpus_preserves_requests_results_and_failures() {
    let mut cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/operations.json")).unwrap();
    cases.extend(
        serde_json::from_str::<Vec<Value>>(include_str!("fixtures/host-operations.json")).unwrap(),
    );
    for mut case in cases {
        let name = case["name"].as_str().unwrap().to_owned();
        tokio::time::timeout(Duration::from_secs(3), async {
            let (client, server) = tokio::io::duplex(4096);
            let (reader, writer) = tokio::io::split(client);
            let peer = Arc::new(RpcPeer::open(reader, writer, 1024 * 1024, 4, |_| {}));
            let agent = AgentClient::new(peer.clone(), Duration::from_secs(1));
            let Value::Array(exchanges) = case["exchanges"].take() else {
                panic!("fixture exchanges must be an array")
            };
            let serve = tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(server);
                let mut reader = BufReader::new(reader);
                let mut line = String::new();
                for mut exchange in exchanges {
                    line.clear();
                    assert!(reader.read_line(&mut line).await.unwrap() > 0);
                    let request: Value = serde_json::from_str(&line).unwrap();
                    assert_eq!(request["method"], exchange["method"]);
                    assert_eq!(request["params"], exchange["params"]);
                    let mut response = exchange["response"].take();
                    response["id"] = request["id"].clone();
                    writer
                        .write_all(response.to_string().as_bytes())
                        .await
                        .unwrap();
                    writer.write_all(b"\n").await.unwrap();
                }
                // An extra turn/start after a rejected resume fails here.
                line.clear();
                assert_eq!(
                    reader.read_line(&mut line).await.unwrap(),
                    0,
                    "unexpected request: {line}"
                );
            });
            let result = agent.command_json(&case["command"].to_string()).await;
            if let Some(expected) = case["errorContains"].as_str() {
                let error = result.unwrap_err();
                assert!(error.to_string().contains(expected), "{name}: {error}");
                let native: Value = serde_json::from_str(&error.into_native_error()).unwrap();
                assert_eq!(native["rawError"], case["errorRaw"], "{name}");
            } else {
                let value: Value = serde_json::from_str(&result.unwrap()).unwrap();
                assert_eq!(value, case["result"], "{name}");
            }
            peer.close();
            serve.await.unwrap();
        })
        .await
        .unwrap_or_else(|_| panic!("timed out: {name}"));
    }
}

#[tokio::test]
async fn snapshot_completion_precedes_the_next_wire_notification() {
    let (client, server) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(client);
    let (ordered, mut received) = tokio::sync::mpsc::unbounded_channel();
    let events = ordered.clone();
    let peer = Arc::new(RpcPeer::open(reader, writer, 4096, 4, move |event| {
        if matches!(event, host_protocol::RpcEvent::Message(_)) {
            events.send("delta").unwrap();
        }
    }));
    let agent = AgentClient::new(peer.clone(), Duration::from_secs(1));
    let read = tokio::spawn(async move {
        agent
            .read_thread_with("thread".into(), true, move |result| {
                result.unwrap();
                ordered.send("snapshot").unwrap();
            })
            .await;
    });
    let serve = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let reply =
            serde_json::json!({"id":request["id"],"result":{"thread":{"id":"thread","turns":[]}}});
        let delta = serde_json::json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"t","itemId":"a","delta":"latest"}});
        writer
            .write_all(format!("{reply}\n{delta}\n").as_bytes())
            .await
            .unwrap();
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap(),
        Some("snapshot")
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap(),
        Some("delta")
    );
    read.await.unwrap();
    serve.await.unwrap();
    peer.close();
}
