use agent_core::client::ClientExt;
use agent_protocol::requests::{Answer, Request};
use agent_protocol::{
    conversation::{self as decision, records},
    session::{ProviderKind, SessionRef},
};
#[allow(dead_code)]
#[path = "support/host.rs"]
mod host_fixture;
use agent_core::state::operations::{
    CancelAccountLogin, ListAccounts, ListFiles, ReadAccountLogin, ReadFile, ReadWorktreeSettings,
    ReviewWorkspace, SelectAccount, StartAccountLogin, UpdateWorktreeSettings,
};
use agent_protocol::{models::WorktreeSettings, operations::*};
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
        "subscribeList" => {
            let output = client
                .call(&records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::from_u128(2),
                    cached: None,
                })
                .await?;
            json!(output.initial)
        }
        "searchList" => call!(records::SearchList {
            term: text(command, "term").into(),
            limit: Some(command["limit"].as_u64().unwrap() as u32)
        }),
        "startThread" => {
            let opened = client
                .call(&records::Create {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    session: SessionRef {
                        provider: None,
                        id: uuid::Uuid::from_u128(3).to_string(),
                    },
                    subscription: uuid::Uuid::from_u128(2),
                    cwd: optional(command, "cwd").map(str::to_owned),
                    selection: records::Selection {
                        model: serde_json::from_value(command["model"].clone()).unwrap(),
                        ..Default::default()
                    },
                    initial: None,
                })
                .await?;
            json!(opened.session)
        }
        "readThread" => {
            let id: SessionRef = serde_json::from_value(command["threadId"].clone()).unwrap();
            let opened = client
                .call(&records::Subscribe {
                    session: id.clone(),
                    subscription: uuid::Uuid::from_u128(2),
                    cached: None,
                    window: None,
                })
                .await?;
            let replica = records::Replica::new(agent_protocol::sync::Scope::Conversation(id), 1)
                .apply(1, opened.initial)
                .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            json!(
                records::project(&replica).ok_or_else(|| PeerError::InvalidMessage(
                    "conversation header is missing".into()
                ))?
            )
        }
        "readItem" => {
            let session: SessionRef = serde_json::from_value(command["threadId"].clone()).unwrap();
            client
                .call(&records::Subscribe {
                    session: session.clone(),
                    subscription: uuid::Uuid::from_u128(2),
                    cached: None,
                    window: None,
                })
                .await?;
            let response = client
                .call(&records::Hydrate {
                    session,
                    subscription: uuid::Uuid::from_u128(2),
                    cached: agent_protocol::sync::Cursor {
                        store: uuid::Uuid::from_u128(1),
                        sequence: 7,
                    },
                    item: text(command, "itemId").into(),
                })
                .await?;
            let item = response
                .messages
                .into_iter()
                .find_map(|message| {
                    let agent_protocol::sync::Message::Hydrate { row, .. } = message else {
                        return None;
                    };
                    let record = match row.content {
                        agent_protocol::sync::Content::Full(record)
                        | agent_protocol::sync::Content::Summary(record) => record,
                    };
                    match record.as_ref() {
                        records::Record::Item(item) => Some(item.content.as_ref().clone()),
                        _ => None,
                    }
                })
                .ok_or_else(|| PeerError::InvalidMessage("item is missing".into()))?;
            json!(ItemResponse {
                item,
                transfer: None
            })
        }
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
            let id: agent_protocol::ids::ClientInputId = text(input, "clientUserMessageId").into();
            call!(records::Change {
                session: serde_json::from_value(command["threadId"].clone()).unwrap(),
                operation: uuid::Uuid::parse_str(id.as_str()).unwrap(),
                command: decision::Command::Submit {
                    id,
                    content: decision::InputContent {
                        parts: items,
                        model: serde_json::from_value(command["model"].clone()).unwrap(),
                        effort: optional(command, "effort").map(str::to_owned),
                        service_tier: optional(command, "serviceTierForTurn").map(str::to_owned),
                        follow_up: serde_json::from_value(command["followUp"].clone())
                            .unwrap_or_default(),
                    }
                },
            })
        }
        "interruptTurn" => {
            client
                .call(&records::Change {
                    session: serde_json::from_value(command["threadId"].clone()).unwrap(),
                    operation: uuid::Uuid::from_u128(1),
                    command: decision::Command::Interrupt {
                        turn: Some(text(command, "turnId").into()),
                    },
                })
                .await?
                .accepted()
                .map_err(|error| PeerError::InvalidMessage(error.message))?;
            Value::Null
        }
        "models" => json!(
            client
                .models(agent_protocol::session::ProviderKind::Codex)
                .await?
                .data
        ),
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
            client
                .respond(
                    &SessionRef {
                        provider: Some(ProviderKind::Codex),
                        id: "thread-1".into(),
                    },
                    &request,
                    &answer,
                )
                .await?;
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
            provider: agent_protocol::session::ProviderKind::Codex,
            thread_id: None,
        }),
        "accountLoginStatus" => call!(ReadAccountLogin {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: text(command, "loginId").to_owned(),
        }),
        "cancelAccountLogin" => {
            call!(CancelAccountLogin {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: text(command, "loginId").to_owned()
            });
            Value::Null
        }
        "forkThread" => call!(records::Fork {
            source: serde_json::from_value(command["threadId"].clone()).unwrap(),
            target: SessionRef {
                provider: None,
                id: uuid::Uuid::from_u128(3).to_string()
            },
            through: text(command, "lastTurnId").into(),
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
                let mut params = request["params"].clone();
                if let Some(expected) = exchange["params"].get("operation") {
                    assert!(uuid::Uuid::parse_str(params["operation"].as_str().unwrap()).is_ok());
                    if case["command"]["type"] == "respond" {
                        params["operation"] = expected.clone();
                    }
                }
                if let Some(expected) = exchange["params"].get("subscription") {
                    assert!(
                        uuid::Uuid::parse_str(params["subscription"].as_str().unwrap()).is_ok()
                    );
                    params["subscription"] = expected.clone();
                }
                assert_eq!(
                    normalize(params),
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
    .unwrap_or_else(|error| panic!("bounded operation {}: {error}", text(case, "name")));
    if let Some(expected) = case.get("result") {
        let expected = match text(&case["command"], "type") {
            "searchList" => normalize::<agent_protocol::models::ThreadList>(expected),
            "readThread" | "readOlder" => expected.clone(),
            "readItem" => normalize::<ItemResponse>(expected),
            "models" => normalize::<Vec<agent_protocol::models::Model>>(expected),
            "accounts" => normalize::<Accounts>(expected),
            "selectAccount" => normalize::<AccountSelection>(expected),
            "startAccountLogin" => normalize::<AccountLogin>(expected),
            "accountLoginStatus" => normalize::<AccountLoginStatus>(expected),
            _ => expected.clone(),
        };
        let actual = result.unwrap();
        if matches!(text(&case["command"], "type"), "readThread" | "readOlder") {
            assert_fields(&actual, &expected, text(case, "name"));
        } else {
            assert_eq!(actual, expected, "{}", case["name"]);
        }
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
                if case["errorRaw"].get("item").is_some() {
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
    for case in operations.iter().chain(&host) {
        run_case(case).await;
    }
}

fn normalize<T: serde::de::DeserializeOwned + serde::Serialize>(value: &Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(value.clone()).unwrap()).unwrap()
}

fn assert_fields(actual: &Value, expected: &Value, name: &str) {
    if let Some(fields) = expected.as_object() {
        for (key, value) in fields {
            assert_fields(&actual[key], value, name);
        }
    } else if let Some(values) = expected.as_array() {
        assert_eq!(actual.as_array().unwrap().len(), values.len(), "{name}");
        for (actual, expected) in actual.as_array().unwrap().iter().zip(values) {
            assert_fields(actual, expected, name);
        }
    } else {
        assert_eq!(actual, expected, "{name}");
    }
}
