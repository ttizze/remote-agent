use agent_core::transport::{Endpoint, Identity, Relays, Trust, authorize};
use host_protocol::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

async fn exercise(command: &[&str], expected: Value) {
    let client_identity = Identity::generate();
    let trust = Trust {
        allowed: BTreeSet::from([client_identity.node_id()]),
        ..Default::default()
    };
    let directory = tempfile::tempdir().unwrap();
    let secret_path = directory.path().join("client-key");
    std::fs::write(&secret_path, client_identity.to_bytes()).unwrap();
    let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
        .await
        .unwrap();
    let ticket = endpoint.ticket().to_string();
    let mode = command[0];
    let server = async {
        let session = endpoint.accept().await.unwrap();
        assert!(
            authorize(&trust, session.node_id(), None, 0)
                .unwrap()
                .is_none()
        );
        let stream = session.accept_stream().await.unwrap();
        let (read, write) = tokio::io::split(stream);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        let first: Value =
            serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
        if mode == "send" {
            assert_eq!(first["method"], "host/thread/read");
            assert_eq!(
                first["params"],
                json!({"threadId":"fixture-thread","includeTurns":true,"paginateHistory":true,"deferItemDetails":true})
            );
            writer.write_line(&json!({"id":first["id"],"result":{"thread":{"id":"fixture-thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]}}}).to_string()).await.unwrap();
            let send: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(send["method"], "turn/start");
            assert_eq!(
                send["params"],
                json!({"threadId":"fixture-thread","clientUserMessageId":"fixture-message","input":[{"type":"text","text":"hello","text_elements":[]}]})
            );
            writer
                .write_line(
                    &json!({"id":send["id"],"result":{"turn":{"id":"fixture-turn"}}}).to_string(),
                )
                .await
                .unwrap();
        } else {
            assert_eq!(first["method"], "host/thread/list");
            assert_eq!(
                first["params"],
                json!({"titleOnly":true,"projectLimit":5,"chatLimit":5,"projectThreadLimits":{},"searchTerm":""})
            );
            writer.write_line(&json!({"id":first["id"],"result":{"data":[{"id":"fixture-thread","name":"CLI fixture"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false}}).to_string()).await.unwrap();
            if mode == "approve" {
                let request_id: Value = serde_json::from_str(command[1]).unwrap();
                writer.write_line(&json!({"id":request_id,"method":"item/commandExecution/requestApproval","params":{"threadId":"fixture-thread"}}).to_string()).await.unwrap();
                let answer: Value =
                    serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(
                    answer,
                    json!({"id":request_id,"result":{"decision":"decline"}})
                );
            }
        }
        // Keep the QUIC endpoint alive until the CLI has consumed its reply.
        let _ = reader.read_line().await;
    };
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_agent-cli"))
        .arg("--ticket")
        .arg(ticket)
        .arg("--identity-file")
        .arg(&secret_path)
        .arg("--no-relay")
        .args(command)
        .kill_on_drop(true)
        .output();
    let child = async {
        let output = child.await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let ((), output) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(server, child)
    })
    .await
    .expect("bounded CLI session");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    endpoint.close().await;
}
#[tokio::test]
async fn cli_lists_over_iroh() {
    exercise(&["list"],json!({"data":[{"id":"fixture-thread","name":"CLI fixture"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})).await;
}
#[tokio::test]
async fn cli_sends_over_iroh() {
    exercise(
        &[
            "send",
            "fixture-thread",
            "hello",
            "--client-message-id",
            "fixture-message",
        ],
        json!("fixture-turn"),
    )
    .await;
}
#[tokio::test]
async fn cli_answers_approval_over_iroh() {
    exercise(&["approve", "7", "--decision", "2"], Value::Null).await;
}

#[tokio::test]
async fn cli_preserves_string_approval_ids_over_iroh() {
    exercise(
        &["approve", r#""fixture-approval""#, "--decision", "2"],
        Value::Null,
    )
    .await;
}
