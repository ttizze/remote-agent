use agent_core::peer::{JsonlReader, JsonlWriter};
use agent_core::state::operations::{
    CancelAccountLogin, ForkThread, Interrupt, ListAccounts, ListFiles, ListThreads,
    ReadAccountLogin, ReadFile, ReadItem, ReadOlder, ReadThread, ReadWorktreeSettings,
    ReviewWorkspace, SelectAccount, StartAccountLogin, StartThread, Unwatch,
    UpdateWorktreeSettings, Watch,
};
use agent_core::{
    client::*,
    models::{ListQuery, Thread, WorktreeSettings},
    peer::{PeerError, RpcPeer},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}
fn optional<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str()
}
async fn execute(client: &Client, command: &Value) -> Result<Value, PeerError> {
    macro_rules! call {
        ($operation:expr) => {
            serde_json::to_value(client.call(&$operation).await?.value).unwrap()
        };
    }
    let result = match text(command, "type") {
        "listThreads" => {
            let query: ListQuery = serde_json::from_value(command["query"].clone()).unwrap();
            call!(ListThreads { query })
        }
        "startThread" => call!(StartThread {
            cwd: optional(command, "cwd")
                .filter(|cwd| !cwd.trim().is_empty())
                .map(str::to_owned),
            model: optional(command, "model").map(str::to_owned)
        }),
        "readThread" => call!(ReadThread {
            thread_id: text(command, "threadId").to_owned(),
            include_turns: true,
            paginate_history: true,
            defer_item_details: command["deferItemDetails"].as_bool().unwrap(),
            open: false
        }),
        "readOlder" => call!(ReadOlder {
            thread_id: text(command, "threadId").to_owned(),
            turn_id: optional(command, "turnId").map(str::to_owned),
            cursor: optional(command, "cursor").map(str::to_owned),
            defer_item_details: true,
        }),
        "readItem" => call!(ReadItem {
            thread_id: text(command, "threadId").to_owned(),
            turn_id: text(command, "turnId").to_owned(),
            item_id: text(command, "itemId").to_owned()
        }),
        "sendTurn" => {
            let snapshot: Option<Thread> =
                serde_json::from_value(command["snapshot"].clone()).unwrap();
            let listed: Option<Thread> = serde_json::from_value(command["listed"].clone()).unwrap();
            let input = &command["input"];
            let mut items = Vec::new();
            if !text(input, "text").is_empty() {
                items.push(Input::Text {
                    text: text(input, "text"),
                    text_elements: &[],
                });
            }
            for attachment in input["attachments"].as_array().unwrap() {
                items.push(if attachment["isImage"] == true {
                    Input::LocalImage {
                        path: text(attachment, "path"),
                    }
                } else {
                    Input::Mention {
                        path: text(attachment, "path"),
                        name: text(attachment, "name"),
                    }
                });
            }
            let target = submission_target(snapshot.as_ref(), listed.as_ref(), None)?;
            let reply = client
                .submit(
                    &Submission {
                        thread_id: text(command, "threadId"),
                        client_user_message_id: text(input, "clientUserMessageId"),
                        input: &items,
                        model: optional(command, "model"),
                        effort: optional(command, "effort"),
                        service_tier: optional(command, "serviceTierForTurn"),
                    },
                    target,
                )
                .await?;
            json!(reply.value)
        }
        "interruptTurn" => {
            call!(Interrupt {
                thread_id: text(command, "threadId").to_owned(),
                turn_id: text(command, "turnId").to_owned()
            });
            Value::Null
        }
        "models" => json!(client.models().await?.data),
        "sessionImages" => json!(client.session_images(text(command, "threadId")).await?),
        "watchThread" => {
            call!(Watch {
                thread_id: text(command, "threadId").to_owned(),
                watch_key: command["watchKey"].as_u64().unwrap(),
                watch_id: command["watchId"].as_u64().unwrap(),
                path: optional(command, "path").map(str::to_owned)
            });
            Value::Null
        }
        "unwatchThread" => {
            call!(Unwatch {
                watch_key: command["watchKey"].as_u64().unwrap(),
                watch_id: command["watchId"].as_u64().unwrap()
            });
            Value::Null
        }
        "transcribe" => json!(
            client
                .call(&Transcribe {
                    audio: text(command, "audio")
                })
                .await?
                .value
                .text
        ),
        "respond" => {
            let request: ServerRequest =
                serde_json::from_value(command["request"].clone()).unwrap();
            let answer = &command["answer"];
            let answers: BTreeMap<String, String> = if answer["type"] == "answers" {
                serde_json::from_value(answer["answers"].clone()).unwrap()
            } else {
                BTreeMap::new()
            };
            let raw = if answer["type"] == "raw" {
                Some(
                    serde_json::from_str::<Value>(text(answer, "json"))
                        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?,
                )
            } else {
                None
            };
            let answer = match text(answer, "type") {
                "decision" => Answer::Decision {
                    index: answer["index"].as_u64().unwrap() as u32,
                },
                "permissions" => Answer::Permissions {
                    allow: answer["allow"].as_bool().unwrap(),
                },
                "answers" => Answer::Questions { answers },
                "raw" => Answer::Raw {
                    value: raw.unwrap(),
                },
                kind => panic!("unknown answer {kind}"),
            };
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
            path: text(command, "path"),
            revision: text(command, "revision"),
            text: text(command, "text")
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
            id: text(command, "accountId").to_owned()
        }),
        "startAccountLogin" => call!(StartAccountLogin {}),
        "accountLoginStatus" => call!(ReadAccountLogin {
            id: text(command, "loginId").to_owned()
        }),
        "cancelAccountLogin" => {
            call!(CancelAccountLogin {
                id: text(command, "loginId").to_owned()
            });
            Value::Null
        }
        "forkThread" => call!(ForkThread {
            thread_id: text(command, "threadId").to_owned(),
            last_turn_id: text(command, "lastTurnId").to_owned(),
            exclude_turns: true
        }),
        kind => panic!("unknown operation {kind}"),
    };
    Ok(result)
}

async fn run_case(case: &Value) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(client_io);
    let peer = Arc::new(
        RpcPeer::open(
            JsonlReader::new(read),
            write,
            Some(Duration::from_secs(1)),
            16,
        )
        .unwrap(),
    );
    let client = Client::new(peer.clone());
    let server = async {
        let (read, write) = tokio::io::split(server_io);
        let mut reader = JsonlReader::new(read);
        let mut writer = JsonlWriter::new(write);
        for exchange in case["exchanges"].as_array().unwrap() {
            let line = reader
                .read_line()
                .await
                .unwrap()
                .expect("expected exchange");
            let request: Value = serde_json::from_str(&line).unwrap();
            if let Some(reply) = exchange.get("reply") {
                assert_eq!(&request, reply, "{}", case["name"]);
            } else {
                assert_eq!(request["method"], exchange["method"], "{}", case["name"]);
                assert_eq!(request["params"], exchange["params"], "{}", case["name"]);
                let mut response = exchange["response"].clone();
                response["id"] = request["id"].clone();
                writer.write_line(&response.to_string()).await.unwrap();
            }
        }
        assert!(
            reader.read_line().await.unwrap().is_none(),
            "unexpected additional request: {}",
            case["name"]
        );
    };
    let operation = async {
        let result = execute(&client, &case["command"]).await;
        peer.close().await.unwrap();
        result
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(operation, server)
    })
    .await
    .expect("bounded operation");
    if let Some(expected) = case.get("result") {
        assert_eq!(result.unwrap(), *expected, "{}", case["name"]);
    } else {
        let error = result.expect_err("expected operation failure");
        let expected = match case["errorContains"].as_str().unwrap() {
            "カーソル" => "cursor",
            "モデル一覧の続きを取得できませんでした" => {
                "model cursor did not advance"
            }
            "音声を認識できませんでした" => "host/dictation/transcribe",
            "承認の選択肢が無効です" => "invalid approval choice",
            "すべての質問に回答してください" => "every question requires an answer",
            other => other,
        };
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
                case["errorRaw"],
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
    assert_eq!(cases.len(), 63);
    for case in cases {
        run_case(case).await;
    }
}

#[test]
fn submission_corpus() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/submission.json")).unwrap();
    assert_eq!(cases.len(), 9);
    for case in cases {
        let snapshot: Option<Thread> = serde_json::from_value(case["snapshot"].clone()).unwrap();
        let listed: Option<Thread> = serde_json::from_value(case["listed"].clone()).unwrap();
        let result = submission_target(snapshot.as_ref(), listed.as_ref(), None);
        let expected = &case["expected"];
        if expected["action"] == "reject" {
            assert!(result.is_err());
            continue;
        }
        let actual = match result.unwrap() {
            SubmissionTarget::Steer(turn_id) => json!({"action":"steer","turnId":turn_id}),
            SubmissionTarget::Queue => json!({"action":"queue"}),
            SubmissionTarget::Start { cwd, resume } => {
                json!({"action":"start","cwd":cwd,"resume":resume})
            }
        };
        assert_eq!(actual, *expected, "{}", case["name"]);
    }
}
