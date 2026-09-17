//! Real Store + iroh streams + the Host's production transfer reservations.
//! Only control responses are scripted, to place deltas and stalls precisely.
use super::WorkspaceFiles;
use agent_core::session::{SessionChange, TextField};
#[allow(dead_code)]
#[path = "../../../agent-core/tests/support/host.rs"]
mod host_fixture;
use agent_core::{
    client::Answer,
    state::{Intent, Snapshot, operations as op},
    store::Store,
    transport::{Endpoint, Identity, Relays, Trust},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc};

type Output = Arc<Mutex<host_fixture::Writer>>;
const OWNER: u64 = 1;
const BODY_SIZE: usize = 1024 * 1024;

struct Fixture {
    store: Store,
    host: Endpoint,
    client: Endpoint,
    session: host_fixture::Session,
    output: Output,
    requests: mpsc::Receiver<host_fixture::Request>,
    server: tokio::task::JoinHandle<()>,
    files: WorkspaceFiles,
    subscription: uuid::Uuid,
    _directory: tempfile::TempDir,
}

async fn wait_for(store: &Store, condition: impl Fn(&Snapshot) -> bool) {
    let mut updates = store.subscribe();
    loop {
        let snapshot = updates.borrow_and_update().clone();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        if condition(&snapshot) {
            return;
        }
        updates.changed().await.unwrap();
    }
}
fn item(id: usize, complete: bool) -> Value {
    if id.is_multiple_of(2) {
        json!({"id":format!("item-{id}"),"type":"agentMessage","text":if complete {format!("{id}:{}", "x".repeat(BODY_SIZE))} else {String::new()}})
    } else if complete {
        json!({"id":format!("item-{id}"),"type":"imageGeneration","result":format!("{id}:{}", "A".repeat(BODY_SIZE))})
    } else {
        json!({"id":format!("item-{id}"),"type":"imageGeneration"})
    }
}
impl Fixture {
    async fn start(count: usize, automatic_reads: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let files = WorkspaceFiles::new(directory.path().join("uploads"));
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let client = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let trust = Trust {
            allowed: [client.node_id()].into(),
            ..Default::default()
        };
        let incoming = async {
            let session = host
                .accept()
                .await
                .unwrap()
                .unwrap()
                .authorize(&trust)
                .unwrap();
            let (session, mut reader, writer) = host_fixture::accept(session).await;
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/session/scope");
            writer
                .reply(&request, json!({"result":"grant-fixture"}))
                .await
                .unwrap();
            (session, reader, writer)
        };
        let ticket = host.ticket();
        let (store, (session, mut reader, writer)) = tokio::join!(
            Store::connect(&client, &ticket, Snapshot::default(), None),
            incoming
        );
        let store = store.unwrap();
        let output: Output = Arc::new(Mutex::new(writer));
        let subscription = uuid::Uuid::new_v4();
        let (send_request, requests) = mpsc::channel(32);
        let server = tokio::spawn({
            let output = output.clone();
            let files = files.clone();
            async move {
                while let Some(request) = reader.read_request().await.unwrap() {
                    let result = match request["method"].as_str().unwrap() {
                        "host/thread/list" => {
                            json!({"data":[],"projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false})
                        }
                        "model/list" => json!({"data":[],"nextCursor":null}),
                        "host/session/open" => {
                            let items = if automatic_reads {
                                (0..count).map(|i| item(i, false)).collect::<Vec<_>>()
                            } else {
                                vec![
                                    json!({"id":"item-0","type":"commandExecution","aggregatedOutput":""}),
                                ]
                            };
                            json!({"session":request["params"]["session"],"subscriptionId":subscription,"response":{"thread":{"id":"A","turns":[{"id":"turn","status":"inProgress","deferredItemIds":(0..count).map(|i| format!("item-{i}")).collect::<Vec<_>>(),"items":items}]}}})
                        }
                        "host/thread/item/read" if automatic_reads => {
                            let id: usize = request["params"]["itemId"]
                                .as_str()
                                .unwrap()
                                .strip_prefix("item-")
                                .unwrap()
                                .parse()
                                .unwrap();
                            match files
                                .download_bytes(
                                    OWNER,
                                    agent_core::protocol::encode(
                                        serde_json::from_value::<agent_core::models::Item>(item(
                                            id, true,
                                        ))
                                        .unwrap(),
                                    )
                                    .unwrap(),
                                )
                                .await
                            {
                                Ok(grant) => json!({"item":item(id,false),"transfer":grant}),
                                Err(error) => {
                                    output.lock().await.reply(&request, json!({"error":{"code":-32000,"message":format!("{error:#}")}})).await.unwrap();
                                    continue;
                                }
                            }
                        }
                        _ => {
                            send_request.send(request).await.unwrap();
                            continue;
                        }
                    };
                    output
                        .lock()
                        .await
                        .reply(&request, json!({"result":result}))
                        .await
                        .unwrap();
                }
            }
        });
        Self {
            store,
            host,
            client,
            session,
            output,
            requests,
            server,
            files,
            subscription,
            _directory: directory,
        }
    }
    async fn open(&self) {
        self.store
            .dispatch(Intent::ReadThread(op::ReadThread::new("A".into())))
            .await
            .unwrap();
    }
    async fn update(&self, change: SessionChange) {
        self.output.lock().await.notify(json!({"method":"host/session/update","params":{"subscriptionId":self.subscription,"change":change}})).await.unwrap();
    }
    async fn close(self) {
        self.store.close().await.unwrap();
        self.server.abort();
        self.session.close();
        self.client.close().await;
        self.host.close().await;
    }
}

#[tokio::test]
async fn bulk_item_reads_hold_slots_through_body_application_without_starving_control() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut fixture = Fixture::start(12, true).await;
        fixture.open().await;
        // Hold four real streams before consuming their tokens. With the old
        // queue, controls 5..12 were issued first and exhausted the eight grants.
        let mut transfers = Vec::new();
        for _ in 0..4 {
            transfers.push(fixture.session.accept_stream().await.unwrap());
        }
        assert_eq!(fixture.files.grants.lock().unwrap().len(), 4);
        fixture.update(SessionChange::Text { turn_id:"turn".into(), item_id:"item-0".into(), field:TextField::Message, delta:"live update".into() }).await;
        fixture.update(SessionChange::Request { request: serde_json::from_value(json!({"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"A","turnId":"turn","itemId":"item-0"}})).unwrap() }).await;
        wait_for(&fixture.store, |s| s.requests.contains_key("\"approval\"") && s.conversations["A"].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].text.as_deref() == Some("live update")).await;
        for intent in [
            Intent::Respond(op::Respond {request_id:json!("approval"),answer:Answer::Decision {index:2}}),
            Intent::Interrupt(op::Interrupt {thread_id:"A".into(),turn_id:"turn".into()}),
        ] {
            let completion = fixture.store.dispatch(intent);
            let request = fixture.requests.recv().await.unwrap();
            assert!(matches!(request["method"].as_str(), Some("host/session/answer" | "turn/interrupt")));
            fixture.output.lock().await.reply(&request, json!({"result":{}})).await.unwrap();
            completion.await.unwrap();
        }
        assert_eq!(fixture.files.grants.lock().unwrap().len(), 4, "stalled bodies must retain their slots");
        // item-0 retries after its stale body is consumed. All 12 final bodies
        // must match completely, and every grant must have been consumed.
        for stream in transfers {
            fixture.files.transfer(OWNER, stream).await.unwrap();
        }
        let serving = async {
            loop {
                let stream = fixture.session.accept_stream().await.unwrap();
                fixture.files.transfer(OWNER, stream).await.unwrap();
            }
        };
        let complete = wait_for(&fixture.store, |s| s.conversations["A"].turns.as_ref().unwrap()[0].deferred_item_ids.as_ref().unwrap().is_empty());
        tokio::select! { _ = serving => unreachable!(), _ = complete => {} }
        let snapshot = fixture.store.snapshot();
        let items = snapshot.conversations["A"].turns.as_ref().unwrap()[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 12);
        for (id, actual) in items.iter().enumerate() {
            let expected: agent_core::models::Item = serde_json::from_value(item(id, true)).unwrap();
            assert_eq!(actual.as_ref(), &expected);
        }
        assert!(fixture.files.grants.lock().unwrap().is_empty());
        fixture.close().await;
    }).await.expect("bulk item reads stalled");
}

#[tokio::test]
async fn stale_control_responses_consume_grants_before_retrying_more_than_eight_times() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut fixture = Fixture::start(1, false).await;
        fixture.open().await;
        let completion = fixture.store.dispatch(Intent::ReadItem(op::ReadItem {thread_id:"A".into(),turn_id:"turn".into(),item_id:"item-0".into()}));
        for iteration in 0..=12 {
            let request = fixture.requests.recv().await.unwrap();
            assert_eq!(request["method"], "host/thread/item/read");
            let body = json!({"id":"item-0","type":"commandExecution","aggregatedOutput":format!("complete-{iteration}:{}", "x".repeat(BODY_SIZE))});
            let grant = fixture.files.download_bytes(OWNER, agent_core::protocol::encode(serde_json::from_value::<agent_core::models::Item>(body).unwrap()).unwrap()).await.unwrap();
            if iteration < 12 {
                // This is deliberately BEFORE the control response, not a
                // delta during the item body transfer. Wait for Store application as the
                // barrier so the source Arc is certainly stale on arrival.
                fixture.update(SessionChange::Text { turn_id:"turn".into(), item_id:"item-0".into(), field:TextField::CommandOutput, delta:"d".into() }).await;
                wait_for(&fixture.store, |s| s.conversations["A"].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].aggregated_output.as_deref() == Some("d".repeat(iteration+1).as_str())).await;
            }
            fixture.output.lock().await.reply(&request, json!({"result":{"item":{"id":"item-0","type":"commandExecution"},"transfer":grant}})).await.unwrap();
            let stream = fixture.session.accept_stream().await.unwrap();
            fixture.files.transfer(OWNER, stream).await.unwrap();
            assert!(fixture.files.grants.lock().unwrap().is_empty());
        }
        completion.await.unwrap();
        let snapshot = fixture.store.snapshot();
        let turn = &snapshot.conversations["A"].turns.as_ref().unwrap()[0];
        assert!(turn.deferred_item_ids.as_ref().unwrap().is_empty());
        assert_eq!(turn.items.as_ref().unwrap()[0].aggregated_output.as_deref(), Some(format!("complete-12:{}", "x".repeat(BODY_SIZE)).as_str()));
        fixture.close().await;
    }).await.expect("stale control response left an unconsumed grant");
}
