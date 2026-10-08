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
        while let Ok(Some(request)) = reader.read_request().await {
            // The CLI closes once its command completes, cancelling unrelated initial reads.
            if handled
                && matches!(
                    request["method"].as_str(),
                    Some("host/model/list" | "host/account/list" | "host/taskActivity/read")
                )
            {
                continue;
            }
            let result = match request["method"].as_str() {
                Some("host/session/scope") => json!("fixture-storage"),
                Some("host/session/request") => {
                    assert_eq!(request["params"]["requestId"], *approval.as_ref().unwrap());
                    json!({"provider":"codex","id":"fixture-thread"})
                }
                Some("host/session/answer") => {
                    assert_eq!(
                        request["params"],
                        json!({"requestId":approval.as_ref().unwrap(),"answer":{"approval":{"choiceId":"decline-choice"}}})
                    );
                    assert!(!handled);
                    handled = true;
                    json!({})
                }
                Some("host/model/list") => json!({"data":[],"nextCursor":null}),
                Some("host/account/list") => json!({"accounts":[], "selected":{}}),
                Some("host/taskActivity/read") => {
                    json!({"revision":0,"display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
                }
                Some("host/session/list") => {
                    lists += 1;
                    let (limit, search) = if mode == "list" {
                        (11, "CLI search")
                    } else {
                        (30, "")
                    };
                    assert_eq!(
                        request["params"],
                        json!({"limit":limit,"searchTerm":search})
                    );
                    if mode == "list" {
                        assert!(!handled);
                        handled = true;
                    }
                    json!({"data":[{"id":{"provider":"codex","id":"fixture-thread"},"name":"CLI fixture"}],"projects":[],"hasMore":false,})
                }
                Some("host/session/open") => {
                    reads += 1;
                    assert_eq!(
                        request["params"],
                        json!({"session":{"provider":"codex","id":"fixture-thread"},"limit":5,"includeActivity":false})
                    );
                    let mut thread = json!({"id":{"provider":"codex","id":"fixture-thread"},"cwd":"/fixture","status":"idle","turns":[]});
                    if let Some(id) = &approval {
                        thread["requests"] = json!({id.as_str().unwrap():{"id":id,"target":"session","delivery":"awaiting","body":{"approval":{"kind":"command","description":"fixture","details":"","choices":[{"id":"accept-choice","label":"承認","description":""},{"id":"session-choice","label":"セッション中","description":""},{"id":"decline-choice","label":"拒否","description":""}]}}}});
                    }
                    json!({"session":request["params"]["session"],"subscriptionId":"00000000-0000-0000-0000-000000000001","revision":0,"response":{"thread":thread}})
                }
                Some("host/workspace/review") => {
                    assert_eq!(request["params"], json!({"cwd":"/fixture"}));
                    json!({"branch":"main","additions":0,"deletions":0,"files":[],"diff":""})
                }
                Some("host/session/submit") => {
                    assert_eq!(mode, "send");
                    assert!(!handled);
                    assert_eq!(
                        request["params"],
                        json!({"threadId":{"provider":"codex","id":"fixture-thread"},"clientUserMessageId":"fixture-message","model":null,"effort":null,"serviceTierForTurn":null,"input":[{"text":{"text":"hello"}}]})
                    );
                    handled = true;
                    json!({"turnId":"fixture-turn"})
                }
                Some(method) => panic!("unexpected command RPC: {method}"),
                None => panic!("unframed approval response is retired"),
            };
            // Keep QUIC alive until the command consumes its reply and closes.
            writer
                .reply(&request, json!({"result":result}))
                .await
                .unwrap();
        }
        session.close();
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
        if mode == "list" {
            serde_json::to_value(
                serde_json::from_value::<agent_protocol::models::ThreadList>(expected).unwrap(),
            )
            .unwrap()
        } else {
            expected
        }
    );
    endpoint.close().await;
}
#[tokio::test]
async fn cli_lists_over_iroh() {
    exercise(&["list", "--limit", "11", "--search", "CLI search"],json!({"data":[{"id":{"provider":"codex","id":"fixture-thread"},"name":"CLI fixture"}],"projects":[],"hasMore":false,})).await;
}
#[tokio::test]
async fn cli_sends_over_iroh() {
    exercise(
        &[
            "send",
            r#"{"provider":"codex","id":"fixture-thread"}"#,
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
        &["approve", "fixture-approval", "--decision", "2"],
        Value::Null,
    )
    .await;
}
