use agent_core::{client::ClientExt, state::operations as op};
use agent_protocol::models;
use agent_protocol::{models::Invitation, operations as rpc};
use agent_transport::{
    client::Client,
    transport::{Endpoint, Identity, Relays, Ticket},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{HostCredentials, HostRpcService, ProjectStore};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

mod codex_fixture;
use host_fixture::test_support::{HostFixture, Memory};

fn next_submission_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!(
        "fixture-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
async fn start_host(directory: &Path) -> HostFixture {
    HostFixture::start(
        directory,
        codex_fixture::config(directory),
        Arc::new(Memory::default()),
        "isolated Host",
        false,
        None,
    )
    .await
    .unwrap()
}
async fn open_session(
    peer: &Client,
    id: &Value,
    limit: usize,
) -> (Value, agent_transport::framing::Reader) {
    let (opened, updates) = peer
        .request_stream::<agent_protocol::session::OpenedSession>(
            &agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                session: serde_json::from_value(id.clone()).unwrap(),
                limit,
            }),
        )
        .await
        .unwrap();
    (serde_json::to_value(opened).unwrap(), updates)
}
async fn next_change(events: &mut agent_transport::framing::Reader, kind: &str) -> Value {
    loop {
        let change: agent_protocol::session::SessionChange =
            events.read().await.unwrap().expect("subscription closed");
        let value = serde_json::to_value(change).unwrap();
        let (name, fields) = value.as_object().unwrap().iter().next().unwrap();
        if name == kind {
            let mut fields = fields.clone();
            fields["type"] = name.clone().into();
            return fields;
        }
    }
}
async fn completed_turn(events: &mut agent_transport::framing::Reader) -> Value {
    loop {
        let change = next_change(events, "turn").await;
        if change["completed"] == true {
            return change["turn"].clone();
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_is_atomic_local_management_is_private_and_revocation_closes_active_sessions() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let stranger = fixture.connect(Identity::generate()).await.unwrap();
        assert!(
            stranger
                .peer
                .request::<models::ThreadResponse>(&agent_protocol::protocol::Call::CreateSession(
                    serde_json::from_value::<op::CreateSession>(
                        json!({"provider":"codex","cwd":directory.path()}),
                    )
                    .unwrap()
                ))
                .await
                .is_err()
        );
        stranger.endpoint.close().await;
        assert_eq!(
            local
                .peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["data"],
            json!([])
        );
        let invitation: Invitation = local.peer.call(&op::CreateInvitation {}).await.unwrap();
        assert_eq!(invitation.host_name, "isolated Host");
        assert_eq!(invitation.ai_recipients, ["OpenAI"]);
        assert_eq!(
            invitation.transcription_recipient.as_deref(),
            Some("OpenAI")
        );
        let key = Identity::generate().to_bytes();
        let trust_path = directory.path().join("state/trust.json");
        let backup = directory.path().join("state/trust-backup.json");
        std::fs::rename(&trust_path, &backup).unwrap();
        std::fs::create_dir(&trust_path).unwrap();
        let failed = fixture.connect(Identity::from_bytes(key)).await.unwrap();
        assert!(
            failed
                .peer
                .call(
                    &serde_json::from_value::<rpc::Pair>(
                        json!({"invitation":invitation.invitation}),
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        failed.endpoint.close().await;
        std::fs::remove_dir(&trust_path).unwrap();
        std::fs::rename(&backup, &trust_path).unwrap();
        let paired = fixture.connect(Identity::from_bytes(key)).await.unwrap();
        paired
            .peer
            .call(
                &serde_json::from_value::<rpc::Pair>(json!({"invitation":invitation.invitation}))
                    .unwrap(),
            )
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        assert!(paired.peer.call(&op::CreateInvitation {}).await.is_err());
        assert!(paired.peer.call(&rpc::ListRemoteHosts {}).await.is_err());
        assert!(
            paired
                .peer
                .call(
                    &serde_json::from_value::<op::RevokeDevice>(
                        json!({"nodeId":local.endpoint.node_id()}),
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        let reused = fixture.connect(Identity::generate()).await.unwrap();
        assert!(
            reused
                .peer
                .call(
                    &serde_json::from_value::<rpc::Pair>(
                        json!({"invitation":invitation.invitation}),
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        reused.endpoint.close().await;
        let status = local
            .peer
            .call(&rpc::ReadHostStatus {})
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        assert!(
            status["devices"]
                .as_array()
                .unwrap()
                .contains(&json!(paired.endpoint.node_id()))
        );
        let restored =
            HostCredentials::load(fixture.memory.clone(), directory.path().join("state"))
                .await
                .unwrap();
        assert_eq!(
            restored.local_identity().await.node_id(),
            local.endpoint.node_id()
        );
        let manager = agent_core::store::Store::connect(
            &local.endpoint,
            &fixture.ticket,
            Default::default(),
            None,
        )
        .await
        .unwrap();
        manager
            .dispatch(agent_core::state::Intent::LoadHostManagement(
                op::LoadHostManagement {},
            ))
            .await
            .unwrap();
        let device = paired.endpoint.node_id().to_string();
        assert!(
            manager
                .snapshot()
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .contains(&device)
        );
        manager
            .dispatch(agent_core::state::Intent::RevokeDevice(op::RevokeDevice {
                id: device.clone(),
            }))
            .await
            .unwrap();
        assert!(
            !manager
                .snapshot()
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .contains(&device)
        );
        manager
            .dispatch(agent_core::state::Intent::LoadHostManagement(
                op::LoadHostManagement {},
            ))
            .await
            .unwrap();
        assert!(
            !manager
                .snapshot()
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .contains(&device)
        );
        assert!(
            paired
                .peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .is_err()
        );
        paired.endpoint.close().await;
        let revoked = fixture.connect(Identity::from_bytes(key)).await.unwrap();
        assert!(
            revoked
                .peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .is_err()
        );
        revoked.endpoint.close().await;
        manager.close().await.unwrap();
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("pairing contract deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_consumers_cannot_both_use_one_invitation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let invitation: Invitation = local.peer.call(&op::CreateInvitation {}).await.unwrap();
        let first = fixture.connect(Identity::generate()).await.unwrap();
        let second = fixture.connect(Identity::generate()).await.unwrap();
        let params = json!({"invitation":invitation.invitation});
        let (a, b) = tokio::join!(
            (async {
                first
                    .peer
                    .call(&serde_json::from_value::<rpc::Pair>((params).clone()).unwrap())
                    .await
                    .map(|output| serde_json::to_value(output).unwrap())
            }),
            (async {
                second
                    .peer
                    .call(&serde_json::from_value::<rpc::Pair>((params).clone()).unwrap())
                    .await
                    .map(|output| serde_json::to_value(output).unwrap())
            })
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let status = local
            .peer
            .call(&rpc::ReadHostStatus {})
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        assert_eq!(status["devices"].as_array().unwrap().len(), 2);
        first.endpoint.close().await;
        second.endpoint.close().await;
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("concurrent pairing deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_clients_share_one_request_and_only_one_valid_answer_wins() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let first = fixture.local().await.unwrap();
        let second = fixture.local().await.unwrap();


        let started = first.peer.request::<models::ThreadResponse>(&agent_protocol::protocol::Call::CreateSession(serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path()})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let id = &started["thread"]["id"];
        let (_, mut first_events) = open_session(&first.peer, id, 5).await;
        let (_, mut second_events) = open_session(&second.peer, id, 5).await;
        first.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[approval]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let a = next_change(&mut first_events, "request").await["request"].clone();
        let b = next_change(&mut second_events, "request").await["request"].clone();
        assert_eq!(
            a["id"], b["id"],
            "all devices share one native execution request"
        );
        assert!(
            first
                .peer.request::<models::Empty>(&agent_protocol::protocol::Call::AnswerSession(serde_json::from_value::<rpc::SessionAnswer>(json!({"requestId":a["id"],"answer":{"approval":{"choiceId":"invalid"}}})).unwrap())).await
                .is_err()
        );
        let answer = json!({"requestId":a["id"],"answer":{"approval":{"choiceId":a["body"]["approval"]["choices"][0]["id"]}}});
        let (one, two) = tokio::join!(
            (async { first
                .peer.request::<models::Empty>(&agent_protocol::protocol::Call::AnswerSession(serde_json::from_value::<rpc::SessionAnswer>((answer).clone()).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()) }),
            (async { second
                .peer.request::<models::Empty>(&agent_protocol::protocol::Call::AnswerSession(serde_json::from_value::<rpc::SessionAnswer>((answer).clone()).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()) })
        );
        assert_ne!(one.is_ok(), two.is_ok(), "one validated answer must win");
        for events in [&mut first_events, &mut second_events] {
            let resolved = next_change(events, "resolveRequest").await;
            assert_eq!(resolved["requestId"], a["id"]);
            assert_eq!(completed_turn(events).await["status"], "completed");
        }
        first.close().await;
        second.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("simultaneous approval deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binary_transfers_use_the_issuing_iroh_session_and_preserve_bytes() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let client = fixture.local().await.unwrap();
        let content: Vec<u8> = (0..65537).map(|n| (n % 251) as u8).collect();
        let source = directory.path().join("source.bin");
        std::fs::write(&source, &content).unwrap();
        use agent_core::state::Attachment;
        use agent_core::state::Intent;
        use agent_core::store::Store;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None)
            .await
            .unwrap();
        store
            .dispatch(Intent::UploadAttachment(op::UploadAttachment {
                draft_key: "transfer-draft".into(),
                attachment: Attachment {
                    path: source.to_str().unwrap().into(),
                    name: "upload.bin".into(),
                    is_image: false,
                },
                directory: directory.path().to_str().unwrap().into(),
            }))
            .await
            .unwrap();
        let uploaded = PathBuf::from(
            &store.snapshot().drafts[&agent_core::state::DraftKey::from("transfer-draft")]
                .attachments[0]
                .path,
        );
        let other = fixture.local().await.unwrap();
        other
            .peer
            .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                rpc::ListSessions::new(Default::default()),
            ))
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        let denied = directory.path().join("denied.bin");
        assert!(
            agent_transport::transfers::download_file(
                &client.peer,
                || async {
                    other
                        .session
                        .open_stream()
                        .await
                        .map_err(std::io::Error::other)
                },
                &source,
                &denied,
            )
            .await
            .is_err()
        );
        assert!(
            !denied.exists(),
            "a different session must not receive file bytes"
        );
        other.close().await;
        let destination = directory.path().join("download.bin");
        store
            .dispatch(Intent::DownloadFile(op::DownloadFile {
                source: uploaded.to_str().unwrap().into(),
                destination: destination.to_str().unwrap().into(),
            }))
            .await
            .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), content);
        // A new chat has no project or working directory yet. Its attachment
        // must still reach Host storage, without changing the selected draft.
        assert!(store.snapshot().navigation.cwd.is_empty());
        store
            .dispatch(Intent::UploadAttachment(op::UploadAttachment {
                draft_key: "unscoped-draft".into(),
                attachment: Attachment {
                    path: source.to_str().unwrap().into(),
                    name: "unscoped.bin".into(),
                    is_image: false,
                },
                directory: store.snapshot().navigation.cwd.clone(),
            }))
            .await
            .unwrap();
        let uploaded = PathBuf::from(
            &store.snapshot().drafts[&agent_core::state::DraftKey::from("unscoped-draft")]
                .attachments[0]
                .path,
        );
        assert!(
            uploaded.starts_with(
                dunce::canonicalize(directory.path())
                    .unwrap()
                    .join("bex-attachments")
            )
        );
        let destination = directory.path().join("unscoped-download.bin");
        store
            .dispatch(Intent::DownloadFile(op::DownloadFile {
                source: uploaded.to_str().unwrap().into(),
                destination: destination.to_str().unwrap().into(),
            }))
            .await
            .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), content);
        assert!(store.snapshot().connected);
        store
            .dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                cwd: directory.path().to_str().unwrap().into(),
            }))
            .await
            .unwrap();
        let snapshot = store.snapshot();
        assert!(snapshot.workspace.review.as_ref().unwrap().files.is_empty());
        assert!(snapshot.error.is_none());
        let retained =
            store.snapshot().drafts[&agent_core::state::DraftKey::from("transfer-draft")].clone();
        let retry = op::UploadAttachment {
            draft_key: "transfer-draft".into(),
            attachment: Attachment {
                path: directory
                    .path()
                    .join("missing.bin")
                    .to_str()
                    .unwrap()
                    .into(),
                name: "missing.bin".into(),
                is_image: false,
            },
            directory: directory.path().to_str().unwrap().into(),
        };
        assert!(
            store
                .dispatch(Intent::UploadAttachment(retry.clone()))
                .await
                .is_err()
        );
        assert_eq!(
            store.snapshot().drafts[&agent_core::state::DraftKey::from("transfer-draft")],
            retained
        );
        assert!(store.snapshot().error.is_some());
        std::fs::write(&retry.attachment.path, &content).unwrap();
        store
            .dispatch(Intent::UploadAttachment(retry))
            .await
            .unwrap();
        let recovered = store.snapshot();
        let attachments =
            &recovered.drafts[&agent_core::state::DraftKey::from("transfer-draft")].attachments;
        assert_eq!(attachments.len(), retained.attachments.len() + 1);
        assert_eq!(attachments[0], retained.attachments[0]);
        assert_eq!(std::fs::read(&attachments[1].path).unwrap(), content);
        assert!(
            recovered.error.is_none(),
            "successful retry retained the upload error"
        );

        store.close().await.unwrap();
        assert!(!store.snapshot().connected);
        client.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("binary transfer deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnecting_during_session_cleanup_keeps_host_requests_available() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let mut session = endpoint.connect(&fixture.ticket).await.unwrap();
        let (mut peer, mut _events) = session
            .open_peer(Duration::from_secs(10), 16)
            .await
            .unwrap();
        for attempt in 0..128 {
            assert_eq!(
                peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["data"],
                json!([])
            );
            let close = async {
                tokio::task::yield_now().await;
                peer.close().await;
                session.close();
            };
            let (_, opened) = tokio::join!(close, endpoint.connect(&fixture.ticket));
            session = opened.unwrap_or_else(|error| panic!("reconnect {attempt}: {error}"));
            (peer, _events) = session
                .open_peer(Duration::from_secs(10), 16)
                .await
                .unwrap();
        }
        assert_eq!(
            peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                rpc::ListSessions::new(Default::default())
            ))
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap()["data"],
            json!([])
        );
        peer.close().await;
        session.close();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("reconnection cleanup deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_live_conversation_avoids_unmaterialized_history_and_survives_reconnect() {
    use agent_core::state::Intent;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None)
            .await
            .unwrap();
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let prompt = "[delayed-input] Preserve this input through reconnect";
        store
            .dispatch(Intent::SetDraftText {
                thread_id: store.snapshot().navigation.draft_key.clone(),
                text: prompt.into(),
            })
            .await
            .unwrap();
        store.disconnect().await.unwrap();
        assert!(
            store
                .dispatch(Intent::Submit {
                    thread_id: None,
                    client_user_message_id: "offline".into()
                })
                .await
                .is_err()
        );
        assert_eq!(
            store.snapshot().drafts[&store.snapshot().navigation.draft_key].text,
            prompt
        );
        let restored =
            serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap()).unwrap();
        store.close().await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, restored, None)
            .await
            .unwrap();
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "sent".into(),
            })
            .await
            .unwrap();
        let id = store.snapshot().navigation.thread_id.clone().unwrap();
        assert!(
            store.snapshot().subscriptions.contains_key(&id),
            "input must have an active subscription before it reaches the provider"
        );
        let client = fixture.local().await.unwrap();
        let current = open_session(&client.peer, &json!(id), 5).await.0;
        assert_eq!(current["response"]["thread"]["id"], json!(id));
        assert_eq!(
            current["response"]["thread"]["historyReadState"]["type"], "complete",
            "a natively unmaterialized history is empty, not a failed history read"
        );
        assert!(
            current["response"]["thread"]["turns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|turn| turn["status"] == "running"),
            "live state must be available before native history materializes"
        );
        store.disconnect().await.unwrap();
        let restored =
            serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap()).unwrap();
        store.close().await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, restored, None)
            .await
            .unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        assert!(store.snapshot().error.is_none());
        assert!(store.snapshot().subscriptions.contains_key(&id));
        assert!(
            store.snapshot().conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .iter()
                .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Running),
            "reconnecting before the first message is persisted must preserve the active turn"
        );
        std::fs::write(directory.path().join("release-inputs"), "").unwrap();
        let mut updates = store.subscribe();
        loop {
            let current = updates.borrow_and_update().clone();
            assert!(current.error.is_none(), "{:?}", current.error);
            if current.conversations[&id]
                .turns
                .as_ref()
                .is_some_and(|turns| {
                    turns
                        .iter()
                        .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
                })
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        store.dispatch(Intent::ShowThreadList).await.unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        let current = store.snapshot();
        assert!(current.error.is_none());
        assert!(current.pending_submissions.is_empty());
        assert!(
            current.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
                && current.drafts[&agent_core::state::DraftKey::from(&id)]
                    .attachments
                    .is_empty()
        );
        let turn = &current.conversations[&id].turns.as_ref().unwrap()[0];
        assert_eq!(turn.status.label(), "completed");
        let items = turn.items.as_ref().unwrap();
        assert!(items.iter().any(|item| item_text(item) == Some(prompt)));
        assert!(items.iter().any(|item| matches!(
            item.body(),
            agent_protocol::items::ItemBody::AssistantText { .. }
        ) && matches!(
            item.body(),
            agent_protocol::items::ItemBody::AssistantText {
                phase: agent_protocol::items::AssistantPhase::Final,
                ..
            }
        )));
        store.close().await.unwrap();
        client.peer.close().await;
        drop(client);
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("live conversation recovery exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submissions_complete_across_saved_worktree_settings_and_chat_scopes() {
    use agent_core::state::Attachment;
    use agent_core::state::Intent;
    use agent_core::store::Outcome;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(60), async {
        for automatic in [true, false] {
            for project in [false, true] {
                for photo in [true, false] {
                    let directory = tempfile::tempdir().unwrap();
                    let root = dunce::canonicalize(directory.path()).unwrap();
                    assert!(std::process::Command::new("git").args(["init", "--quiet"]).current_dir(&root).status().unwrap().success());
                    let workspace = root.join("project");
                    std::fs::create_dir(&workspace).unwrap();
                    let git = |args: &[&str]| {
                        let output = std::process::Command::new("git").current_dir(&workspace).args(args).output().unwrap();
                        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
                        output.stdout
                    };
                    git(&["init", "--quiet"]);
                    std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
                    git(&["add", "tracked.txt"]);
                    git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture"]);
                    let destination = root.join("worktrees");
                    // Load persisted preferences before connecting, as a configured
                    // installation does. No test depends on default/off settings.
                    std::fs::write(root.join("bex-worktrees.json"), serde_json::to_vec(&json!({
                        "settings":{"createOnNewSession":automatic,"worktreeDirectory":destination}
                    })).unwrap()).unwrap();
                    std::fs::write(root.join("bex-projects.json"), serde_json::to_vec(&json!([{"id":"default","name":"Default checkout","roots":[{"path":root}]}])).unwrap()).unwrap();
                    let source = root.join("photo.png");
                    let bytes = include_bytes!("../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png");
                    std::fs::write(&source, bytes).unwrap();
                    let fixture = start_host(&root).await;
                    let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
                    let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None).await.unwrap();
                    // Compare submission effects after asynchronous model defaults
                    // have loaded, so catalog normalization cannot change the draft.
                    let mut updates = store.subscribe();
                    while updates.borrow_and_update().models.is_empty() {
                        updates.changed().await.unwrap();
                    }
                    store.dispatch(Intent::NewChat { cwd: if project { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                    let mut thread_id = None;
                    let mut session_cwd = None;
                    for number in 0..2 {
                        let key = store.snapshot().navigation.draft_key.clone();
                        let prompt = format!("[success] automatic={automatic}, project={project}, photo={photo}, message={number}");
                        store.dispatch(Intent::SetDraftText { thread_id: key.clone(), text: prompt.clone() }).await.unwrap();
                        let uploaded = if photo {
                            store.dispatch(Intent::UploadAttachment(op::UploadAttachment {
                                draft_key: key,
                                attachment: Attachment { path: source.to_str().unwrap().into(), name: "photo.png".into(), is_image: true },
                                directory: store.snapshot().navigation.cwd.clone(),
                            })).await.unwrap();
                            let current = store.snapshot();
                            Some(current.drafts[&current.navigation.draft_key].attachments[0].path.clone())
                        } else { None };
                        if !project && number == 0 {
                            let blocked = root.join("bex-chats");
                            std::fs::write(&blocked, "not a directory").unwrap();
                            let before = store.snapshot();
                            let failed = store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: "failed-chat".into() }).await;
                            assert!(failed.is_err(), "an unavailable chat directory must fail before creating a conversation");
                            let after = store.snapshot();
                            assert!(after.navigation.thread_id.is_none());
                            assert!(after.navigation.cwd.is_empty());
                            assert!(after.selected_directory().is_empty());
                            assert!(after.pending_submissions.is_empty());
                            assert_eq!(after.drafts[&after.navigation.draft_key], before.drafts[&before.navigation.draft_key]);
                            std::fs::remove_file(blocked).unwrap();
                            // Foreground reconnection reloads Host state and clears the
                            // previous connection's reported error without discarding drafts.
                            store.disconnect().await.unwrap();
                            store.reconnect(&endpoint, &fixture.ticket, None).await.unwrap();
                        }
                        let sent = store.dispatch(Intent::Submit { thread_id: thread_id.clone(), client_user_message_id: format!("client-{number}").into() }).await;
                        assert!(matches!(sent.unwrap_or_else(|error| panic!("{prompt}: {error}")), Outcome::Submitted { .. }));
                        let id = store.snapshot().navigation.thread_id.clone().unwrap();
                        if let Some(previous) = &thread_id { assert_eq!(&id, previous); }
                        let mut updates = store.subscribe();
                        loop {
                            let completed = updates.borrow_and_update().conversations.get(&id).is_some_and(|thread| {
                                thread.turns.as_ref().is_some_and(|turns| turns.len() == number + 1 && turns[number].status.label() == "completed")
                            });
                            if completed { break; }
                            updates.changed().await.unwrap();
                        }
                        let cwd = store.snapshot().navigation.cwd.clone();
                        if let Some(previous) = &session_cwd { assert_eq!(&cwd, previous); }
                        if automatic && project {
                            assert_eq!(Path::new(&cwd).file_name(), workspace.file_name());
                            assert_eq!(Path::new(&cwd).parent().unwrap().parent().unwrap(), destination);
                        }
                        else if project { assert_eq!(Path::new(&cwd), &workspace); }
                        else {
                            assert!(store.snapshot().selected_directory().is_empty(), "an unselected chat must remain unselected: {cwd}");
                            assert_eq!(Path::new(&cwd), root.join("bex-chats"));
                            let snapshot = store.snapshot();
                            assert_eq!(snapshot.conversations[&id].project_id, models::ProjectMembership::Unassigned {});
                            assert_eq!(snapshot.conversations[&id].cwd.as_deref(), root.join("bex-chats").to_str());
                        }
                        store.dispatch(Intent::ShowThreadList).await.unwrap();
                        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
                        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                        if project {
                            store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace { cwd: cwd.clone() })).await.unwrap();
                        } else {
                            assert!(store.snapshot().workspace.review_cwd.is_none());
                            assert!(store.snapshot().workspace.review.is_none());
                        }
                        store.dispatch(Intent::ListFiles(op::ListFiles { path: cwd.clone() })).await.unwrap();
                        assert_eq!(store.snapshot().workspace.directory.as_ref().unwrap().path, cwd);
                        assert_eq!(store.snapshot().navigation.cwd, cwd, "reopening must preserve the execution directory");
                        assert_eq!(store.snapshot().selected_directory(), if project { cwd.clone() } else { String::new() });
                        let snapshot = store.snapshot();
                        assert!(snapshot.connected);
                        assert!(snapshot.error.is_none(), "{prompt}: {:?}", snapshot.error);
                        assert!(snapshot.pending_submissions.is_empty());
                        let listed = snapshot.threads.as_ref().unwrap().data.iter().find(|thread| thread.id.as_ref() == Some(&id)).expect("sent conversation must be listed");
                        assert_eq!(listed.project_id, if project { models::ProjectMembership::Assigned("default".into()) } else { models::ProjectMembership::Unassigned {} });
                        let restored: agent_core::state::Snapshot = serde_json::from_slice(&serde_json::to_vec(snapshot.as_ref()).unwrap()).unwrap();
                        assert_eq!(restored.selected_directory(), snapshot.selected_directory());

                        let draft = &snapshot.drafts[&agent_core::state::DraftKey::from(&id)];
                        assert!(draft.text.is_empty() && draft.attachments.is_empty());
                        let turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[number];
                        assert!(turn.error.is_none());
                        let items = turn.items.as_ref().unwrap();
                        let user = items.iter().find(|item| matches!(item.body(), agent_protocol::items::ItemBody::UserMessage { .. })).unwrap();
                        assert_eq!(item_text(user), Some(prompt.as_str()));
                        let agent_protocol::items::ItemBody::UserMessage { content, .. } = user.body() else { panic!("user body") }; let mut images = content.iter().filter_map(|input| match input {agent_protocol::items::MessagePart::Image { source } => Some(source), _ => None});
                        if let Some(path) = uploaded {
                            assert_eq!(images.next().unwrap(), &path);
                            assert_eq!(std::fs::read(path).unwrap(), bytes);
                        }
                        assert!(images.next().is_none());
                        assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { phase: agent_protocol::items::AssistantPhase::Final, .. }) && item_text(item).is_some_and(|text| !text.is_empty())));
                        thread_id = Some(id);
                        session_cwd = Some(cwd);
                    }
                    let worktrees = String::from_utf8(git(&["worktree", "list", "--porcelain"])).unwrap();
                    assert_eq!(worktrees.lines().filter(|line| line.starts_with("worktree ")).count(), if automatic && project { 2 } else { 1 });
                    store.close().await.unwrap();
                    drop(store);
                    endpoint.close().await;
                    fixture.close().await.unwrap();
                }
            }
        }
    }).await.expect("submission matrix exceeded 60 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_registration_pairs_the_local_client_identity_for_direct_connections() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = start_host(a.path()).await;
        let second = start_host(b.path()).await;
        let endpoint_a = Endpoint::bind(first.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let endpoint_b =
            Endpoint::bind(second.credentials.local_identity().await, Relays::Disabled)
                .await
                .unwrap();
        let manager_a =
            agent_core::store::Store::connect(&endpoint_a, &first.ticket, Default::default(), None)
                .await
                .unwrap();
        let manager_b = agent_core::store::Store::connect(
            &endpoint_b,
            &second.ticket,
            Default::default(),
            None,
        )
        .await
        .unwrap();
        use agent_core::state::Intent;
        use agent_core::store::Outcome;
        manager_b
            .dispatch(Intent::CreateInvitation(op::CreateInvitation {}))
            .await
            .unwrap();
        let invitation = manager_b
            .snapshot()
            .management
            .invitation
            .as_ref()
            .unwrap()
            .as_ref()
            .clone();
        let serialized = serde_json::to_string(&manager_b.snapshot()).unwrap();
        assert!(!serialized.contains(&invitation.invitation.to_string()));
        let id = second.ticket.node_id().to_string();
        assert_eq!(
            manager_a
                .dispatch(Intent::PairRemoteHost(op::PairRemoteHost {
                    invitation,
                    name: "remote fixture".into()
                }))
                .await
                .unwrap(),
            Outcome::RemoteHostPaired { id: id.clone() }
        );
        assert_eq!(manager_a.snapshot().management.remotes.len(), 1);
        assert_eq!(manager_a.snapshot().management.remotes[0].id, id);
        assert!(manager_a.snapshot().error.is_none());
        let direct_session = endpoint_a.connect(&second.ticket).await.unwrap();
        let (direct_peer, _direct_events) = direct_session
            .open_peer(Duration::from_secs(10), 128)
            .await
            .unwrap();
        assert_eq!(
            direct_peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["data"],
            json!([])
        );
        assert!((direct_peer).call(&op::CreateInvitation {}).await.is_err());
        manager_a
            .dispatch(Intent::LoadHostManagement(op::LoadHostManagement {}))
            .await
            .unwrap();
        let snapshot = manager_a.snapshot();
        assert_eq!(snapshot.management.remotes.len(), 1);
        assert_eq!(snapshot.management.remotes[0].id, id);
        assert_eq!(
            snapshot.management.remotes[0]
                .ticket
                .parse::<Ticket>()
                .unwrap()
                .node_id(),
            second.ticket.node_id()
        );
        manager_a
            .dispatch(Intent::RemoveRemoteHost(op::RemoveRemoteHost { id }))
            .await
            .unwrap();
        assert!(manager_a.snapshot().management.remotes.is_empty());
        manager_a
            .dispatch(Intent::LoadHostManagement(op::LoadHostManagement {}))
            .await
            .unwrap();
        assert!(manager_a.snapshot().management.remotes.is_empty());
        direct_peer.close().await;
        direct_session.close();
        manager_a.close().await.unwrap();
        manager_b.close().await.unwrap();
        first.close().await.unwrap();
        second.close().await.unwrap();
    })
    .await
    .expect("remote registration deadline");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_task_uses_cached_history_while_the_host_read_is_pending() {
    use agent_core::state::Intent;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        std::fs::write(root.join("list-fixture.json"), serde_json::to_vec(&json!([
            {"id":"selected","name":"Selected task","cwd":root,"historyMode":"paginated","createdAt":1,"updatedAt":1}
        ])).unwrap()).unwrap();
        let fixture = start_host(&root).await;
        let local = fixture.local().await.unwrap();
        let invitation: Invitation = local.peer.call(&op::CreateInvitation {}).await.unwrap();
        let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), Some(invitation.invitation)).await.unwrap();
        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
        // A paired mobile client must resume without local management privileges.
        tokio::time::timeout(Duration::from_millis(500), store.resume(&endpoint, &fixture.ticket)).await.unwrap().unwrap();
        store.dispatch(Intent::SetDraftText { thread_id: agent_protocol::session::SessionRef {provider:agent_protocol::session::ProviderKind::Codex,id:"selected".into()}.into(), text: "Unsent draft".into() }).await.unwrap();
        let mut saved = None;
        let mut owner = Some(store);
        for cached in [false, true] {
            let store = if let Some(store) = owner.take() { store } else {
                Store::connect(&endpoint, &fixture.ticket, saved.take().unwrap(), None).await.unwrap()
            };
            assert_eq!(store.snapshot().conversations.contains_key(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() }), cached);
            if cached {
                std::fs::write(root.join("background-reply"), "Latest reply from another client").unwrap();
            }
            std::fs::write(root.join("hold-history-reads"), []).unwrap();
            let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() })));
            tokio::pin!(opening);
            assert_eq!(store.snapshot().navigation.thread_id.as_ref().map(|session| session.id.as_str()), Some("selected"));
            assert_eq!(store.snapshot().navigation.draft_key, agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() }));
            assert_eq!(store.snapshot().drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() })].text, "Unsent draft");
            while !root.join("history-read-held").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(tokio::time::timeout(Duration::from_millis(50), &mut opening).await.is_err());
            if cached {
                let thread = &store.snapshot().conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() }];
                assert_eq!(item_text(&(thread.turns.as_ref().unwrap().last().unwrap().items.as_ref().unwrap()[0])), Some("History for Selected task"));
            }
            // List refresh shares the production transport but must not wait for history.
            store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
            assert!(store.snapshot().threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_ref().map(|session| session.id.as_str()) == Some("selected")));
            std::fs::remove_file(root.join("hold-history-reads")).unwrap();
            std::fs::remove_file(root.join("history-read-held")).unwrap();
            opening.await.unwrap();
            let current = store.snapshot();
            let last = current.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() }].turns.as_ref().unwrap().last().unwrap();
            assert_eq!(last.status.label(), "completed");
            assert_eq!(item_text(&(last.items.as_ref().unwrap()[0])), Some(if cached { "Latest reply from another client" } else { "History for Selected task" }));
            assert_eq!(current.navigation.thread_id.as_ref().map(|session| session.id.as_str()), Some("selected"));
            assert_eq!(current.drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "selected".into() })].text, "Unsent draft");
            assert!(current.error.is_none(), "{:?}", current.error);
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            saved = Some(serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap()).unwrap());
            store.close().await.unwrap();
        }
        endpoint.close().await;
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("opening a task exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_turn_history_preserves_both_responses_after_reopening_and_restoration() {
    use agent_core::state::Intent;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None)
            .await
            .unwrap();
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SetDraftText {
                thread_id: key,
                text: "[duplicate] Preserve both persisted responses".into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "duplicate-message".into(),
            })
            .await
            .unwrap();
        let id = store.snapshot().navigation.thread_id.clone().unwrap();
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if snapshot.conversations[&id]
                .turns
                .as_ref()
                .is_some_and(|turns| {
                    turns.first().is_some_and(|turn| {
                        turn.status == agent_protocol::execution::TurnStatus::Completed
                    })
                })
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        let mut store = store;
        for restore in [false, true] {
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            if restore {
                let saved = serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap())
                    .unwrap();
                store.close().await.unwrap();
                store = Store::connect(&endpoint, &fixture.ticket, saved, None)
                    .await
                    .unwrap();
            }
            store
                .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                .await
                .unwrap();
            let snapshot = store.snapshot();
            let items: Vec<_> = snapshot.conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .iter()
                .flat_map(|turn| turn.items.as_ref().unwrap())
                .collect();
            let mut unique_items = std::collections::HashSet::new();
            assert!(
                items.iter().all(|item| unique_items.insert(&item.id)),
                "turn hydration duplicated another occurrence's items"
            );
            assert!(
                items
                    .iter()
                    .any(|item| item.id.starts_with("fixture-final-")),
                "latest response missing after restore={restore}"
            );
            assert!(
                items
                    .iter()
                    .any(|item| item.id == "duplicate-history-old".into()),
                "older response missing after restore={restore}"
            );
            let rendered = agent_core::presentation::conversation::project_conversation(
                &snapshot,
                snapshot.conversations[&id].clone(),
                &None,
            );
            let responses: Vec<_> = rendered
                .turns
                .iter()
                .flat_map(|turn| &turn.rows)
                .filter_map(|row| match &row.content {
                    agent_core::presentation::conversation::ConversationRowContent::Response {
                        item,
                        ..
                    } => Some(item.data.id.as_str()),
                    _ => None,
                })
                .collect();
            assert!(
                responses.iter().any(|id| id.starts_with("fixture-final-")),
                "latest response collapsed after restore={restore}"
            );
            assert!(responses.contains(&"duplicate-history-old"));
            assert!(snapshot.pending_submissions.is_empty());
            assert!(
                snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
            );
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        }
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("repeated-turn history exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refreshed_history_pages_recover_every_turn_and_item_through_store() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let turns = (0..12).map(|turn| json!({"id":format!("turn-{turn}"),"status":"completed",
            "items":(0..if turn == 11 { 620 } else { 1 }).map(|item| json!({
                "id":format!("item-{turn}-{item}"),"type":"agentMessage","text":format!("answer {turn}/{item}")
            })).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([{
            "id":"history","name":"History pagination","cwd":directory.path(),"historyMode":"paginated","turns":turns
        }])).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let previous: models::Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"history"},"historyCursor":null,
            "turns":[{"id":"turn-0","status":"completed","items":[{"id":"item-0-0","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"answer 0/0","phase":"unknown"}}}}}]}, {"id":"turn-11","status":"completed","items":[{"id":"item-11-0","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"answer 11/0","phase":"unknown"}}}}}],
                "itemsHasMore":false,"itemsNextCursor":null}]})).unwrap();
        let initial = Snapshot {
            conversations: Arc::new(std::collections::BTreeMap::from([(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }, Arc::new(previous))])),
            ..Default::default()
        };
        let store = Store::new((local.peer, local.events), initial);
        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
        store.dispatch(Intent::ReadThread(op::ReadThread::new(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }))).await.unwrap();
        let snapshot = store.snapshot();
        let thread = &snapshot.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }];
        assert_eq!(thread.turns.as_ref().unwrap().len(), 5, "refresh must not attach disconnected cached turns");
        assert_eq!(thread.history_has_more, Some(true));
        let latest = thread.turns.as_ref().unwrap().last().unwrap();
        assert_eq!(latest.items.as_ref().unwrap().len(), 500);
        assert_eq!(latest.items_has_more, Some(true));
        assert_eq!(latest.items.as_ref().unwrap()[0].id, "item-11-120".into());
        for _ in 0..20 {
            let snapshot = store.snapshot();
            if snapshot.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }].history_has_more != Some(true) { break; }
            store.dispatch(Intent::ReadOlder { thread_id: agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() } }).await.unwrap();
        }
        for reopen in [false, true] {
            if reopen {
                store.dispatch(Intent::ReadThread(op::ReadThread::open(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }))).await.unwrap();
            }
            let snapshot = store.snapshot();
            let thread = &snapshot.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "history".into() }];
            assert_eq!(thread.history_has_more, Some(false));
            let loaded = thread.turns.as_ref().unwrap();
            assert_eq!(loaded.len(), turns.len());
            for (loaded, expected) in loaded.iter().zip(&turns) {
                assert_eq!(json!(loaded.id), expected["id"]);
                assert_eq!(loaded.items_has_more, Some(false));
                let actual = loaded.items.as_ref().unwrap();
                let expected = expected["items"].as_array().unwrap();
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(expected) {assert_eq!(json!(actual.id),expected["id"]); assert!(matches!(actual.body(),agent_protocol::items::ItemBody::AssistantText {text,..} if text == expected["text"].as_str().unwrap()));}
            }
            assert_eq!(snapshot.error, None);
        }
        store.close().await.unwrap();
        local.endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("history refresh and recovery exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_small_command_outputs_do_not_delay_opening_history() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let output = "saved command output\n".repeat(100);
        let mut items = vec![json!({"id":"question","type":"userMessage","content":[{"type":"text","text":"Inspect the build"}]})];
        items.extend((0..498).map(|index| json!({
            "id":format!("command-{index}"),"type":"commandExecution",
            "command":"cargo check","status":"completed","aggregatedOutput":output,"exitCode":0
        })));
        items.push(json!({"id":"answer","type":"agentMessage","phase":"final_answer","text":"The build passed"}));
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([{
            "id":"command-history","cwd":directory.path(),"historyMode":"paginated",
            "turns":[{"id":"turn","status":"completed","items":items}]
        }])).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        local.peer.call(&op::ListSessions::new(Default::default())).await.unwrap();
        let (opened, _updates) = local.peer.request_stream::<agent_protocol::session::OpenedSession>(
            &agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                session: agent_protocol::session::SessionRef::new(agent_protocol::session::ProviderKind::Codex, "command-history".to_string()).unwrap(), limit: 5,
            }),
        ).await.unwrap();
        let bytes = agent_protocol::protocol::encode(&opened).unwrap().len();
        assert!(bytes < 100 * 1024, "collapsed command bodies delayed history: {bytes} bytes");
        let turn = &opened.response.thread.turns.as_ref().unwrap()[0];
        let loaded = turn.items.as_ref().unwrap();
        assert_eq!(loaded.len(), 500);
        assert!(matches!(loaded[0].body(), agent_protocol::items::ItemBody::UserMessage {content, ..} if content == &vec![agent_protocol::items::MessagePart::Text {text: "Inspect the build".into()}]));
        assert_eq!(item_text(&(loaded[499])), Some("The build passed"));
        assert_eq!(loaded.iter().filter(|item| item.is_deferred()).count(), 498);
        for index in [1, 249, 498] {
            let detail = local.peer.call(&rpc::ReadItem {
                thread_id: agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "command-history".into() }, turn_id: "turn".into(), item_id: loaded[index].id.clone(),
            }).await.unwrap();
            assert!(matches!(detail.item.body(), agent_protocol::items::ItemBody::CommandExecution {output, ..} if output == items[index]["aggregatedOutput"].as_str().unwrap())); assert!(!detail.item.is_deferred());
        }
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("command history exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_history_loads_conversation_before_lossless_item_details() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("bex-projects.json"), serde_json::to_vec(&json!([{"id":"workspace", "name":"Workspace", "roots":[{"path":directory.path()}]}])).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let mobile = fixture.local().await.unwrap();

        let started = mobile.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path().join("large-history")})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(started["thread"]["projectId"], json!({"Assigned":"workspace"}));
        let thread = &started["thread"]["id"];
        let listed = mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>(json!({"limit":20})).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(listed["data"][0]["id"], *thread);
        assert_eq!(listed["data"][0]["projectId"], json!({"Assigned":"workspace"}));
        let start = std::time::Instant::now();
        // This fixture materializes saved history after creation. Release the
        // empty created view before asking the native adapter for that history.
        let _empty = open_session(&mobile.peer, thread, 5).await.0;
        let preview = open_session(&mobile.peer, thread, 5).await.0["response"].clone();
        let preview_bytes = agent_protocol::protocol::encode(serde_json::from_value::<models::ThreadResponse>(preview.clone()).unwrap()).unwrap().len();
        assert_eq!(preview["thread"]["projectId"], json!({"Assigned":"workspace"}));
        for field in ["id", "capabilities"] {
            assert!(!started["thread"][field].is_null(), "missing {field}");
            assert_eq!(started["thread"][field], listed["data"][0][field]);
            assert_eq!(started["thread"][field], preview["thread"][field]);
        }
        println!("large history preview: {preview_bytes} bytes, {} ms", start.elapsed().as_millis());
        // A byte budget is deterministic; machine speed and network scheduling are not.
        assert!(preview_bytes < 16 * 1024, "collapsed output must not delay the conversation: {preview_bytes} bytes");
        let turn = &preview["thread"]["turns"][0];
        assert_eq!(body_json(&turn["items"][0])["userMessage"]["content"][0]["text"]["text"], "Read the whole output");
        assert_eq!(body_json(&turn["items"][2])["assistantText"]["text"], "Large history is complete");
        assert_eq!(turn["items"].as_array().unwrap().iter().filter(|item| item["body"]["deferred"].is_object()).map(|item| item["id"].as_str().unwrap()).collect::<Vec<_>>(), ["large-command"]);
        let detail = mobile.peer.call(&serde_json::from_value::<rpc::ReadItem>(json!({"threadId":thread,"turnId":"large-turn","itemId":"large-command"})).unwrap()).await.unwrap();
        let detail = serde_json::to_value(agent_transport::transfers::resolve_item(detail, Some(&mobile.session)).await.unwrap()).unwrap();
        assert_eq!(body_json(&detail["item"])["commandExecution"]["output"], format!("{}END_OF_LARGE_OUTPUT", "output line\n".repeat(700000)));
        assert_eq!(detail["item"]["id"], "large-command");
        assert!(mobile.peer.call(&serde_json::from_value::<rpc::ReadItem>(json!({"threadId":thread,"turnId":"wrong-turn","itemId":"large-command"})).unwrap()).await.is_err());
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([
            {"id":"fixture-long-history","cwd":directory.path(),"historyMode":"paginated","updatedAt":1}
        ])).unwrap()).unwrap();
        mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>(json!({"useStateDbOnly":true})).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let mut page = open_session(&mobile.peer, &json!({"provider":"codex","id":"fixture-long-history"}), 5).await.0["response"].clone();
        assert_eq!(page["thread"]["turns"].as_array().unwrap().len(), 5);
        assert_eq!(page["thread"]["turns"].as_array().unwrap().iter().map(|t| t["items"].as_array().unwrap().len()).sum::<usize>(), 500);
        assert_eq!(page["thread"]["turns"][4]["items"][153]["id"], "long-latest-message");
        for limit in (10..=100).step_by(5) {
            if page["thread"]["historyHasMore"] != true { break; }
            page = open_session(&mobile.peer, &json!({"provider":"codex","id":"fixture-long-history"}), limit).await.0["response"].clone();
        }
        assert_eq!(page["thread"]["historyHasMore"], false);
        let turns = page["thread"]["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 10);
        let items: Vec<_> = turns.iter().flat_map(|turn| turn["items"].as_array().unwrap()).collect();
        let ids: std::collections::HashSet<_> = items.iter().map(|item| item["id"].as_str().unwrap()).collect();
        assert_eq!(ids.len(), items.len(), "no item is duplicated when expanding the window");
        assert_eq!(items.len(), 3718);

        let started = mobile.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path()})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let image_thread = &started["thread"]["id"];

        let (_, mut messages) = open_session(&mobile.peer, image_thread, 5).await;
        mobile.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":image_thread,"input":[{"text":{"text":"[generated-images]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        completed_turn(&mut messages).await;
        let history = open_session(&mobile.peer, image_thread, 5).await.0["response"].clone();
        let turn = &history["thread"]["turns"][0];
        let images: Vec<_> = turn["items"].as_array().unwrap().iter().filter(|item| body_json(item)["imageGeneration"].is_object()).collect();
        assert_eq!(images.len(), 2);
        let original = std::fs::read(directory.path().join("fixture image.png")).unwrap();
        for item in images {
            assert_eq!(item["status"], "completed");
            let full = mobile.peer.call(&serde_json::from_value::<rpc::ReadItem>(json!({"threadId":image_thread,"turnId":turn["id"],"itemId":item["id"]})).unwrap()).await.unwrap();
            let full = serde_json::to_value(agent_transport::transfers::resolve_item(full, Some(&mobile.session)).await.unwrap()).unwrap();
            assert!(STANDARD.decode(body_json(&full["item"])["imageGeneration"]["data"].as_str().unwrap()).unwrap() == original, "native image details must be lossless");
            if item["body"]["deferred"].is_object() { assert!(body_json(item)["imageGeneration"]["data"].is_null(), "a truncated base64 value must never be rendered as an image"); }
            else { assert!(STANDARD.decode(body_json(item)["imageGeneration"]["data"].as_str().unwrap()).unwrap() == original); }
        }
        mobile.close().await;
        fixture.close().await.unwrap();
    }).await.expect("large history loop exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn title_lists_are_recent_scoped_small_and_expand_without_loading_bodies() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let mut projects = Vec::new();
        let mut threads = Vec::new();
        for project in 1..=7 {
            let id = format!("project-{project}");
            let cwd = directory.path().join(&id);
            projects.push(json!({"id":id,"name":format!("Project {project:02}"),"roots":[{"path":cwd}]}));
            for index in 1..=18 {
                threads.push(json!({"id":format!("p{project}-{index}"),"projectId":null,"cwd":cwd,"name":format!("Project {project:02} conversation {index:02}"),"updatedAt":project*100+index,"preview":"unused history".repeat(1000)}));
            }
        }
        for index in 1..=18 {
            threads.push(json!({"id":format!("chat-{index}"),"cwd":directory.path().join("unassigned"),"name":format!("Chat {index:02}"),"updatedAt":index}));
        }
        threads.extend([
            json!({"id":"explicit","projectId":"native-project-other","cwd":directory.path().join("project-3"),"name":"Explicit assignment","updatedAt":90000}),
            json!({"id":"projectless","projectId":null,"cwd":directory.path().join("bex-chats"),"name":"Explicit chat","updatedAt":90001}),
            json!({"id":"worktree","projectId":"native-project-other","cwd":directory.path().join("worktree"),"name":"Worktree conversation","updatedAt":90002}),
        ]);
        let rollout = directory.path().join("external-rollout.jsonl");
        std::fs::write(&rollout, "initial\n").unwrap();
        let external = threads.iter_mut().find(|thread| thread["id"] == "p5-1").unwrap();
        external["historyMode"] = json!("paginated");
        external["status"] = json!({"type":"notLoaded"});
        external["path"] = json!(rollout);
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        std::fs::write(directory.path().join("bex-projects.json"), serde_json::to_vec(&projects).unwrap()).unwrap();
        let checkout = directory.path().join("worktree");
        std::fs::write(directory.path().join("bex-worktrees.json"), json!({"workspaceRoots":{checkout.to_str().unwrap():directory.path().join("project-5")}}).to_string()).unwrap();
        let fixture = start_host(directory.path()).await;
        let mobile = fixture.local().await.unwrap();

        let request = |project_limit, chat_limit, thread_limit| json!({"projectLimit":project_limit,"chatLimit":chat_limit,"projectThreadLimits":{"project-5":thread_limit}});
        let start = std::time::Instant::now();
        let first = mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>((request(5, 5, 5)).clone()).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let bytes = agent_protocol::protocol::encode(serde_json::from_value::<models::ThreadList>(first.clone()).unwrap()).unwrap().len();
        println!("title list through iroh: {bytes} bytes, {} ms", start.elapsed().as_millis());
        assert!(bytes < 16 * 1024, "initial titles exceeded the transfer budget");
        let rows = first["data"].as_array().unwrap();
        assert_eq!(rows.len(), 30);
        assert_eq!(first["projects"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreProjects"], true);
        assert_eq!(first["projects"].as_array().unwrap().iter().take(5).map(|project| project["id"].as_str().unwrap()).collect::<Vec<_>>(), ["project-5", "project-3", "project-7", "project-6", "project-4"]);
        assert_eq!(rows[0]["id"], json!({"provider":"codex","id":"worktree"}));
        assert_eq!(rows[4]["id"], json!({"provider":"codex","id":"p5-15"}));
        assert_eq!(rows[5]["id"], json!({"provider":"codex","id":"explicit"}));
        assert_eq!(rows[25]["id"], json!({"provider":"codex","id":"projectless"}));
        assert_eq!(rows[29]["id"], json!({"provider":"codex","id":"chat-15"}));
        assert!(rows.iter().all(|thread| thread["turns"].is_null() && thread["preview"].is_null()));
        assert_eq!(first["moreProjectIds"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreChats"], true);

        let more = mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>((request(5, 5, 15)).clone()).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(more["data"].as_array().unwrap().len(), 40);
        assert_eq!(more["data"][14]["id"], json!({"provider":"codex","id":"p5-5"}));
        let end = mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>((request(15, 25, 25)).clone()).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(end["data"].as_array().unwrap().len(), 19 + 6*5 + 19);
        assert_eq!(end["hasMoreChats"], false);
        assert_eq!(end["hasMoreProjects"], false);
        assert!(!end["moreProjectIds"].as_array().unwrap().contains(&json!("project-5")));
        let found = mobile.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>(json!({"searchTerm":"Project 01"})).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(found["projects"].as_array().unwrap().len(), 1);
        assert_eq!(found["data"].as_array().unwrap().len(), 5);
        assert_eq!(found["data"][0]["id"], json!({"provider":"codex","id":"p1-18"}));
        let body = open_session(&mobile.peer, &json!({"provider":"codex","id":"p5-1"}), 5).await.0["response"].clone();
        assert_eq!(body["thread"]["projectId"], json!({"Assigned":"project-5"}));
        assert_eq!(body_json(&body["thread"]["turns"][0]["items"][0])["assistantText"]["text"], "History for Project 05 conversation 01");
        assert_eq!(body["thread"]["status"], "unknown");
        let item = mobile.peer.call(&serde_json::from_value::<rpc::ReadItem>(json!({"threadId":{"provider":"codex","id":"p5-1"},"turnId":"turn-p5-1","itemId":"answer-p5-1"})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(body_json(&item["item"])["assistantText"]["text"], "History for Project 05 conversation 01");
        assert!(agent_protocol::protocol::json_boundary::call("host/thread/watch", json!({"watchId":1,"threadId":{"provider":"codex","id":"p5-1"},"path":rollout})).is_err(), "external rollout following is retired");
        mobile.close().await;
        fixture.close().await.unwrap();
    }).await.expect("title list loop exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_worktree_settings_apply_to_new_threads_and_preserve_project_membership() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let workspace = root.join("project");
        std::fs::create_dir(&workspace).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git").current_dir(&workspace).args(args).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        };
        git(&["init", "--quiet"]);
        std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture"]);
        std::fs::write(workspace.join(".env"), "FIXTURE_VALUE=isolated\n").unwrap();
        let project_state = root.join("bex-projects.json");
        std::fs::write(&project_state, serde_json::to_vec(&json!([{"id":"workspace","name":"Workspace","roots":[{"path":workspace}]}])).unwrap()).unwrap();
        let server = Arc::new(CodexAppServer::spawn(codex_fixture::config(&root)).await.unwrap());
        let service = HostRpcService::new(Ok(server.clone()), ProjectStore::new(root.join("bex-worktrees.json")));
        let mut session = service.open_session();
        async fn request(service: &HostRpcService, session: &mut host_daemon::HostSession, method: &str, params: Value) -> Value {
            let reply = service.dispatch(session.id(), &agent_protocol::protocol::json_boundary::call(method, params).unwrap()).await.unwrap();
            let response = agent_protocol::protocol::json_boundary::reply(method, &reply.initial).unwrap();
            assert!(response.get("result").is_some() || response.get("error").is_some());
            assert!(response.get("error").is_none(), "{response}");
            response["result"].clone()
        }
        let initial = request(&service, &mut session, "host/session/create", json!({"provider":"codex","cwd":workspace})).await;
        assert_eq!(initial["thread"]["cwd"], workspace.to_str().unwrap());
        let destination = root.join("worktree storage");
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env"],"worktreeDirectory":destination});
        assert_eq!(request(&service, &mut session, "host/worktree/settings/update", settings.clone()).await, settings);
        let mut ids = Vec::new();
        let mut paths = Vec::new();
        for _ in 0..2 {
            let started = request(&service, &mut session, "host/session/create", json!({"provider":"codex","cwd":workspace,"model":{"provider":"codex","id":"fixture-model"}})).await;
            let thread = &started["thread"];
            let cwd = std::path::PathBuf::from(thread["cwd"].as_str().unwrap());
            assert_ne!(cwd, workspace);
            assert_eq!(cwd.file_name(), workspace.file_name());
            assert_eq!(cwd.parent().unwrap().parent().unwrap(), destination);
            assert_eq!(std::fs::read(cwd.join(".env")).unwrap(), std::fs::read(workspace.join(".env")).unwrap());
            assert_eq!(thread["projectId"], json!({"Assigned":"workspace"}));
            ids.push(thread["id"].clone());
            paths.push(cwd);
        }
        assert_ne!(paths[0], paths[1]);
        let before = std::process::Command::new("git").current_dir(&workspace).args(["worktree", "list", "--porcelain"]).output().unwrap().stdout;
        for id in &ids {
            let read = request(&service, &mut session, "host/session/open", json!({"session":id,"limit":5})).await["response"].clone();
            assert_eq!(read["thread"]["projectId"], json!({"Assigned":"workspace"}));
        }
        let mut chat_ids = Vec::new();
        for params in [json!({"provider":"codex"}), json!({"provider":"codex","cwd":"  "}), json!({"provider":"codex","cwd":""})] {
                let global = request(&service, &mut session, "host/session/create", params).await;
                assert_eq!(global["thread"]["cwd"], root.join("bex-chats").to_str().unwrap());
                assert_eq!(global["thread"]["projectId"], json!({"Unassigned":{}}));
                chat_ids.push(global["thread"]["id"].clone());
        }
        let restarted = HostRpcService::new(Ok(server.clone()), ProjectStore::new(root.join("bex-worktrees.json")));
        let mut restarted_session = restarted.open_session();
        assert_eq!(request(&restarted, &mut restarted_session, "host/worktree/settings/read", json!({})).await, settings);
        for id in &chat_ids {
            let read = request(&restarted, &mut restarted_session, "host/session/open", json!({"session":id,"limit":5})).await["response"].clone();
            assert_eq!(read["thread"]["cwd"], root.join("bex-chats").to_str().unwrap());
            assert_eq!(read["thread"]["projectId"], json!({"Unassigned":{}}));
        }
        let listed = request(&restarted, &mut restarted_session, "host/session/list", json!({"chatLimit":10})).await;
        for id in &ids {
            let thread = listed["data"].as_array().unwrap().iter().find(|thread| thread["id"] == *id).expect("worktree task must remain in the project list after restart");
            assert_eq!(thread["projectId"], json!({"Assigned":"workspace"}));
        }
        for id in &chat_ids {
            let thread = listed["data"].as_array().unwrap().iter().find(|thread| thread["id"] == *id).expect("chat must remain in the list after restart");
            assert_eq!(thread["projectId"], json!({"Unassigned":{}}));
        }
        let after = std::process::Command::new("git").current_dir(&workspace).args(["worktree", "list", "--porcelain"]).output().unwrap().stdout;
        assert_eq!(before, after, "opening and listing must not create worktrees");
        service.close_session(session.id());
        restarted.close_session(restarted_session.id());
        drop(session); drop(restarted_session); drop(service); drop(restarted);
        server.shutdown().await.unwrap();
    }).await.expect("worktree integration exceeded 40 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_edits_preserve_encoding_and_reject_stale_revisions() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let client = fixture.local().await.unwrap();
        let files = directory.path().join("editable");
        std::fs::create_dir(&files).unwrap();
        let path = files.join("document.txt");
        std::fs::write(&path, b"\xef\xbb\xbffirst\r\n").unwrap();
        let original = client
            .peer
            .request::<models::FileContent>(&agent_protocol::protocol::Call::ReadFile(
                serde_json::from_value::<op::ListFiles>(json!({"path":path})).unwrap(),
            ))
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        assert_eq!(original["bom"], true);
        assert_eq!(original["lineEnding"], "crlf");
        let saved = client
            .peer
            .call(
                &serde_json::from_value::<rpc::WriteFile>(
                    json!({"path":path,"revision":original["revision"],"text":"second\n"}),
                )
                .unwrap(),
            )
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        assert_eq!(saved["text"], "second\r\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"\xef\xbb\xbfsecond\r\n");
        std::fs::write(&path, b"external edit").unwrap();
        assert!(
            client
                .peer
                .call(
                    &serde_json::from_value::<rpc::WriteFile>(
                        json!({"path":path,"revision":saved["revision"],"text":"stale"}),
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external edit");
        assert_eq!(
            std::fs::read_dir(&files).unwrap().count(),
            1,
            "atomic save cleans its staging directory"
        );
        client.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("file edit deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_exposes_project_roots_and_structured_tool_results() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("bex-projects.json"), serde_json::to_vec(&json!([{"id":"workspace","name":"Workspace","roots":[{"path":directory.path()}]}])).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let started = local.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path()})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let thread_id = &started["thread"]["id"];

        let (_, mut events) = open_session(&local.peer, thread_id, 5).await;
        local.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"threadId":thread_id,"clientUserMessageId":"wire-fixture","input":[{"text":{"text":"[items]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        completed_turn(&mut events).await;
        let list = local.peer.request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(agent_protocol::operations::ListSessions { query: serde_json::from_value::<models::ListQuery>(json!({})).unwrap() })).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let history = open_session(&local.peer, thread_id, 5).await.0["response"].clone();
        assert_eq!(list["projects"][0]["roots"][0]["path"], directory.path().to_str().unwrap());
        assert!(history["thread"]["turns"][0]["items"].as_array().unwrap().iter().any(|item| item["body"]["inline"]["body"]["toolCall"]["result"].is_object()));
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("wire fixture deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_handshakes_do_not_stop_the_host() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let stranger = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_address_lookup()
            .bind()
            .await
            .unwrap();
        let ticket: iroh_tickets::endpoint::EndpointTicket =
            fixture.ticket.to_string().parse().unwrap();
        assert!(
            stranger
                .connect(ticket.endpoint_addr().clone(), b"wrong-alpn")
                .await
                .is_err()
        );
        let local = fixture.local().await.unwrap();
        assert_eq!(
            local
                .peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["data"],
            json!([])
        );
        stranger.close().await;
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("handshake isolation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upstream_exit_keeps_host_management_connected() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let store = Store::new((local.peer, local.events), Snapshot::default());
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        assert!(store.snapshot().connected);
        std::fs::write(directory.path().join("exit-on-list"), "").unwrap();
        assert!(
            store
                .dispatch(Intent::ListSessions(op::ListSessions::new(
                    Default::default()
                )))
                .await
                .is_err()
        );
        assert!(store.snapshot().error.is_some());
        let management = fixture
            .local()
            .await
            .expect("Codex exit must not close the Host");
        assert!(
            management
                .peer
                .call(&rpc::ReadHostStatus {})
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["nodeId"]
                .is_string()
        );
        assert!(store.snapshot().connected);
        assert!(!fixture.running.is_finished());
        management.close().await;
        store.close().await.unwrap();
        local.session.close();
        local.endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("upstream shutdown deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_invitation_is_rejected_by_daemon_and_remains_unconsumed() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let invitation: Invitation = local.peer.call(&op::CreateInvitation {}).await.unwrap();
        let saved_keys = fixture.memory.clone();
        local.close().await;
        fixture.close().await.unwrap();
        std::fs::remove_file(
            directory
                .path()
                .join(format!("bex-codex-fixture{}", std::env::consts::EXE_SUFFIX)),
        )
        .unwrap();
        let path = directory.path().join("state/trust.json");
        let mut trust: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        trust["trust"]["invitations"][invitation.invitation.to_string()] = json!(0);
        std::fs::write(&path, serde_json::to_vec(&trust).unwrap()).unwrap();
        // Reload this isolated Host's expired trust while preserving its key store.
        let fixture = HostFixture::start(
            directory.path(),
            codex_fixture::config(directory.path()),
            saved_keys,
            "isolated Host",
            false,
            None,
        )
        .await
        .unwrap();
        let stranger = fixture.connect(Identity::generate()).await.unwrap();
        assert!(
            stranger
                .peer
                .call(
                    &serde_json::from_value::<rpc::Pair>(
                        json!({"invitation":invitation.invitation}),
                    )
                    .unwrap()
                )
                .await
                .is_err()
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["trust"]["invitations"][invitation.invitation.to_string()],
            0
        );
        stranger.endpoint.close().await;
        let local = fixture.local().await.unwrap();
        assert_eq!(
            local
                .peer
                .call(&rpc::ReadHostStatus {})
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["devices"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("expired invitation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_session_recovers_the_current_approval_without_event_replay() {
    passive_approval_after(Duration::ZERO).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "301-second QUIC soak; run by the manual Native clients workflow"]
async fn passive_client_can_approve_after_five_minutes_without_reconnecting() {
    passive_approval_after(Duration::from_secs(301)).await;
}

async fn passive_approval_after(delay: Duration) {
    tokio::time::timeout(delay + Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let sender = fixture.local().await.unwrap();
        let started = sender.peer.request::<models::ThreadResponse>(&agent_protocol::protocol::Call::CreateSession(serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path()})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();

        let id = &started["thread"]["id"];
        let (_, mut sender_events) = open_session(&sender.peer, id, 5).await;
        sender.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[approval]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let original = next_change(&mut sender_events, "request").await["request"].clone();
        sender.peer.close().await;
        let passive = fixture.local().await.unwrap();

        let (opened, mut events) = open_session(&passive.peer, id, 5).await;
        let request = opened["response"]["thread"]["requests"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(request["id"], original["id"]);
        tokio::time::sleep(delay).await;
        passive.peer.request::<models::Empty>(&agent_protocol::protocol::Call::AnswerSession(serde_json::from_value::<rpc::SessionAnswer>(json!({"requestId":request["id"],"answer":{"approval":{"choiceId":request["body"]["approval"]["choices"][0]["id"]}}})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert_eq!(completed_turn(&mut events).await["status"], "completed");
        passive.close().await;
        sender.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("unattended approval deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpaired_connections_cannot_exhaust_authorized_session_slots() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let mut strangers = Vec::new();
        for _ in 0..80 {
            let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let session = endpoint.connect(&fixture.ticket).await.ok();
            strangers.push((endpoint, session));
        }
        let local = fixture.local().await.unwrap();
        assert_eq!(
            local
                .peer
                .request::<models::ThreadList>(&agent_protocol::protocol::Call::ListSessions(
                    rpc::ListSessions::new(Default::default())
                ))
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap()["data"],
            json!([])
        );
        for (endpoint, session) in strangers {
            if let Some(session) = session {
                session.close();
            }
            endpoint.close().await;
        }
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("authorized admission deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovered_host_keeps_mobile_and_desktop_turns_in_sync_across_reconnect() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    use agent_protocol::requests::Answer;
    use host_daemon::{
        FileKeyStore, HostRuntime,
        local_host::{LocalHostRegistry, LocalHostState},
    };

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
    tokio::time::timeout(Duration::from_secs(40), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let mobile_state = root.join("mobile-host");
        let desktop_state = root.join("desktop");
        let registry = LocalHostRegistry::new(desktop_state.clone());
        let lease = registry.acquire(&mobile_state).unwrap();
        let credentials = Arc::new(
            HostCredentials::load(
                Arc::new(FileKeyStore(mobile_state.join("identity.keys"))),
                mobile_state.clone(),
            )
            .await
            .unwrap(),
        );
        let host_endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let server = Arc::new(
            CodexAppServer::spawn(codex_fixture::config(&root))
                .await
                .unwrap(),
        );
        let service = HostRpcService::new(
            Ok(server.clone()),
            ProjectStore::new(root.join("bex-worktrees.json")),
        );
        let runtime = Arc::new(
            HostRuntime::new(
                service,
                host_endpoint,
                credentials,
                "shared Host".into(),
                std::time::Duration::from_secs(30 * 24 * 60 * 60),
            )
            .await,
        );
        lease.publish(&runtime.ticket()).unwrap();
        let stop = CancellationToken::new();
        let running = tokio::spawn(runtime.clone().run(stop.clone()));

        // Fresh desktop state must discover the mobile Host and read *its* keys,
        // without provisioning a second identity in the desktop directory.
        let location = registry.resolve(&desktop_state).unwrap();
        assert_eq!(location.directory, mobile_state);
        let LocalHostState::Ready(ticket) = location.state else {
            panic!("Host is not ready")
        };
        assert!(ticket == runtime.ticket());
        assert!(!desktop_state.join("identity.keys").exists());
        assert!(!desktop_state.join("trust.json").exists());
        let desktop_endpoint = Endpoint::bind(
            host_daemon::load_local_identity(&location.directory).unwrap(),
            Relays::Disabled,
        )
        .await
        .unwrap();
        let mut desktop = Store::connect(&desktop_endpoint, &ticket, Snapshot::default(), None)
            .await
            .unwrap();
        desktop
            .dispatch(Intent::CreateInvitation(op::CreateInvitation {}))
            .await
            .unwrap();
        let expires_at = desktop
            .snapshot()
            .management
            .invitation
            .as_ref()
            .unwrap()
            .expires_at;
        let remaining = expires_at
            - std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
        assert!((30 * 24 * 60 * 60 - 5..=30 * 24 * 60 * 60).contains(&remaining));
        let invitation = desktop
            .snapshot()
            .management
            .invitation
            .as_ref()
            .unwrap()
            .invitation;
        let mobile_endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let mobile = Store::connect(
            &mobile_endpoint,
            &ticket,
            Snapshot::default(),
            Some(invitation),
        )
        .await
        .unwrap();
        mobile
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();

        for (index, final_status) in ["completed", "interrupted"].into_iter().enumerate() {
            let previous_id = mobile.snapshot().navigation.thread_id.clone();
            let draft_key = mobile.snapshot().navigation.draft_key.clone();
            let prompt = format!("[approval] shared turn {index}");
            mobile
                .dispatch(Intent::SetDraftText {
                    thread_id: draft_key,
                    text: prompt.clone(),
                })
                .await
                .unwrap();
            mobile
                .dispatch(Intent::Submit {
                    thread_id: previous_id,
                    client_user_message_id: format!("shared-{index}").into(),
                })
                .await
                .unwrap();
            let id = mobile.snapshot().navigation.thread_id.clone().unwrap();
            wait_for(&mobile, |snapshot| snapshot.requests().next().is_some()).await;
            desktop
                .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                .await
                .unwrap();
            let active = |snapshot: &Snapshot| {
                snapshot
                    .conversations
                    .get(&id)
                    .and_then(|thread| thread.turns.as_ref())
                    .and_then(|turns| turns.last())
                    .is_some_and(|turn| {
                        turn.status == agent_protocol::execution::TurnStatus::Running
                    })
            };
            assert!(active(&mobile.snapshot()));
            assert!(active(&desktop.snapshot()));

            // Restore the stale interrupted status from the formerly separate
            // desktop Host. Reconnecting must replace it with the owner's live turn.
            let mut saved = serde_json::to_value(desktop.snapshot()).unwrap();
            saved["conversations"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry[0] == json!(id))
                .unwrap()[1]["turns"][index]["status"] = "interrupted".into();
            desktop.close().await.unwrap();
            let registry = LocalHostRegistry::new(desktop_state.clone());
            let location = registry.resolve(&desktop_state).unwrap();
            let LocalHostState::Ready(reloaded_ticket) = location.state else {
                panic!("Host disappeared")
            };
            assert!(reloaded_ticket == ticket);
            desktop = Store::connect(
                &desktop_endpoint,
                &reloaded_ticket,
                serde_json::from_value(saved).unwrap(),
                None,
            )
            .await
            .unwrap();
            wait_for(&desktop, |snapshot| {
                active(snapshot) && snapshot.requests().next().is_some()
            })
            .await;
            let snapshot = desktop.snapshot();
            let request = snapshot.requests().next().unwrap();
            let choice_id = request.body.choices()[if final_status == "completed" { 0 } else { 3 }]
                .id
                .clone();
            desktop
                .dispatch(Intent::Respond(op::Respond {
                    request_id: request.id.clone(),
                    answer: Answer::Approval { choice_id },
                }))
                .await
                .unwrap();
            for store in [&mobile, &desktop] {
                wait_for(store, |snapshot| {
                    snapshot.conversations[&id]
                        .turns
                        .as_ref()
                        .unwrap()
                        .last()
                        .is_some_and(|turn| turn.status.label() == final_status)
                        && snapshot.requests().next().is_none()
                })
                .await;
                store.dispatch(Intent::ShowThreadList).await.unwrap();
                store
                    .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                    .await
                    .unwrap();
                let snapshot = store.snapshot();
                assert!(snapshot.error.is_none());
                let turns = snapshot.conversations[&id].turns.as_ref().unwrap();
                assert_eq!(turns.len(), index + 1);
                let turn = turns.last().unwrap();
                assert_eq!(turn.status.label(), final_status);
                assert!(
                    turn.items
                        .as_ref()
                        .unwrap()
                        .iter()
                        .any(|item| item_text(item) == Some(&prompt))
                );
                if final_status == "completed" {
                    assert!(turn.items.as_ref().unwrap().iter().any(|item| matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::AssistantText { .. }
                    ) && matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::AssistantText {
                            phase: agent_protocol::items::AssistantPhase::Final,
                            ..
                        }
                    )));
                }
            }
            assert!(mobile.snapshot().pending_submissions.is_empty());
            let snapshot = mobile.snapshot();
            assert!(
                snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
            );
            assert!(
                snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                    .attachments
                    .is_empty()
            );
        }
        mobile.close().await.unwrap();
        desktop.close().await.unwrap();
        mobile_endpoint.close().await;
        desktop_endpoint.close().await;
        stop.cancel();
        running.await.unwrap().unwrap();
        drop(runtime);
        server.shutdown().await.unwrap();
        drop(lease);
        assert!(matches!(
            registry.resolve(&desktop_state).unwrap().state,
            LocalHostState::Stopped
        ));
    })
    .await
    .expect("shared Host conversation synchronization deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_conversations_refresh_the_sidebar_without_manual_reload() {
    use agent_core::state::Intent;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        for automatic in [false, true] {
            for scoped in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let root = dunce::canonicalize(directory.path()).unwrap();
                let project = root.join("project");
                std::fs::create_dir(&project).unwrap();
                let git = |args: &[&str]| {
                    let result = std::process::Command::new("git").current_dir(&project).args(args).output().unwrap();
                    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
                };
                git(&["init", "--quiet"]);
                git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet", "-m", "fixture"]);
                let configured_project = root.join("project-alias");
                #[cfg(unix)]
                std::os::unix::fs::symlink(&project, &configured_project).unwrap();
                #[cfg(not(unix))]
                let configured_project = project.clone();
                std::fs::write(root.join("bex-projects.json"), serde_json::to_vec(&json!([{"id":"project","name":"Project","roots":[{"path":configured_project}]}])).unwrap()).unwrap();
                std::fs::write(root.join("bex-worktrees.json"), serde_json::to_vec(&json!({"settings":{"createOnNewSession":automatic}})).unwrap()).unwrap();
                let program = host_fixture::fixture::Config { deferred_thread_metadata: true, stream_delay_ms: 10, ..Default::default() }
                    .install(Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")), &root).unwrap();
                let fixture = HostFixture::start(&root, AppServerConfig { program, ..Default::default() }, Arc::new(Memory::default()), "isolated", false, None).await.unwrap();
                let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
                let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None).await.unwrap();
                store.dispatch(Intent::NewChat { cwd: if scoped { project.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                let key = store.snapshot().navigation.draft_key.clone();
                store.dispatch(Intent::SetDraftText { thread_id: key, text: "[success] list automatically".into() }).await.unwrap();
                store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: "sidebar-message".into() }).await.unwrap();
                let id = store.snapshot().navigation.thread_id.clone().unwrap();
                let mut updates = store.subscribe();
                let reflected = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let snapshot = updates.borrow_and_update().clone();
                        let complete = snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns.first().is_some_and(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed));
                        let listed = snapshot.thread_list().is_some_and(|list| list.threads.iter().any(|thread| thread.id == id && thread.title == "Completed conversation" && thread.project_id == scoped.then(|| "project".into())));
                        if complete && listed {
                            assert!(snapshot.pending_submissions.is_empty());
                            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
                            assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty());
                            if scoped {
                                let selected = snapshot.selected_directory();
                                let cwd = Path::new(&selected);
                                assert_eq!(cwd.file_name(), project.file_name());
                                if automatic {
                                    assert_eq!(cwd.parent().unwrap().parent().unwrap(), project.join(".worktree"));
                                } else {
                                    assert_eq!(cwd, project);
                                }
                            }
                            break;
                        }
                        updates.changed().await.unwrap();
                    }
                }).await;
                store.close().await.unwrap();
                drop(store);
                endpoint.close().await;
                fixture.close().await.unwrap();
                assert!(reflected.is_ok(), "completed conversation and title did not appear automatically (worktree={automatic}, scoped={scoped})");
            }
        }
    }).await.expect("sidebar regression exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn worktree_management_preserves_conversations_and_recreates_deleted_checkouts() {
    use agent_core::state::Intent;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git").current_dir(&project).args(args).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            output.stdout
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet", "-m", "fixture"]);
        let fixture = start_host(&root).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None).await.unwrap();
        store.dispatch(Intent::UpdateWorktreeSettings(op::UpdateWorktreeSettings { settings: models::WorktreeSettings { create_on_new_session: true, ..Default::default() } })).await.unwrap();
        store.dispatch(Intent::NewChat { cwd: project.to_str().unwrap().into() }).await.unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store.dispatch(Intent::SetDraftText { thread_id: key, text: "[success] [delayed-input] keep running".into() }).await.unwrap();
        store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: "managed".into() }).await.unwrap();
        let id = store.snapshot().navigation.thread_id.clone().unwrap();
        let path = store.snapshot().navigation.cwd.clone();
        let mut updates = store.subscribe();
        let turn_id = loop {
            let snapshot = updates.borrow_and_update().clone();
            if let Some(turn) = snapshot.conversations[&id].turns.as_ref().and_then(|turns| turns.first()) { break turn.id.clone(); }
            updates.changed().await.unwrap();
        };
        store.dispatch(Intent::ListWorktrees(op::ListWorktrees {})).await.unwrap();
        let snapshot = store.snapshot();
        let entries = snapshot.workspace.worktrees.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, path);
        assert_eq!(entries[0].project_path, project.to_str().unwrap());
        assert_eq!(entries[0].threads[0].id, id);
        assert!(entries[0].threads[0].active && entries[0].blocked_reason.is_some());
        assert!(store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.is_err());
        assert!(Path::new(&path).is_dir());
        store.dispatch(Intent::Interrupt(op::Interrupt { thread_id: id.clone(), turn_id })).await.unwrap();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns[0].status.label() == "interrupted") { break; }
            updates.changed().await.unwrap();
        }
        let local = fixture.local().await.unwrap();
        // Failed process starts must not leave a permanent "terminal open" block.
        assert!(local.peer.call(&serde_json::from_value::<rpc::StartTerminal>(json!({"processHandle":"failed-terminal","cwd":root.join("missing"),"size":{"rows":24,"cols":80}})).unwrap()).await.is_err());
        let terminal_directory = {
            #[cfg(unix)]
            {
                let alias = root.join("worktree-alias");
                std::os::unix::fs::symlink(&path, &alias).unwrap();
                alias
            }
            #[cfg(not(unix))]
            { PathBuf::from(&path) }
        };
        local.peer.call(&serde_json::from_value::<rpc::StartTerminal>(json!({"processHandle":"managed-terminal","cwd":terminal_directory,"size":{"rows":24,"cols":80}})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert!(store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.is_err());
        assert!(Path::new(&path).is_dir());
        // Detach retains the PTY and its worktree lease; canonical-path reattach succeeds.
        local.peer.call(&op::DetachTerminal { handle: "managed-terminal".into() }).await.unwrap();
        assert!(store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.is_err());
        local.peer.call(&rpc::StartTerminal { handle: "managed-terminal".into(), cwd: path.clone(), size: rpc::TerminalSize { rows: 24, cols: 80 } }).await.unwrap();
        assert!(store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.is_err());
        local.peer.request::<models::Empty>(&agent_protocol::protocol::Call::KillTerminal(serde_json::from_value::<rpc::TerminalKill>(json!({"processHandle":"managed-terminal"})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
        store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.unwrap();
        assert!(!Path::new(&path).exists());
        assert_eq!(store.snapshot().workspace.worktrees.as_ref().unwrap().len(), 1);
        store.disconnect().await.unwrap();
        store.reconnect(&endpoint, &fixture.ticket, None).await.unwrap();
        store.dispatch(Intent::ListWorktrees(op::ListWorktrees {})).await.unwrap();
        assert_eq!(store.snapshot().workspace.worktrees.as_ref().unwrap().len(), 1);
        assert!(store.snapshot().error.is_none());
        assert!(String::from_utf8(git(&["branch", "--list", "bex/*"])).unwrap().contains("bex/session-"));
        let listed = local.peer.call(&rpc::ListSessions::new(Default::default())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        assert!(listed["data"].as_array().unwrap().iter().any(|thread| thread["id"] == json!(id)), "removal must preserve conversation history");
        let review = local.peer.call(&rpc::ReviewWorkspace { cwd: path.clone() }).await.unwrap();
        assert!(review.files.is_empty());
        assert!(!Path::new(&path).exists(), "reading history must not recreate the worktree");
        git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet", "-m", "advance main"]);
        let receipt = local.peer.call(&rpc::Submission {
            thread_id: id.clone(),
            client_user_message_id: "after-removal".into(),
            input: vec![rpc::Input::Text { text: "[success] continue after removal".into() }],
            model: None,
            effort: None,
            service_tier: None,
        }).await.unwrap();
        assert!(receipt.turn_id.is_some());
        assert!(Path::new(&path).is_dir(), "sending must recreate the checkout");
        assert_eq!(git(&["-C", &path, "rev-parse", "HEAD"]), git(&["rev-parse", "main"]));
        let (opened, _) = open_session(&local.peer, &json!(id), 5).await;
        assert_eq!(opened["response"]["thread"]["cwd"], path);
        assert!(opened.to_string().contains("[success] continue after removal"));
        local.close().await;
        store.close().await.unwrap();
        drop(store);
        endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("worktree management exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn visualization_reaches_store_and_reopens_after_source_removal() {
    use agent_core::presentation::markdown::MarkdownBlock;
    use agent_core::presentation::markdown::markdown_blocks;
    use agent_core::state::Intent;
    use agent_core::store::Outcome;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None)
            .await
            .unwrap();
        let prompt = "[success] [visualize] Compare twelve icons";
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        store
            .dispatch(Intent::SetDraftText {
                thread_id: store.snapshot().navigation.draft_key.clone(),
                text: prompt.into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "visualize".into(),
            })
            .await
            .unwrap();
        let id = store.snapshot().navigation.thread_id.clone().unwrap();
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            if snapshot.conversations[&id]
                .turns
                .as_ref()
                .is_some_and(|turns| {
                    turns
                        .iter()
                        .any(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
                })
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        fn reference(
            snapshot: &agent_core::state::Snapshot,
            id: &agent_protocol::session::SessionRef,
            prompt: &str,
        ) -> String {
            assert!(snapshot.error.is_none());
            assert!(snapshot.pending_submissions.is_empty());
            assert!(
                snapshot.drafts[&agent_core::state::DraftKey::from(id)]
                    .text
                    .is_empty()
            );
            let turns = snapshot.conversations[id].turns.as_ref().unwrap();
            assert_eq!(turns.last().unwrap().status.label(), "completed");
            let items: Vec<_> = turns
                .iter()
                .flat_map(|turn| turn.items.iter().flatten())
                .collect();
            assert!(items.iter().any(|item| item_text(item) == Some(prompt)));
            items
                .iter()
                .filter_map(|item| item_text(item))
                .flat_map(|text| markdown_blocks(text.to_owned()))
                .find_map(|block| match block {
                    MarkdownBlock::Visualization { path } => Some(path),
                    _ => None,
                })
                .expect("visualization must reach conversation")
        }
        let path = reference(&store.snapshot(), &id, prompt);
        let load = || {
            Intent::LoadVisualization(op::LoadVisualization {
                path: path.clone(),
                cwd: String::new(),
            })
        };
        let Outcome::Visualization { html: original } = store.dispatch(load()).await.unwrap()
        else {
            panic!("missing HTML")
        };
        assert!(original.contains("Git の合流") && original.contains("folder-check"));
        assert!(original.contains("sandbox=\"allow-scripts\""));
        std::fs::remove_file(&path).unwrap();
        let persisted =
            serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap()).unwrap();
        store.close().await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, persisted, None)
            .await
            .unwrap();
        store.dispatch(Intent::ShowThreadList).await.unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        assert_eq!(reference(&store.snapshot(), &id, prompt), path);
        assert_eq!(
            store.dispatch(load()).await.unwrap(),
            Outcome::Visualization { html: original }
        );
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("visualization round trip deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_list_tracks_real_worktree_merges_through_host_and_store() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    fn git(cwd: &Path, args: &[&str]) -> String {
        let result = std::process::Command::new("git")
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().to_owned()
    }
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let repo = root.join("repo");
        let checkout = root.join("checkout");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "base"]);
        git(
            &repo,
            &["worktree", "add", "-b", "task", checkout.to_str().unwrap()],
        );
        std::fs::create_dir(checkout.join("nested")).unwrap();
        let fixture = start_host(&root).await;
        let local = fixture.local().await.unwrap();
        let mut ids = Vec::new();
        for cwd in [&checkout, &checkout.join("nested"), &repo, &root] {
            let reply = local
                .peer
                .call(
                    &serde_json::from_value::<op::CreateSession>(
                        json!({"provider":"codex","cwd":cwd}),
                    )
                    .unwrap(),
                )
                .await
                .map(|output| serde_json::to_value(output).unwrap())
                .unwrap();
            ids.push(
                serde_json::from_value::<agent_protocol::session::SessionRef>(
                    reply["thread"]["id"].clone(),
                )
                .unwrap(),
            );
        }
        let store = Store::new((local.peer, local.events), Snapshot::default());
        for (step, expected) in [
            ("fresh", false),
            ("commit", false),
            ("merge", true),
            ("new-work", false),
        ] {
            match step {
                "commit" | "new-work" => {
                    git(&checkout, &["commit", "--allow-empty", "-m", step]);
                }
                "merge" => {
                    git(&repo, &["merge", "--ff-only", "task"]);
                }
                _ => {}
            }
            store
                .dispatch(Intent::ListSessions(op::ListSessions::new(
                    Default::default(),
                )))
                .await
                .unwrap();
            let snapshot = store.snapshot();
            let list = snapshot.thread_list().unwrap();
            for (index, id) in ids.iter().enumerate() {
                let row = list.threads.iter().find(|row| &row.id == id).unwrap();
                assert_eq!(
                    row.worktree_merged,
                    index < 2 && expected,
                    "{step}: {index}"
                );
            }
            assert_eq!(snapshot.error, None);
        }
        git(&repo, &["merge", "--no-ff", "-m", "merge task", "task"]);
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        assert!(
            store
                .snapshot()
                .thread_list()
                .unwrap()
                .threads
                .iter()
                .find(|row| row.id == ids[0])
                .unwrap()
                .worktree_merged
        );
        git(&checkout, &["checkout", "--detach"]);
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        assert!(
            !store
                .snapshot()
                .thread_list()
                .unwrap()
                .threads
                .iter()
                .find(|row| row.id == ids[0])
                .unwrap()
                .worktree_merged
        );
        store.close().await.unwrap();
        local.endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("worktree merge list deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_open_delivers_a_snapshot_before_updates_and_reopens_current_state() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let client = fixture.local().await.unwrap();
        let created = client.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","cwd":directory.path()})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let id = &created["thread"]["id"];
        let (opened, mut events) = client
            .peer
            .request_stream::<agent_protocol::session::OpenedSession>(&agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                    session: serde_json::from_value(id.clone()).unwrap(),
                    limit: 5,
                }))
            .await
            .unwrap();
        assert_eq!(json!(opened.response.thread.id), *id);
        client.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"session update fixture"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let mut thread = opened.response.thread;
        loop {
            let message = events
                .read::<agent_protocol::session::SessionChange>()
                .await
                .unwrap()
                .expect("subscription ended before completion");
            // Applying in receive order and comparing with native history below
            // verifies the stream's snapshot/delta ordering without global counters.
            thread = message.apply(&thread).unwrap();
            if matches!(
                message,
                agent_protocol::session::SessionChange::Turn {
                    completed: true,
                    ..
                }
            ) {
                break;
            }
        }
        let reopened: agent_protocol::session::OpenedSession = client.peer.request::<agent_protocol::session::OpenedSession>(&agent_protocol::protocol::Call::OpenSession(serde_json::from_value::<agent_protocol::session::OpenSession>(json!({"session":id,"limit":5})).unwrap())).await.unwrap();
        assert_eq!(reopened.response.thread.turns, thread.turns);
        drop(events);
        client.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn gallery_reads_native_older_images_after_a_live_turn_completes() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = start_host(directory.path()).await;
    let mobile = fixture.local().await.unwrap();

    let started = mobile
        .peer
        .request::<models::ThreadResponse>(&agent_protocol::protocol::Call::CreateSession(
            serde_json::from_value::<op::CreateSession>(
                json!({"provider":"codex","cwd":directory.path()}),
            )
            .unwrap(),
        ))
        .await
        .map(|output| serde_json::to_value(output).unwrap())
        .unwrap();
    let id = &started["thread"]["id"];
    let (_, mut events) = open_session(&mobile.peer, id, 5).await;
    mobile.peer.call(&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[generated-images] [gallery] Browse all generated images"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
    completed_turn(&mut events).await;
    let peer = Arc::new(mobile.peer);
    let images = peer
        .session_images(
            &serde_json::from_value(id.clone()).unwrap(),
            Some(&mobile.session),
        )
        .await
        .unwrap();
    assert_eq!(
        images.len(),
        8,
        "native older turns and earlier item pages remain accessible"
    );
    peer.close().await;
    mobile.session.close();
    mobile.endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oversized_session_opens_repeatedly_and_downloads_lossless_items_without_reconnecting() {
    tokio::time::timeout(Duration::from_secs(120), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("oversized-history")).unwrap();
        let fixture = start_host(directory.path()).await;
        let mobile = fixture.local().await.unwrap();
        let started = mobile
            .peer
            .call(
                &serde_json::from_value::<op::CreateSession>(
                    json!({"provider":"codex","cwd":directory.path().join("oversized-history")}),
                )
                .unwrap(),
            )
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        let thread = &started["thread"]["id"];
        for _ in 0..2 {
            let opened = open_session(&mobile.peer, thread, 5).await.0;
            assert!(agent_protocol::protocol::encode(&opened).unwrap().len() < 16 * 1024);
            let turn = &opened["response"]["thread"]["turns"][0];
            assert_eq!(
                turn["items"].as_array().unwrap().iter().filter(|item| item["body"]["deferred"].is_object()).map(|item| item["id"].as_str().unwrap()).collect::<Vec<_>>(),
                ["oversized-text","oversized-image","oversized-tool"]
            );
            assert!(
                body_json(&turn["items"][1])["imageGeneration"]["data"].is_null(),
                "base64 must be absent, never truncated"
            );
        }
        for (id, kind, field, byte) in [
            ("oversized-text", "assistantText", "text", b'x'),
            ("oversized-image", "imageGeneration", "data", b'A'),
            ("oversized-tool", "commandExecution", "output", b'z'),
        ] {
            let reply = mobile
                .peer
                .call(
                    &serde_json::from_value::<rpc::ReadItem>(
                        json!({"threadId":thread,"turnId":"oversized-turn","itemId":id}),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(reply.transfer.as_ref().unwrap().size > 16 * 1024 * 1024);
            assert!(serde_json::to_vec(&reply).unwrap().len() < 16 * 1024);
            let item = serde_json::to_value(
                agent_transport::transfers::resolve_item(reply, Some(&mobile.session))
                    .await
                    .unwrap()
                    .item,
            )
            .unwrap();
            let body = body_json(&item)[kind][field].as_str().unwrap();
            assert_eq!(body.len(), 17 * 1024 * 1024);
            assert!(body.bytes().all(|value| value == byte));
        }
        // Exercise the shared Store path used by all native clients: visible
        // messages/images load automatically, while tool details are requested.
        use agent_core::state::Intent;
        use agent_core::store::Store;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None)
            .await
            .unwrap();
        let mut updates = store.subscribe();
        let thread_id: agent_protocol::session::SessionRef =
            serde_json::from_value(thread.clone()).unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(thread_id.clone())))
            .await
            .unwrap();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            let items = &snapshot.conversations[&thread_id].turns.as_ref().unwrap()[0]
                .items
                .as_ref()
                .unwrap();
            if item_text(&items[0]).is_some_and(|text| text.len() == 17 * 1024 * 1024)
                && matches!(items[1].body(), agent_protocol::items::ItemBody::ImageGeneration { data: Some(data), .. } if data.len() == 17 * 1024 * 1024)
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        store
            .dispatch(Intent::ReadItem(op::ReadItem {
                thread_id: thread_id.clone(),
                turn_id: "oversized-turn".into(),
                item_id: "oversized-tool".into(),
            }))
            .await
            .unwrap();
        assert!(matches!(store.snapshot().conversations[&thread_id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[2].body(), agent_protocol::items::ItemBody::CommandExecution {output, ..} if output.len() == 17 * 1024 * 1024));
        store.close().await.unwrap();
        endpoint.close().await;
        mobile
            .peer
            .call(&rpc::ReadHostStatus {})
            .await
            .map(|output| serde_json::to_value(output).unwrap())
            .unwrap();
        mobile.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("large item transfers exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_a_chat_folder_registers_a_project_before_submission() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let folder = root.join("new-project");
        std::fs::create_dir(&folder).unwrap();
        let fixture = start_host(&root).await;
        let local = fixture.local().await.unwrap();
        let direct = fixture.local().await.unwrap();
        let peer = &direct.peer;
        let store = Store::new((local.peer, local.events), Snapshot::default());
        let mut updates = store.subscribe();
        updates
            .wait_for(|snapshot| snapshot.connected && snapshot.threads.is_some())
            .await
            .unwrap();
        for _ in 0..2 {
            let registration = store.dispatch(Intent::AddProject(op::AddProject {
                cwd: folder.to_str().unwrap().into(),
            }));
            let duplicate_request = rpc::AddProject {
                cwd: folder.to_str().unwrap().into(),
            };
            let duplicate = peer.call(&duplicate_request);
            let (registered, duplicate) = tokio::join!(registration, duplicate);
            registered.unwrap();
            assert_eq!(duplicate.unwrap(), folder.to_str().unwrap());
            updates
                .wait_for(|snapshot| {
                    snapshot.navigation.cwd == folder.to_str().unwrap()
                        && snapshot.thread_list().is_some_and(|list| {
                            list.projects
                                .iter()
                                .any(|project| project.name == "new-project")
                        })
                })
                .await
                .unwrap();
            let snapshot = store.snapshot();
            let list = snapshot.thread_list().unwrap();
            assert_eq!(list.projects.len(), 1);
            assert!(list.threads.is_empty());
            assert_ne!(
                snapshot.navigation.draft_key,
                agent_core::state::Navigation::default().draft_key
            );
            assert_eq!(snapshot.error, None);
        }
        let project_id = store.snapshot().thread_list().unwrap().projects[0]
            .id
            .clone();
        let created = peer
            .call(&op::CreateSession {
                provider: agent_protocol::session::ProviderKind::Codex,
                cwd: Some(folder.to_str().unwrap().into()),
                model: None,
            })
            .await
            .unwrap();
        assert_eq!(
            created.thread.project_id.as_deref(),
            Some(project_id.as_str())
        );
        assert!(root.join("bex-projects.json").exists());
        assert!(!root.join(".codex-global-state.json").exists());
        store.close().await.unwrap();
        local.endpoint.close().await;
        direct.close().await;
        let memory = fixture.memory.clone();
        fixture.close().await.unwrap();
        let reopened = HostFixture::start(
            &root,
            codex_fixture::config(&root),
            memory,
            "isolated Host",
            false,
            None,
        )
        .await
        .unwrap();
        let client = reopened.local().await.unwrap();
        let list = client
            .peer
            .call(&op::ListSessions::new(Default::default()))
            .await
            .unwrap();
        assert_eq!(list.projects.len(), 1);
        assert_eq!(list.projects[0].name, "new-project");
        client.close().await;
        reopened.close().await.unwrap();
    })
    .await
    .expect("project registration deadline");
}

#[tokio::test]
async fn composer_catalog_uses_host_provider_and_excludes_disabled_entries() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let catalog = local
            .peer
            .call(&op::LoadComposerCatalog {
                cwd: "/fixture/project".into(),
            })
            .await
            .unwrap();
        assert_eq!(catalog.cwd, "/fixture/project");
        assert!(!catalog.loading);
        assert!(catalog.errors.is_empty());
        assert_eq!(catalog.candidates.len(), 2);
        let plugin = catalog
            .candidates
            .iter()
            .find(|c| c.invocation.kind == agent_protocol::composer::InvocationKind::Plugin)
            .unwrap();
        assert_eq!(plugin.invocation.name, "Fixture Plugin");
        assert_eq!(plugin.invocation.path, "plugin://fixture@local");
        let skill = catalog
            .candidates
            .iter()
            .find(|c| c.invocation.kind == agent_protocol::composer::InvocationKind::Skill)
            .unwrap();
        assert_eq!(skill.invocation.path, "/fixture/skills/review/SKILL.md");
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_routes_client_intents_and_replays_delivery_before_native_echo() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let first = fixture.local().await.unwrap();
        let thread = first
            .peer
            .call(&op::CreateSession {
                provider: agent_protocol::session::ProviderKind::Codex,
                cwd: None,
                model: None,
            })
            .await
            .unwrap()
            .thread
            .id
            .unwrap();
        let input = rpc::Submission {
            thread_id: thread.clone(),
            client_user_message_id: "first-input".into(),
            input: vec![rpc::Input::Text {
                text: "[delayed-input] wait for another client".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
        };
        let (started, duplicate) = tokio::join!(first.peer.call(&input), first.peer.call(&input));
        let started = started.unwrap();
        assert_eq!(
            duplicate.unwrap(),
            started,
            "concurrent identical input intents must share one provider submission"
        );
        assert!(started.turn_id.is_some());
        first.peer.close().await;
        let second = fixture.local().await.unwrap();
        assert_eq!(
            second.peer.call(&input).await.unwrap(),
            started,
            "a repeated ID must replay its receipt without another provider input"
        );
        let steered = second
            .peer
            .call(&rpc::Submission {
                client_user_message_id: "second-input".into(),
                input: vec![rpc::Input::Text {
                    text: "additional input".into(),
                }],
                ..input
            })
            .await
            .unwrap();
        assert_eq!(
            steered.turn_id, started.turn_id,
            "the Host must steer its live turn without client execution state"
        );
        let (opened, mut events) = open_session(&second.peer, &json!(thread), 5).await;
        let deliveries = &opened["response"]["thread"]["submissions"];
        assert_eq!(
            deliveries["first-input"]["accepted"]["turnId"],
            json!(started.turn_id)
        );
        assert_eq!(
            deliveries["second-input"]["accepted"]["turnId"],
            json!(started.turn_id)
        );
        std::fs::write(directory.path().join("release-inputs"), "").unwrap();
        // The fixture releases the first turn and its steered input in
        // independent tasks. Observe both echoes before checking native history;
        // the final-turn event is not a barrier for the other task's echo.
        let mut echoed = std::collections::HashSet::new();
        let mut finished = false;
        while !finished
            || !["first-input", "second-input"]
                .iter()
                .all(|id| echoed.contains(*id))
        {
            use agent_protocol::session::SessionChange;
            let change: SessionChange = events.read().await.unwrap().expect("subscription closed");
            let items = match change {
                SessionChange::Turn { turn, completed } => {
                    finished |= completed;
                    turn.items.unwrap_or_default()
                }
                SessionChange::Item { item, .. } => vec![item],
                _ => Vec::new(),
            };
            for item in items {
                if matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                ) && let Some(id) = &item.client_input_id
                {
                    echoed.insert(id.clone());
                }
            }
        }
        let reopened = open_session(&second.peer, &json!(thread), 5).await.0;
        let turns = reopened["response"]["thread"]["turns"].as_array().unwrap();
        for id in ["first-input", "second-input"] {
            assert_eq!(
                turns
                    .iter()
                    .flat_map(|turn| turn["items"].as_array().unwrap())
                    .filter(|item| item["clientInputId"] == id)
                    .count(),
                1
            );
        }
        second.peer.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("Host submission routing stalled");
}

#[tokio::test]
async fn permission_settings_edit_native_codex_defaults_and_detect_external_changes() {
    use agent_protocol::{permissions::*, session::ProviderKind};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture-native-config.json");
    std::fs::write(&path, r#"{"model":"preserve-me","approval_policy":"on-request","sandbox_mode":"workspace-write"}"#).unwrap();
    let fixture = start_host(root.path()).await;
    let local = fixture.local().await.unwrap();
    let read = ReadPermissionSettings {
        provider: ProviderKind::Codex,
    };
    let mut settings = local.peer.call(&read).await.unwrap();
    assert_eq!(settings.mode, Some(PermissionMode::Ask));
    for (mode, approval, reviewer, sandbox) in [
        (
            PermissionMode::Auto,
            "on-request",
            "auto_review",
            "workspace-write",
        ),
        (
            PermissionMode::FullAccess,
            "never",
            "user",
            "danger-full-access",
        ),
        (PermissionMode::Ask, "on-request", "user", "workspace-write"),
    ] {
        settings = local
            .peer
            .call(&UpdatePermissionSettings {
                provider: read.provider,
                mode,
                version: settings.version,
            })
            .await
            .unwrap();
        assert_eq!(settings.mode, Some(mode));
        let native: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            native,
            json!({"model":"preserve-me","approval_policy":approval,"approvals_reviewer":reviewer,"sandbox_mode":sandbox})
        );
    }
    std::fs::write(
        &path,
        r#"{"approval_policy":"untrusted","sandbox_mode":"read-only"}"#,
    )
    .unwrap();
    assert!(
        local
            .peer
            .call(&UpdatePermissionSettings {
                provider: read.provider,
                mode: PermissionMode::FullAccess,
                version: settings.version
            })
            .await
            .is_err()
    );
    assert_eq!(local.peer.call(&read).await.unwrap().mode, None);
    local.endpoint.close().await;
    fixture.close().await.unwrap();
}

fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
    match item.body() {
        agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
        agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
        agent_protocol::items::ItemBody::Reasoning { content, .. } => {
            content.first().map(String::as_str)
        }
        _ => None,
    }
}

fn body_json(item: &Value) -> &Value {
    let body = &item["body"];
    if body["inline"].is_object() {
        &body["inline"]["body"]
    } else {
        &body["deferred"]["summary"]
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_history_errors_preserve_failure_and_only_confirmed_empty_history() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let cases = [
            (-32600, "empty", "empty", true, false),
            (-32600, "denied", "empty", false, false),
            (-32603, "wrong-code", "wrong-code", false, false),
            (-32600, "wrong-session", "different-session", false, false),
            (-32600, "invalid", "permission denied", false, false),
            (-32601, "fallback", "pagination unavailable", true, true),
        ];
        let threads: Vec<_> = cases.iter().map(|(code, id, detail, _, has_history)| {
            let message = if *detail == "permission denied" || *detail == "pagination unavailable" {
                detail.to_string()
            } else {
                format!("thread {detail} is not materialized yet; thread/turns/list is unavailable before first user message")
            };
            json!({"id":id,"cwd":directory.path(),"historyMode":"paginated",
                "fixtureHistoryError":{"code":code,"message":message},
                "turns":if *has_history {vec![json!({"id":"saved","status":"completed","items":[{"id":"answer","type":"agentMessage","text":"Saved answer"}]})]} else {vec![]}
            })
        }).collect();
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        local.peer.call(&op::ListSessions::new(Default::default())).await.unwrap();
        for ((_, id, _, succeeds, unavailable), native) in cases.iter().zip(&threads) {
            let result = local.peer.request_stream::<agent_protocol::session::OpenedSession>(
                &agent_protocol::protocol::Call::OpenSession(agent_protocol::session::OpenSession {
                    session: agent_protocol::session::SessionRef::new(agent_protocol::session::ProviderKind::Codex, id.to_string()).unwrap(), limit:5,
                })
            ).await;
            if *succeeds {
                let (opened, _updates) = result.unwrap();
                let thread = opened.response.thread;
                if *unavailable {
                    assert!(thread.turns.is_none());
                    let state = thread.history_read_state.unwrap();
                    assert_eq!(state.kind, agent_protocol::session::HistoryReadKind::Unavailable);
                    assert_eq!(state.issues, ["Full history hydration is unavailable; use pagination"]);
                } else {
                    assert!(thread.turns.unwrap().is_empty());
                    let state = thread.history_read_state.unwrap();
                    assert_eq!(state.kind, agent_protocol::session::HistoryReadKind::Complete);
                    assert!(state.issues.is_empty());
                }
            } else {
                let error = match result {
                    Err(agent_protocol::error::PeerError::Remote { error, .. }) => error,
                    _ => panic!("native history failure was replaced with a successful empty snapshot: {id}"),
                };
                let failure: agent_protocol::error::RpcFailure = serde_json::from_str(&error).unwrap();
                assert_eq!(failure.code, "session_open_failed");
                assert_eq!(failure.message, native["fixtureHistoryError"]["message"]);
            }
        }
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("native history classification deadline");
}
