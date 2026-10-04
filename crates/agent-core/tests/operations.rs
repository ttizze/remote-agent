use agent_core::client::ClientExt;
use agent_protocol::requests::{Answer, Request};
#[allow(dead_code)]
#[path = "support/host.rs"]
mod host_fixture;
use agent_core::state::operations::{
    CancelAccountLogin, CreateSession, ForkSession, Interrupt, ListAccounts, ListFiles,
    ListSessions, ReadAccountLogin, ReadFile, ReadItem, ReadThread, ReadWorktreeSettings,
    ReviewWorkspace, SelectAccount, StartAccountLogin, UpdateWorktreeSettings,
};
use agent_protocol::{
    models::{ListQuery, WorktreeSettings},
    operations::*,
};
use agent_transport::{client::Client, peer::PeerError};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}
fn optional<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str()
}
async fn execute(client: &Client, command: &Value) -> Result<Value, PeerError> {
    macro_rules! call {
        ($operation:expr) => {
            serde_json::to_value(client.call(&$operation).await?).unwrap()
        };
    }
    let result = match text(command, "type") {
        "listThreads" => {
            let query: ListQuery = serde_json::from_value(command["query"].clone()).unwrap();
            call!(ListSessions { query })
        }
        "startThread" => call!(CreateSession {
            provider: agent_protocol::session::ProviderKind::Codex,
            cwd: optional(command, "cwd").map(str::to_owned),
            model: serde_json::from_value(command["model"].clone()).unwrap()
        }),
        "readThread" | "readOlder" => serde_json::to_value(
            client
                .call(&ReadThread {
                    limit: if command["type"] == "readOlder" {
                        10
                    } else {
                        5
                    },
                    ..ReadThread::new(serde_json::from_value(command["threadId"].clone()).unwrap())
                })
                .await?
                .response,
        )
        .unwrap(),
        "readItem" => call!(ReadItem {
            thread_id: serde_json::from_value(command["threadId"].clone()).unwrap(),
            turn_id: text(command, "turnId").into(),
            item_id: text(command, "itemId").into()
        }),
        "sendTurn" => {
            let input = &command["input"];
            let mut items = Vec::new();
            if !text(input, "text").is_empty() {
                items.push(Input::Text {
                    text: text(input, "text").to_owned(),
                });
            }
            for attachment in input["attachments"].as_array().unwrap() {
                items.push(if attachment["isImage"] == true {
                    Input::LocalImage {
                        path: text(attachment, "path").to_owned(),
                    }
                } else {
                    Input::Mention {
                        path: text(attachment, "path").to_owned(),
                        name: text(attachment, "name").to_owned(),
                    }
                });
            }
            let reply = client
                .call(&Submission {
                    thread_id: serde_json::from_value(command["threadId"].clone()).unwrap(),
                    client_user_message_id: text(input, "clientUserMessageId").into(),
                    input: items,
                    model: serde_json::from_value(command["model"].clone()).unwrap(),
                    effort: optional(command, "effort").map(str::to_owned),
                    service_tier: optional(command, "serviceTierForTurn").map(str::to_owned),
                })
                .await?
                .turn_id;
            json!(reply)
        }
        "interruptTurn" => {
            call!(Interrupt {
                thread_id: serde_json::from_value(command["threadId"].clone()).unwrap(),
                turn_id: text(command, "turnId").into()
            });
            Value::Null
        }
        "models" => json!(client.models().await?.data),
        "sessionImages" => json!(
            client
                .session_images(
                    &serde_json::from_value(command["threadId"].clone()).unwrap(),
                    None
                )
                .await?
        ),
        "transcribe" => json!(
            client
                .call(&Transcribe {
                    preparation: None,
                    audio: base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        text(command, "audio")
                    )
                    .unwrap()
                })
                .await?
                .text
        ),
        "respond" => {
            let request: Request = serde_json::from_value(command["request"].clone()).unwrap();
            let answer: Answer = serde_json::from_value(command["answer"].clone())
                .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            client.respond(&request, &answer).await?;
            Value::Null
        }
        "listFiles" => call!(ListFiles {
            path: text(command, "path").to_owned()
        }),
        "readFile" => call!(ReadFile {
            path: text(command, "path").to_owned(),
            discard_draft: false
        }),
        "writeFile" => call!(WriteFile {
            path: text(command, "path").to_owned(),
            revision: text(command, "revision").to_owned(),
            text: text(command, "text").to_owned()
        }),
        "reviewWorkspace" => call!(ReviewWorkspace {
            cwd: text(command, "cwd").to_owned()
        }),
        "worktreeSettings" => call!(ReadWorktreeSettings {}),
        "updateWorktreeSettings" => {
            let settings: WorktreeSettings =
                serde_json::from_value(command["settings"].clone()).unwrap();
            call!(UpdateWorktreeSettings { settings })
        }
        "accounts" => call!(ListAccounts {}),
        "selectAccount" => call!(SelectAccount {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: text(command, "accountId").to_owned()
        }),
        "startAccountLogin" => call!(StartAccountLogin {
            provider: agent_protocol::session::ProviderKind::Codex
        }),
        "accountLoginStatus" => call!(ReadAccountLogin {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: text(command, "loginId").to_owned(),
            thread_id: None,
        }),
        "cancelAccountLogin" => {
            call!(CancelAccountLogin {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: text(command, "loginId").to_owned()
            });
            Value::Null
        }
        "forkThread" => call!(ForkSession {
            thread_id: serde_json::from_value(command["threadId"].clone()).unwrap(),
            last_turn_id: text(command, "lastTurnId").into(),
        }),
        kind => panic!("unknown operation {kind}"),
    };
    Ok(result)
}

async fn run_case(case: &Value) {
    let ((peer, _events), mut reader, writer) = host_fixture::connect(&Default::default()).await;
    let peer = Arc::new(peer);
    let server = async {
        for exchange in case["exchanges"].as_array().unwrap() {
            let request = reader
                .read_request()
                .await
                .unwrap()
                .expect("expected exchange");
            if let Some(reply) = exchange.get("reply") {
                assert_eq!(&*request, reply, "{}", case["name"]);
            } else {
                assert_eq!(request["method"], exchange["method"], "{}", case["name"]);
                let method = request["method"].as_str().unwrap();
                let normalize = |params: Value| {
                    agent_protocol::protocol::json_boundary::call(method, params)
                        .unwrap()
                        .params_json()
                        .unwrap()
                };
                assert_eq!(
                    normalize(request["params"].clone()),
                    normalize(exchange["params"].clone()),
                    "{}",
                    case["name"]
                );
                writer
                    .reply(&request, exchange["response"].clone())
                    .await
                    .unwrap();
            }
        }
        assert!(
            reader.read_request().await.unwrap().is_none(),
            "unexpected additional request: {}",
            case["name"]
        );
    };
    let operation = async {
        let result = execute(&peer, &case["command"]).await;
        peer.close().await;
        result
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(operation, server)
    })
    .await
    .expect("bounded operation");
    if let Some(expected) = case.get("result") {
        let expected = match text(&case["command"], "type") {
            "listThreads" => normalize::<agent_protocol::models::ThreadList>(expected),
            "startThread" | "readThread" | "readOlder" | "forkThread" => {
                normalize::<agent_protocol::models::ThreadResponse>(expected)
            }
            "readItem" => normalize::<ItemResponse>(expected),
            "models" => normalize::<Vec<agent_protocol::models::Model>>(expected),
            "accounts" => normalize::<Accounts>(expected),
            "selectAccount" => normalize::<AccountSelection>(expected),
            "startAccountLogin" => normalize::<AccountLogin>(expected),
            "accountLoginStatus" => normalize::<AccountLoginStatus>(expected),
            _ => expected.clone(),
        };
        assert_eq!(result.unwrap(), expected, "{}", case["name"]);
    } else {
        let error = result.expect_err("expected operation failure");
        let expected = case["errorContains"].as_str().unwrap();
        assert!(
            error.to_string().contains(expected),
            "{}: {error}; expected {expected}",
            case["name"]
        );
        if !case["errorRaw"].is_null() {
            let raw = match error {
                PeerError::Remote { error, .. } => error,
                PeerError::InvalidResponse { raw, .. } => raw,
                other => panic!("missing raw error: {other}"),
            };
            assert_eq!(
                serde_json::from_str::<Value>(&raw).unwrap(),
                if case["errorRaw"].get("response").is_some() {
                    normalize::<agent_protocol::session::OpenedSession>(&case["errorRaw"])
                } else if case["errorRaw"].get("item").is_some() {
                    normalize::<ItemResponse>(&case["errorRaw"])
                } else {
                    normalize::<agent_protocol::error::RpcFailure>(&case["errorRaw"])
                },
                "{}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn operation_corpus() {
    let operations: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/operations.json")).unwrap();
    let host: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/host-operations.json")).unwrap();
    let cases: Vec<_> = operations.iter().chain(&host).collect();
    for case in cases {
        run_case(case).await;
    }
}

fn normalize<T: serde::de::DeserializeOwned + serde::Serialize>(value: &Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(value.clone()).unwrap()).unwrap()
}
