#[allow(dead_code)]
#[path = "../../agent-core/tests/support/host.rs"]
mod host_fixture;
use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
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
        let (session, mut reader, writer) = host_fixture::accept(session).await;
        let approval = (mode == "approve").then(|| Value::String(command[1].into()));
        let mut handled = false;
        let mut reads = 0;
        let mut lists = 0;
        // Connected owns initial reads; the command consumes that same state.
        loop {
            let request = match reader.read_request().await {
                Ok(Some(request)) => request,
                Ok(None) => break,
                // Once the command has its answer, the process closes QUIC.
                // The child exit status and stdout below verify that it consumed it.
                Err(_) if handled => break,
                Err(error) => panic!("CLI request framing: {error}"),
            };
            let result = match request["method"].as_str() {
                Some("host/device/identify") => {
                    json!({"storageScope":"fixture-storage","ticket":""})
                }
                Some("host/session/request") => {
                    assert_eq!(request["params"]["requestId"], *approval.as_ref().unwrap());
                    json!({"provider":"codex","id":"fixture-thread"})
                }
                Some("host/model/list") => json!({"data":[],"nextCursor":null}),
                Some("host/account/list") => json!({"accounts":[], "selected":{}}),
                Some("host/list/subscribe") => {
                    lists += 1;
                    assert!(request["params"]["cached"].is_null());
                    json!({"data":[{"id":{"provider":"codex","id":"fixture-thread"},"name":"CLI fixture"}],"projects":[],"limit":5,"hasMore":false,"hasMoreProjects":false})
                }
                Some("host/list/search") => {
                    assert_eq!(mode, "list");
                    assert_eq!(request["params"], json!({"term":"CLI search","limit":null}));
                    assert!(!handled);
                    handled = true;
                    json!({"data":[{"id":{"provider":"codex","id":"fixture-thread"},"name":"CLI fixture"}],"projects":[],"limit":10000,"hasMore":false,"hasMoreProjects":false})
                }
                Some("host/conversation/subscribe") => {
                    reads += 1;
                    assert_eq!(
                        request["params"]["session"],
                        json!({"provider":"codex","id":"fixture-thread"})
                    );
                    assert!(request["params"]["cached"].is_null());
                    let mut thread = json!({"id":{"provider":"codex","id":"fixture-thread"},"cwd":"/fixture","status":"idle","turns":[]});
                    if let Some(id) = &approval {
                        thread["turns"] =
                            json!([{"id":"fixture-turn","status":"running","items":[]}]);
                        thread["requests"] = json!({id.as_str().unwrap():{"id":id,"target":{"turn":{"turnId":"fixture-turn","itemId":null}},"delivery":"awaiting","body":{"approval":{"kind":"command","description":"fixture","details":"","choices":[{"id":"accept-choice","label":"承認","description":"","effect":"allow"},{"id":"session-choice","label":"セッション中","description":"","effect":"allowLasting"},{"id":"decline-choice","label":"拒否","description":"","effect":"deny"}]}}}});
                    }
                    json!({"thread":thread})
                }
                Some("host/workspace/review") => {
                    assert_eq!(request["params"], json!({"cwd":"/fixture"}));
                    json!({"controls":agent_protocol::models::WorkspaceControls::default(),"branch":"main","base_ref":"origin/main","additions":0,"deletions":0,"files":[],"diff":""})
                }
                Some("host/conversation/change") => {
                    assert!(!handled);
                    assert_eq!(
                        request["params"]["session"],
                        json!({"provider":"codex","id":"fixture-thread"})
                    );
                    let command = &request["params"]["command"];
                    if mode == "send" {
                        assert_eq!(
                            command["Submit"]["id"],
                            "00000000-0000-0000-0000-000000000007"
                        );
                        assert_eq!(
                            command["Submit"]["content"]["parts"],
                            json!([{"text":{"text":"hello"}}])
                        );
                        assert_eq!(request["params"]["operation"], command["Submit"]["id"]);
                    } else {
                        assert_eq!(
                            *command,
                            json!({"Answer":{"id":approval.as_ref().unwrap(),"answer":{"approval":{"choiceId":"decline-choice"}}}})
                        );
                    }
                    handled = true;
                    json!({})
                }
                Some(method) => panic!("unexpected command RPC: {method}"),
                None => panic!("unframed approval response is retired"),
            };
            // Keep QUIC alive until the command consumes its reply and closes.
            if let Err(error) = writer.reply(&request, json!({"result":result})).await {
                eprintln!("CLI reply ended: {error}");
                break;
            }
            if request["method"] == "host/conversation/change" && mode == "send" {
                writer.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"fixture-thread"},"change":{"type":"turn","turn":{"id":"fixture-turn","status":"running","items":[{"id":"00000000-0000-0000-0000-000000000007","clientInputId":"00000000-0000-0000-0000-000000000007","status":"completed","body":{"inline":{"body":{"userMessage":{"content":[{"text":{"text":"hello"}}]}}}}}]},"completed":false}})).await.unwrap();
            }
        }
        session.close();
        (handled, reads, lists)
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
    let ((handled, reads, lists), output) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(server, child)
    })
    .await
    .expect("bounded CLI session");
    assert!(handled);
    assert_eq!(reads, usize::from(mode == "send" || mode == "approve"));
    if mode == "list" {
        assert_eq!(lists, 1);
    }
    let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    if mode == "list" {
        assert_eq!(actual["data"][0]["id"], expected["data"][0]["id"]);
        assert_eq!(actual["data"][0]["name"], "CLI fixture");
        assert_eq!(actual["limit"], 11);
    } else {
        assert_eq!(actual, expected);
    }
    endpoint.close().await;
}
#[tokio::test]
async fn cli_lists_over_iroh() {
    exercise(
        &["list", "--limit", "11", "--search", "CLI search"],
        json!({"data":[{"id":{"provider":"codex","id":"fixture-thread"},"name":"CLI fixture"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5}),
    )
    .await;
}
#[tokio::test]
async fn cli_sends_over_iroh() {
    exercise(
        &[
            "send",
            r#"{"provider":"codex","id":"fixture-thread"}"#,
            "hello",
            "--client-message-id",
            "00000000-0000-0000-0000-000000000007",
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
        &["approve", "fixture-approval", "--decision", "2"],
        Value::Null,
    )
    .await;
}
