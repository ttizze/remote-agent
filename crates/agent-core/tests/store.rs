use agent_core::{
    client::Answer,
    models::Thread,
    peer::{PeerError, RpcPeer},
    state::{Draft, Intent, Snapshot},
    store::{Outcome, Store},
};
use host_protocol::{JsonlReader, JsonlWriter};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

fn setup(
    snapshot: Snapshot,
) -> (
    Arc<Store>,
    JsonlReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    JsonlWriter<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
) {
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(client);
    let peer = RpcPeer::open(JsonlReader::new(read), write, Duration::from_secs(1), 16).unwrap();
    let (read, write) = tokio::io::split(server);
    (
        Arc::new(Store::new(peer, snapshot)),
        JsonlReader::new(read),
        JsonlWriter::new(write),
    )
}
async fn read(reader: &mut JsonlReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>) -> Value {
    serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap()
}
async fn wait_for(store: &Store, predicate: impl Fn(&Snapshot) -> bool) {
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if predicate(&updates.borrow_and_update()) {
                break;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
fn thread(text: &str) -> Thread {
    serde_json::from_value(json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[{"id":"turn","status":"inProgress","items":[{"id":"item","type":"agentMessage","text":text}]}]})).unwrap()
}
fn snapshot() -> Snapshot {
    Snapshot {
        conversations: Arc::new(BTreeMap::from([("thread".into(), Arc::new(thread("old")))])),
        ..Default::default()
    }
}
fn loaded_text(snapshot: &Snapshot) -> Option<&str> {
    snapshot
        .conversations
        .get("thread")?
        .turns
        .as_ref()?
        .first()?
        .items
        .as_ref()?
        .first()?
        .text
        .as_deref()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_response_precedes_following_delta_even_when_server_closes() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" obsolete"}}).to_string()).await.unwrap();
        writer
            .write_line(&json!({"id":request["id"],"result":{"thread":thread("base")}}).to_string())
            .await
            .unwrap();
        writer.write_line(&json!({"method":"item/agentMessage/delta","params":{"threadId":"thread","turnId":"turn","itemId":"item","delta":" tail"}}).to_string()).await.unwrap();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        store.dispatch(Intent::ReadThread("thread".into())),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap(), Outcome::Applied);
    server.await.unwrap();
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("base tail")
    })
    .await;
}
#[tokio::test]
async fn approval_can_complete_while_another_request_is_waiting() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let pending = read(&mut reader).await;
        writer.write_line(&json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"thread"}}).to_string()).await.unwrap();
        let answer = read(&mut reader).await;
        assert_eq!(
            answer,
            json!({"id":"approval","result":{"decision":"decline"}})
        );
        writer
            .write_line(
                &json!({"id":pending["id"],"result":{"thread":thread("approved path")}})
                    .to_string(),
            )
            .await
            .unwrap();
        writer
    });
    let loading = tokio::spawn({
        let store = store.clone();
        async move { store.dispatch(Intent::ReadThread("thread".into())).await }
    });
    wait_for(&store, |snapshot| {
        snapshot.requests.contains_key("\"approval\"")
    })
    .await;
    store
        .dispatch(Intent::Respond {
            request_id: json!("approval"),
            answer: Answer::Decision(2),
        })
        .await
        .unwrap();
    assert_eq!(loading.await.unwrap().unwrap(), Outcome::Applied);
    let _writer = server.await.unwrap();
    assert!(store.snapshot().requests.is_empty());
    assert_eq!(loaded_text(&store.snapshot()), Some("approved path"));
}
#[tokio::test]
async fn invalid_typed_reply_does_not_block_later_wire_events() {
    let (store, mut reader, mut writer) = setup(snapshot());
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":{"cwd":"/fixture"}}}).to_string(),
            )
            .await
            .unwrap();
        let request = read(&mut reader).await;
        writer
            .write_line(
                &json!({"id":request["id"],"result":{"thread":thread("recovered")}}).to_string(),
            )
            .await
            .unwrap();
        writer
    });
    let error = store
        .dispatch(Intent::ReadThread("thread".into()))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PeerError::InvalidResponse {
            sequence: Some(_),
            ..
        }
    ));
    store
        .dispatch(Intent::ReadThread("thread".into()))
        .await
        .unwrap();
    let _writer = server.await.unwrap();
    assert_eq!(loaded_text(&store.snapshot()), Some("recovered"));
}
#[tokio::test]
async fn successful_submission_does_not_erase_a_newer_draft() {
    let mut initial = snapshot();
    Arc::make_mut(&mut initial.conversations).insert(
        "thread".into(),
        Arc::new(
            serde_json::from_value(
                json!({"id":"thread","cwd":"/fixture","status":{"type":"idle"},"turns":[]}),
            )
            .unwrap(),
        ),
    );
    let (store, mut reader, mut writer) = setup(initial);
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: Draft {
                text: "first".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let sending = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .dispatch(Intent::Submit {
                    thread_id: "thread".into(),
                    client_user_message_id: "fixture-message".into(),
                })
                .await
        }
    });
    let request = tokio::time::timeout(Duration::from_secs(2), read(&mut reader))
        .await
        .unwrap();
    assert_eq!(request["params"]["input"][0]["text"], "first");
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: Draft {
                text: "second".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    writer
        .write_line(&json!({"id":request["id"],"result":{"turn":{"id":"turn-new"}}}).to_string())
        .await
        .unwrap();
    assert_eq!(
        sending.await.unwrap().unwrap(),
        Outcome::Submitted(Some("turn-new".into()))
    );
    assert_eq!(store.snapshot().drafts["thread"].text, "second");
}

#[tokio::test]
async fn malformed_file_change_delta_does_not_poison_store() {
    let mut previous = snapshot();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut previous.conversations)
            .get_mut("thread")
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0])
        .items
        .as_mut()
        .unwrap()
        .push(Arc::new(
            serde_json::from_value(json!({"id":"file", "type":"fileChange", "changes":[7]}))
                .unwrap(),
        ));
    let (store, _reader, mut writer) = setup(previous);
    for (method, item) in [
        ("item/fileChange/outputDelta", "file"),
        ("item/agentMessage/delta", "item"),
    ] {
        writer
            .write_line(
                &json!({"method":method,"params":{
                    "threadId":"thread","turnId":"turn","itemId":item,"delta":" continues"
                }})
                .to_string(),
            )
            .await
            .unwrap();
    }
    wait_for(&store, |snapshot| {
        loaded_text(snapshot) == Some("old continues")
    })
    .await;
    assert_eq!(
        store.snapshot().conversations["thread"]
            .turns
            .as_ref()
            .unwrap()[0]
            .items
            .as_ref()
            .unwrap()[1]
            .extra["changes"],
        json!([7])
    );
    store
        .dispatch(Intent::SetDraft {
            thread_id: "thread".into(),
            draft: agent_core::state::Draft {
                text: "still usable".into(),
                ..Default::default()
            },
        })
        .await
        .unwrap();
    assert_eq!(store.snapshot().drafts["thread"].text, "still usable");
    store.close().await.unwrap();
}

#[tokio::test]
async fn read_older_through_store_prepends_turns_and_items_and_preserves_newer_content() {
    let previous: Thread = serde_json::from_value(json!({"id":"thread","historyCursor":"turn-page",
        "turns":[{"id":"new","status":"completed","items":[{"id":"new-item","type":"agentMessage","text":"new"}]}]})).unwrap();
    let (store, mut reader, mut writer) = setup(Snapshot {
        conversations: Arc::new(BTreeMap::from([("thread".into(), Arc::new(previous))])),
        ..Default::default()
    });
    let server = tokio::spawn(async move {
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/thread/turns/list");
        assert_eq!(request["params"]["cursor"], "turn-page");
        writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"thread","historyCursor":null,
            "turns":[{"id":"old","itemsHasMore":true,"itemsNextCursor":"item-page","items":[{"id":"last-old","text":"tail"}]}]}}}).to_string()).await.unwrap();
        let request = read(&mut reader).await;
        assert_eq!(request["method"], "host/thread/items/list");
        assert_eq!(request["params"]["turnId"], "old");
        assert_eq!(request["params"]["cursor"], "item-page");
        writer.write_line(&json!({"id":request["id"],"result":{"thread":{"id":"thread",
            "turns":[{"id":"old","itemsHasMore":false,"itemsNextCursor":null,"items":[{"id":"first-old","text":"head"}]}]}}}).to_string()).await.unwrap();
        writer
    });
    store
        .dispatch(Intent::ReadOlder {
            thread_id: "thread".into(),
            turn_id: None,
            cursor: Some("turn-page".into()),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::ReadOlder {
            thread_id: "thread".into(),
            turn_id: Some("old".into()),
            cursor: Some("item-page".into()),
        })
        .await
        .unwrap();
    let snapshot = store.snapshot();
    let thread = &snapshot.conversations["thread"];
    let turns = thread.turns.as_ref().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["old", "new"]
    );
    assert_eq!(
        turns[0]
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["first-old", "last-old"]
    );
    assert_eq!(
        turns[1].items.as_ref().unwrap()[0].text.as_deref(),
        Some("new")
    );
    assert_eq!(turns[0].items_has_more, Some(false));
    assert_eq!(thread.history_cursor, Some(None));
    let _writer = server.await.unwrap();
    store.close().await.unwrap();
}
