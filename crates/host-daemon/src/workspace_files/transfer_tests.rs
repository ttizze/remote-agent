//! Real Store + iroh streams + the Host's production transfer reservations.
//! Only control responses are scripted, to place deltas and stalls precisely.
use super::WorkspaceFiles;
use agent_protocol::session::SessionChange;
#[allow(dead_code)]
#[path = "../../../agent-core/tests/support/host.rs"]
mod host_fixture;
use agent_core::{
    state::{Intent, Snapshot, operations as op},
    store::Store,
};
use agent_protocol::requests::Answer;
use agent_transport::transport::{Endpoint, Identity, Relays, Trust};
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
    _directory: tempfile::TempDir,
}

async fn wait_for(store: &Store, condition: impl Fn(&Snapshot) -> bool) {
    let mut updates = store.subscribe();
    loop {
        let snapshot = updates.borrow_and_update().clone();
        assert!(
            snapshot.panes.primary().error.is_none(),
            "{:?}",
            snapshot.panes.primary().error
        );
        if condition(&snapshot) {
            return;
        }
        updates.changed().await.unwrap();
    }
}
fn item(id: usize, complete: bool) -> Value {
    let body = if id.is_multiple_of(2) {
        json!({"assistantText":{"text":if complete {format!("{id}:{}", "x".repeat(BODY_SIZE))} else {String::new()},"phase":"unknown"}})
    } else {
        json!({"imageGeneration":{"savedPath":null,"data":if complete {Some(format!("{id}:{}", "A".repeat(BODY_SIZE)))} else {None},"revisedPrompt":null}})
    };
    json!({"id":format!("item-{id}"),"status":"unknown","clientInputId":null,"body":if complete {json!({"inline":{"body":body}})} else {json!({"deferred":{"summary":body}})}})
}
fn command(output: &str) -> Value {
    json!({"id":"item-0","status":"running","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"pwd","cwd":null,"output":output,"exitCode":null}}}}})
}
fn deferred_command(output: &str) -> Value {
    let mut item = command(output);
    let body = item["body"]["inline"]["body"].take();
    item["body"] = json!({"deferred":{"summary":body}});
    item
}
fn command_output(item: &agent_protocol::items::Item) -> &str {
    let agent_protocol::items::ItemBody::CommandExecution { output, .. } = item.body() else {
        panic!("command body")
    };
    output
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
            assert_eq!(request["method"], "host/device/identify");
            writer
                .reply(
                    &request,
                    json!({"result":{"storageScope":"grant-fixture","ticket":""}}),
                )
                .await
                .unwrap();
            (session, reader, writer)
        };
        let ticket = host.ticket();
        let (store, (session, mut reader, writer)) = tokio::join!(
            Store::connect(
                &client,
                &ticket,
                Snapshot::default(),
                None,
                "Fixture device",
                None
            ),
            incoming
        );
        let store = store.unwrap();
        let output: Output = Arc::new(Mutex::new(writer));
        let (send_request, requests) = mpsc::channel(32);
        let server = tokio::spawn({
            let output = output.clone();
            let files = files.clone();
            async move {
                while let Some(request) = reader.read_request().await.unwrap() {
                    let result = match request["method"].as_str().unwrap() {
                        "host/list/subscribe" => {
                            json!({"data":[],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,})
                        }
                        "host/account/list" => json!({"accounts":[],"selected":{}}),
                        "host/model/list" => json!({"data":[],"nextCursor":null}),
                        "host/conversation/subscribe" => {
                            let items = if automatic_reads {
                                (0..count).map(|i| item(i, false)).collect::<Vec<_>>()
                            } else {
                                vec![deferred_command("")]
                            };
                            json!({"thread":{"id":{"provider":"codex","id":"A"},"turns":[{"id":"turn","status":"running","items":items}]}})
                        }
                        "host/conversation/hydrate" if automatic_reads => {
                            let id: usize = request["params"]["item"]
                                .as_str()
                                .unwrap()
                                .strip_prefix("item-")
                                .unwrap()
                                .parse()
                                .unwrap();
                            match files
                                .download_bytes(
                                    OWNER,
                                    agent_protocol::protocol::encode(
                                        serde_json::from_value::<agent_protocol::models::Item>(
                                            item(id, true),
                                        )
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
            _directory: directory,
        }
    }
    async fn open(&self) {
        self.store
            .dispatch(Intent::ReadThread(op::ReadThread::new(
                agent_protocol::session::SessionRef {
                    provider: Some(agent_protocol::session::ProviderKind::Codex),
                    id: "A".into(),
                },
            )))
            .await
            .unwrap();
    }
    async fn update(&self, change: SessionChange) {
        self.output.lock().await.notify(json!({"method":"fixture/session/change","session":{"provider":"codex","id":"A"},"change":change})).await.unwrap();
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
async fn attachments_outlive_worktrees_while_linked_images_fail_only_their_download() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut fixture = Fixture::start(0, false).await;
        fixture.open().await;
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("screenshot.png");
        let bytes = b"captured image";
        std::fs::write(&source, bytes).unwrap();
        let draft_key = fixture
            .store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone();
        let upload = fixture
            .store
            .dispatch(Intent::UploadAttachment(op::UploadAttachment {
                draft_key: draft_key.clone(),
                attachment: agent_core::state::Attachment {
                    path: source.to_str().unwrap().into(),
                    name: "screenshot.png".into(),
                    is_image: true,
                },
            }));
        let request = fixture.requests.recv().await.unwrap();
        assert_eq!(request["method"], "host/blob/upload");
        let grant = fixture
            .files
            .dispatch(
                OWNER,
                agent_protocol::protocol::Call::Upload(
                    serde_json::from_value(request["params"].clone()).unwrap(),
                ),
            )
            .unwrap();
        // A transfer completion must append to the current draft, preserving
        // edits made while its bytes were in flight.
        fixture
            .store
            .dispatch(Intent::SetDraftText {
                thread_id: draft_key.clone(),
                text: "edited during upload".into(),
            })
            .await
            .unwrap();
        fixture
            .output
            .lock()
            .await
            .reply(&request, json!({"result":grant}))
            .await
            .unwrap();
        fixture
            .files
            .transfer(OWNER, fixture.session.accept_stream().await.unwrap())
            .await
            .unwrap();
        upload.await.unwrap();
        let attached = fixture.store.snapshot().input.drafts[&draft_key].attachments[0].clone();
        assert!(
            std::path::Path::new(&attached.path)
                .starts_with(dunce::canonicalize(fixture.files.upload_directory.as_ref()).unwrap())
        );
        workspace.close().unwrap();
        let before = fixture.store.snapshot();
        let destination = fixture._directory.path().join("download.png");
        let download = fixture
            .store
            .dispatch(Intent::DownloadFile(op::DownloadFile {
                source: attached.path.clone(),
                destination: destination.to_str().unwrap().into(),
            }));
        let request = fixture.requests.recv().await.unwrap();
        let grant = fixture
            .files
            .dispatch(
                OWNER,
                agent_protocol::protocol::Call::Download(
                    serde_json::from_value(request["params"].clone()).unwrap(),
                ),
            )
            .unwrap();
        fixture
            .output
            .lock()
            .await
            .reply(&request, json!({"result":grant}))
            .await
            .unwrap();
        fixture
            .files
            .transfer(OWNER, fixture.session.accept_stream().await.unwrap())
            .await
            .unwrap();
        download.await.unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        assert_eq!(before.input.drafts[&draft_key].text, "edited during upload");
        assert!(!source.exists());
        let download = fixture
            .store
            .dispatch(Intent::DownloadFile(op::DownloadFile {
                source: source.to_str().unwrap().into(),
                destination: destination.to_str().unwrap().into(),
            }));
        let request = fixture.requests.recv().await.unwrap();
        let failure = fixture
            .files
            .dispatch(
                OWNER,
                agent_protocol::protocol::Call::Download(
                    serde_json::from_value(request["params"].clone()).unwrap(),
                ),
            )
            .unwrap_err();
        assert_eq!(failure.code, "file_not_found");
        fixture
            .output
            .lock()
            .await
            .reply(&request, json!({"error":failure}))
            .await
            .unwrap();
        let error = download.await.unwrap_err();
        assert_eq!(
            agent_core::presentation::error::error_text(
                agent_core::localization::UiLanguage::Japanese,
                &error.to_string()
            )
            .message,
            "ファイルまたはフォルダが見つかりません。"
        );
        let after = fixture.store.snapshot();
        assert!(after.panes.primary().error.is_none());
        assert!(after.connection.connected);
        assert_eq!(after.sessions.conversations, before.sessions.conversations);
        assert_eq!(after.input.drafts, before.input.drafts);
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            bytes,
            "a failed linked image read must preserve the downloaded attachment"
        );
        fixture.close().await;
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn uploading_a_named_pipe_fails_without_waiting_or_reserving_host_storage() {
    use std::os::unix::fs::OpenOptionsExt;
    let mut fixture = Fixture::start(0, false).await;
    let path = fixture._directory.path().join("pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let mut upload = Box::pin(fixture.store.dispatch(Intent::UploadAttachment(
        op::UploadAttachment {
            draft_key: "draft".into(),
            attachment: agent_core::state::Attachment {
                path: path.to_str().unwrap().into(),
                name: "pipe".into(),
                is_image: false,
            },
        },
    )));
    let rejected = tokio::time::timeout(Duration::from_secs(1), &mut upload).await;
    if rejected.is_err() {
        drop(
            std::fs::File::options()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
                .unwrap(),
        );
        let _ = upload.await;
    }
    assert!(
        fixture.requests.try_recv().is_err(),
        "a non-file needs no transfer reservation"
    );
    assert!(
        fixture
            .store
            .snapshot()
            .input
            .drafts
            .values()
            .all(|draft| draft.attachments.is_empty())
    );
    fixture.close().await;
    assert!(
        rejected
            .expect("uploading a named pipe blocked on local I/O")
            .is_err()
    );
}

#[tokio::test]
async fn file_operations_preserve_remote_reasons_and_download_can_retry_after_failure() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut fixture = Fixture::start(0, false).await;
        let local = tempfile::tempdir().unwrap();
        let source = local.path().join("source.txt");
        std::fs::write(&source, b"content").unwrap();
        let destination = local.path().join("download.txt");
        let download = || Intent::DownloadFile(op::DownloadFile {
            source: source.to_str().unwrap().into(),
            destination: destination.to_str().unwrap().into(),
        });
        for (intent, method) in [
            (download(), "host/blob/download"),
            (Intent::UploadAttachment(op::UploadAttachment {
                draft_key: Default::default(),
                attachment: agent_core::state::Attachment {
                    path: source.to_str().unwrap().into(),
                    name: "source.txt".into(), is_image: false,
                },
            }), "host/blob/upload"),
            (Intent::ResolveFile(op::ResolveFile { path: "~/missing".into() }), "host/file/resolve"),
            (Intent::ListFiles(op::ListFiles { path: "~/missing".into() }), "host/file/list"),
        ] {
            let completion = fixture.store.dispatch(intent);
            let request = fixture.requests.recv().await.unwrap();
            assert_eq!(request["method"], method);
            fixture.output.lock().await.reply(&request, json!({"error":{
                "code":"file_operation_failed", "message":"Host supplied reason", "delivery":"notSent"
            }})).await.unwrap();
            let error = completion.await.unwrap_err();
            assert!(matches!(&error, agent_protocol::error::PeerError::Remote {
                delivery: agent_protocol::error::Delivery::NotSent, ..
            }));
            assert_eq!(agent_core::presentation::error::error_text(agent_core::localization::UiLanguage::Japanese, &error.to_string()).details.as_deref(), Some("Host supplied reason"));
        }
        assert!(!destination.exists());
        let completion = fixture.store.dispatch(download());
        let request = fixture.requests.recv().await.unwrap();
        assert_eq!(request["method"], "host/blob/download");
        let agent_protocol::protocol::Body::Grant(grant) = fixture.files.dispatch(OWNER, agent_protocol::protocol::Call::Download(
            agent_protocol::operations::ListFiles { path: source.to_str().unwrap().into() }
        )).unwrap() else { panic!("expected download grant") };
        fixture.output.lock().await.reply(&request, json!({"result":grant})).await.unwrap();
        fixture.files.transfer(OWNER, fixture.session.accept_stream().await.unwrap()).await.unwrap();
        completion.await.unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"content");
        fixture.close().await;
    }).await.unwrap();
}

#[tokio::test]
async fn concurrent_directory_requests_each_receive_their_own_result() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut fixture = Fixture::start(0, false).await;
        let first = fixture.store.dispatch(Intent::ListFiles(op::ListFiles {
            path: "/first".into(),
        }));
        let second = fixture.store.dispatch(Intent::ListFiles(op::ListFiles {
            path: "/second".into(),
        }));
        let a = fixture.requests.recv().await.unwrap();
        let b = fixture.requests.recv().await.unwrap();
        for request in [&b, &a] {
            assert_eq!(request["method"], "host/file/list");
            fixture
                .output
                .lock()
                .await
                .reply(
                    request,
                    json!({"result":{
                        "path":request["params"]["path"], "entries":[], "truncated":false
                    }}),
                )
                .await
                .unwrap();
        }
        for (completion, path) in [(first, "/first"), (second, "/second")] {
            let agent_core::store::Outcome::Directory { listing } = completion.await.unwrap()
            else {
                panic!("expected directory result")
            };
            assert_eq!(listing.path, path);
        }
        fixture.close().await;
    })
    .await
    .unwrap();
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
        let mut changed: agent_protocol::items::Item = serde_json::from_value(item(0, false)).unwrap();
        if let agent_protocol::items::ItemBody::AssistantText {text, ..} = changed.body_mut() {*text = "live update".into();}
        fixture.update(SessionChange::Item {turn_id:"turn".into(), item: changed.into()}).await;
        fixture.update(SessionChange::Request { request: serde_json::from_value(json!({"id": "approval", "target": {"turn": {"turnId": "turn", "itemId": "item-0"}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "effect": "allow"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "effect": "allowLasting"}, {"id": "choice-2", "label": "拒否", "description": "", "effect": "deny"}]}}})).unwrap() }).await;
        wait_for(&fixture.store, |s| s.request("approval").is_some() && matches!(s.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::AssistantText {text, ..} if text == "live update")).await;
        for intent in [
            Intent::Respond(op::Respond {request_id: "approval".into(),answer:Answer::Approval {choice_id: "choice-2".into()}}),
            Intent::Interrupt(op::Interrupt {thread_id: agent_protocol::session::SessionRef {provider:Some(agent_protocol::session::ProviderKind::Codex),id:"A".into()},turn_id:"turn".into()}),
        ] {
            let completion = fixture.store.dispatch(intent);
            let request = fixture.requests.recv().await.unwrap();
            assert!(matches!(request["method"].as_str(), Some("host/conversation/change")));
            fixture.output.lock().await.reply(&request, json!({"result":{"outcome":"Accepted","cursor":{"store":"00000000-0000-0000-0000-000000000001","sequence":8}}})).await.unwrap();
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
        let complete = wait_for(&fixture.store, |s| s.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().all(|item| !item.is_deferred()));
        tokio::select! { _ = serving => unreachable!(), _ = complete => {} }
        let snapshot = fixture.store.snapshot();
        let items = snapshot.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "A".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap();
        assert_eq!(items.len(), 12);
        for (id, actual) in items.iter().enumerate() {
            let expected: agent_protocol::models::Item = serde_json::from_value(item(id, true)).unwrap();
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
        let completion = fixture.store.dispatch(Intent::ReadItem(op::ReadItem {
            thread_id: agent_protocol::session::SessionRef {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                id: "A".into(),
            },
            turn_id: "turn".into(),
            item_id: "item-0".into(),
        }));
        for iteration in 0..=12 {
            let request = fixture.requests.recv().await.unwrap();
            assert_eq!(request["method"], "host/conversation/hydrate");
            let target = serde_json::from_value(request["params"]["session"].clone()).unwrap();
            let replica = fixture.output.lock().await.replica(&target);
            let version = replica.rows()[&agent_protocol::sync::Key::Item {
                thread: target,
                id: "item-0".into(),
            }]
                .version;
            let cursor = replica.cursor().unwrap();
            let body = command(&format!("complete-{iteration}:{}", "x".repeat(BODY_SIZE)));
            let grant = fixture
                .files
                .download_bytes(
                    OWNER,
                    agent_protocol::protocol::encode(
                        serde_json::from_value::<agent_protocol::models::Item>(body).unwrap(),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            if iteration < 12 {
                // This is deliberately BEFORE the control response, not a
                // delta during the item body transfer. Wait for Store application as the
                // barrier so the source Arc is certainly stale on arrival.
                fixture
                    .update(SessionChange::Item {
                        turn_id: "turn".into(),
                        item: serde_json::from_value(deferred_command(&"d".repeat(iteration + 1)))
                            .unwrap(),
                    })
                    .await;
                wait_for(&fixture.store, |s| {
                    command_output(
                        &s.sessions.conversations[&agent_protocol::session::SessionRef {
                            provider: Some(agent_protocol::session::ProviderKind::Codex),
                            id: "A".into(),
                        }]
                            .turns
                            .as_ref()
                            .unwrap()[0]
                            .items
                            .as_ref()
                            .unwrap()[0],
                    ) == "d".repeat(iteration + 1)
                })
                .await;
            }
            fixture
                .output
                .lock()
                .await
                .reply(
                    &request,
                    json!({"result":{"cursor":cursor,"version":version,"transfer":grant}}),
                )
                .await
                .unwrap();
            let stream = fixture.session.accept_stream().await.unwrap();
            fixture.files.transfer(OWNER, stream).await.unwrap();
            assert!(fixture.files.grants.lock().unwrap().is_empty());
        }
        completion.await.unwrap();
        let snapshot = fixture.store.snapshot();
        let turn = &snapshot.sessions.conversations[&agent_protocol::session::SessionRef {
            provider: Some(agent_protocol::session::ProviderKind::Codex),
            id: "A".into(),
        }]
            .turns
            .as_ref()
            .unwrap()[0];
        assert!(
            turn.items
                .as_ref()
                .unwrap()
                .iter()
                .all(|item| !item.is_deferred())
        );
        assert_eq!(
            command_output(&turn.items.as_ref().unwrap()[0]),
            format!("complete-12:{}", "x".repeat(BODY_SIZE))
        );
        fixture.close().await;
    })
    .await
    .expect("stale control response left an unconsumed grant");
}
