use agent_core::peer::{JsonlReader, JsonlWriter};
use agent_core::transport::{Endpoint, Identity, Relays, Trust};
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
        let session = endpoint
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let stream = session.accept_stream().await.unwrap();
        let (read, write) = tokio::io::split(stream);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        assert_eq!(reader.read_line().await.unwrap().as_deref(), Some(""));
        let approval =
            (mode == "approve").then(|| serde_json::from_str::<Value>(command[1]).unwrap());
        let mut handled = false;
        let mut reads = 0;
        let mut lists = 0;
        // Connected owns initial reads; the command consumes that same state.
        while let Ok(Some(line)) = reader.read_line().await {
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str() {
                Some("host/session/scope") => json!("fixture-storage"),
                Some("host/session/request") => {
                    assert_eq!(request["params"]["requestId"], *approval.as_ref().unwrap());
                    json!({"provider":"codex","id":"fixture-thread"})
                }
                Some("host/session/close") => json!({}),
                Some("host/session/answer") => {
                    assert_eq!(
                        request["params"],
                        json!({"requestId":approval.as_ref().unwrap(),"result":{"decision":"decline"}})
                    );
                    assert!(!handled);
                    handled = true;
                    json!({})
                }
                Some("model/list") => json!({"data":[],"nextCursor":null}),
                Some("host/thread/list") => {
                    lists += 1;
                    let (projects, chats, search) = if mode == "list" {
                        (9, 11, "CLI search")
                    } else {
                        (5, 5, "")
                    };
                    assert_eq!(
                        request["params"],
                        json!({"projectLimit":projects,"chatLimit":chats,"projectThreadLimits":{},"searchTerm":search})
                    );
                    if mode == "list" {
                        assert!(!handled);
                        handled = true;
                    }
                    json!({"data":[{"id":"fixture-thread","name":"CLI fixture"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})
                }
                Some("host/session/open") => {
                    reads += 1;
                    assert_eq!(
                        request["params"],
                        json!({"session":{"provider":"codex","id":"fixture-thread"},"limit":5})
                    );
                    let mut thread = json!({"id":"fixture-thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]});
                    if let Some(id) = &approval {
                        thread["requests"] = json!({id.to_string():{"id":id,"method":"item/commandExecution/requestApproval","params":{"threadId":"fixture-thread"}}});
                    }
                    json!({"session":request["params"]["session"],"subscriptionId":"00000000-0000-0000-0000-000000000001","revision":0,"response":{"thread":thread}})
                }
                Some("host/workspace/review") => {
                    assert_eq!(request["params"], json!({"cwd":"/fixture"}));
                    json!({"branch":"main","additions":0,"deletions":0,"files":[],"diff":""})
                }
                Some("turn/start") => {
                    assert_eq!(mode, "send");
                    assert!(!handled);
                    assert_eq!(
                        request["params"],
                        json!({"threadId":"fixture-thread","clientUserMessageId":"fixture-message","input":[{"type":"text","text":"hello","text_elements":[]}]})
                    );
                    handled = true;
                    json!({"turn":{"id":"fixture-turn"}})
                }
                Some(method) => panic!("unexpected command RPC: {method}"),
                None => panic!("unframed approval response is retired"),
            };
            // Keep QUIC alive until the command consumes its reply and closes.
            writer
                .write_line(&json!({"id":request["id"],"result":result}).to_string())
                .await
                .unwrap();
        }
        assert!(handled);
        assert_eq!(reads, usize::from(mode == "send" || mode == "approve"));
        if mode == "list" {
            assert_eq!(lists, 1);
        }
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
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    endpoint.close().await;
}
#[tokio::test]
async fn cli_lists_over_iroh() {
    exercise(&["list", "--project-limit", "9", "--chat-limit", "11", "--search", "CLI search"],json!({"data":[{"id":"fixture-thread","name":"CLI fixture"}],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})).await;
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
