use agent_core::{
    diagnostics,
    peer::{JsonlReader, RpcPeer},
};
use serde_json::{Value, json};
use std::{fs, time::Duration};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn rpc_failure_is_persisted_before_delivery_and_success_preserves_it() {
    let directory = tempfile::tempdir().unwrap();
    // Calling the same RPC log site before initialization must not disable it
    // after the process-wide subscriber is installed.
    diagnostics::rpc_error(
        "before-initialize",
        None,
        serde_json::value::RawValue::from_string(
            r#"{"message":"PRIVATE_BEFORE_INITIALIZE"}"#.into(),
        )
        .unwrap()
        .as_ref(),
    );
    assert!(!directory.path().join("logs").exists());
    diagnostics::initialize(
        directory.path(),
        diagnostics::Component::Desktop,
        "test-application",
    )
    .unwrap();
    let (client, server) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(client);
    let peer = RpcPeer::open(
        JsonlReader::new(reader),
        writer,
        Some(Duration::from_secs(2)),
        4,
    )
    .unwrap();
    let fixture = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server);
        let mut reader = JsonlReader::new(reader);
        for failed in [true, false] {
            let line = reader.read_line().await.unwrap().unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = if failed {
                json!({"id":request["id"],"error":{"code":-32603,"message":"failed to read session metadata: rollout is empty","data":{"text":"PRIVATE_RESPONSE"}}})
            } else {
                json!({"id":request["id"],"result":{"ok":true}})
            };
            if !failed {
                for notification in [
                    json!({"method":"error","params":{"error":{"message":"object error","data":{"text":"PRIVATE_ERROR"}}}}),
                    json!({"method":"error","params":{"error":"stream disconnected"}}),
                ] {
                    writer
                        .write_all(format!("{notification}\n").as_bytes())
                        .await
                        .unwrap();
                }
                let notification = json!({"method":"turn/completed","params":{"turn":{"error":{"message":"provider failed\nretry later"},"items":[{"text":"PRIVATE_TURN"}]}}});
                writer
                    .write_all(format!("{notification}\n").as_bytes())
                    .await
                    .unwrap();
            }
            writer
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let first = peer
        .request::<_, Value>("thread/read", &json!({"text":"PRIVATE_REQUEST"}))
        .await;
    assert!(first.is_err());
    let path = directory.path().join("logs/desktop.jsonl");
    let before = fs::read_to_string(&path).unwrap();
    let error: Value = serde_json::from_str(before.lines().last().unwrap()).unwrap();
    assert_eq!(error["operation"], "thread/read");
    assert_eq!(error["errorCode"], -32603);
    assert_eq!(error["version"], "test-application");
    assert_eq!(
        error["message"],
        "failed to read session metadata: rollout is empty"
    );
    let reply = peer
        .request::<_, Value>("thread/read", &json!({}))
        .await
        .unwrap();
    assert_eq!(reply.value["ok"], true);
    fixture.await.unwrap();
    tracing::info!(target: "bex", operation = "shutdown", "Bex shutting down");
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(&before));
    assert!(!after.contains("PRIVATE_"));
    let notification: Value = after
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["operation"] == "turn/completed")
        .unwrap();
    assert_eq!(notification["message"], "provider failed\nretry later");
    let records: Vec<Value> = after
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let causes: Vec<_> = records
        .iter()
        .filter(|record| record["operation"] == "error")
        .map(|record| record["message"].as_str().unwrap())
        .collect();
    assert_eq!(causes, ["object error", "stream disconnected"]);

    // Exercise the process-wide subscriber used by Host and Desktop, including
    // events emitted from async workers above and explicit field filtering here.
    tracing::error!(target: "dependency", operation = "ignored", "PRIVATE_DEPENDENCY");
    tracing::debug!(target: "bex", operation = "ignored", "PRIVATE_DEBUG");
    tracing::error!(target: "bex", operation = "upload", request_id = 42u64,
        payload = "PRIVATE_PAYLOAD", message = "failed password=PRIVATE_PASSWORD");
    diagnostics::rpc_error(
        "read",
        Some(43),
        serde_json::value::RawValue::from_string(
            r#"{"message":"failed","code":"provider_failed","data":"PRIVATE_DATA"}"#.into(),
        )
        .unwrap()
        .as_ref(),
    );
    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.starts_with(&after));
    assert!(!contents.contains("PRIVATE_"), "{contents}");
    let records: Vec<Value> = contents[after.len()..]
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["requestId"], 42);
    assert_eq!(records[0]["operation"], "upload");
    assert_eq!(records[0]["message"], "failed [credential omitted]");
    assert_eq!(records[1]["errorCode"], "provider_failed");
    assert_eq!(records[1]["requestId"], 43);
}
