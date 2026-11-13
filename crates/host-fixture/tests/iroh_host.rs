use agent_core::{client::ClientExt, state::operations as op};
use agent_protocol::models;
use agent_protocol::{
    models::Invitation,
    operations as rpc,
    session::{ProviderKind, SessionRef},
};
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
// Shared scenarios exercise different subsets of the transport helpers.
#[allow(dead_code)]
#[path = "support/conversations.rs"]
mod conversations;
use host_fixture::test_support::{HostFixture, Memory, assert_project_directory};

// These transport Hosts run without account management and use an explicit
// Codex new-chat preference.
fn codex_chat_snapshot() -> agent_core::state::Snapshot {
    agent_core::state::Snapshot {
        catalog: agent_core::state::Catalog {
            model_preferences: agent_core::state::ModelPreferences {
                provider: Some(agent_protocol::session::ProviderKind::Codex),
                selections: [(
                    agent_protocol::session::ProviderKind::Codex,
                    agent_core::state::ModelSelection {
                        model: agent_protocol::models::ModelRef {
                            provider: agent_protocol::session::ProviderKind::Codex,
                            id: "fixture-model".into(),
                        },
                        effort: None,
                        service_tier: None,
                    },
                )]
                .into(),
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

fn next_submission_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn input_id(label: &str) -> agent_protocol::ids::ClientInputId {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, label.as_bytes())
        .to_string()
        .into()
}
async fn start_host(directory: &Path) -> HostFixture {
    HostFixture::start(
        directory,
        codex_fixture::config(directory),
        Arc::new(Memory::default()),
        "isolated Host",
        false,
        None,
        None,
    )
    .await
    .unwrap()
}
async fn completed_chat(client: &Client, cwd: Option<String>) -> models::Thread {
    let mut request = conversations::create_request(ProviderKind::Codex, cwd, None);
    request.initial = Some((
        next_submission_id().into(),
        agent_protocol::conversation::InputContent {
            parts: vec![rpc::Input::Text {
                text: "[success] [immediate] prepare workspace".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
            follow_up: Default::default(),
        },
    ));
    let created = conversations::created(client, request).await.unwrap();
    let mut conversation = conversations::Conversation::open(client, &created.thread.id.unwrap())
        .await
        .unwrap();
    // Creation may finish before subscribing, so inspect its saved turn as well.
    loop {
        let thread = conversation.view().thread;
        if thread
            .turns
            .iter()
            .flatten()
            .any(|turn| turn.status == models::TurnStatus::Completed)
        {
            return thread;
        }
        conversation.next().await;
    }
}
async fn open_conversation(peer: &Client, id: &Value) -> (Value, conversations::Conversation) {
    let session = serde_json::from_value(id.clone()).unwrap();
    let stream = conversations::Conversation::open(peer, &session)
        .await
        .unwrap();
    (serde_json::to_value(stream.view()).unwrap(), stream)
}
async fn next_request(events: &mut conversations::Conversation) -> Value {
    loop {
        if let Some(request) =
            events.view().thread.requests.values().find(|request| {
                request.delivery == agent_protocol::session::RequestDelivery::Awaiting
            })
        {
            return json!(request);
        }
        events.next().await;
    }
}
async fn resolved(events: &mut conversations::Conversation, id: &Value) {
    loop {
        if !events
            .view()
            .thread
            .requests
            .keys()
            .any(|key| json!(key) == *id)
        {
            return;
        }
        events.next().await;
    }
}
async fn completed_turn(events: &mut conversations::Conversation) -> Value {
    json!(events.finished().await)
}
async fn imported(fixture: &HostFixture, session: &SessionRef) {
    let local = fixture.local().await.unwrap();
    let stream = conversations::Conversation::open(&local.peer, session)
        .await
        .unwrap();
    assert!(stream.replica.cursor().is_some());
    local.close().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_is_atomic_local_management_is_private_and_revocation_closes_active_sessions() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let stranger = fixture.connect(Identity::generate()).await.unwrap();
        assert!(
            conversations::created(
                &stranger.peer,
                conversations::create_request(
                    ProviderKind::Codex,
                    Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),
                    None
                )
            )
            .await
            .is_err()
        );
        stranger.endpoint.close().await;
        assert_eq!(
            local
                .peer
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .map(|output| conversations::list_ids(output.initial))
                .unwrap(),
            Vec::<SessionRef>::new()
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
                .iter()
                .any(|device| device["id"] == json!(paired.endpoint.node_id()))
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
            "Fixture device",
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
                .connection
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .iter()
                .any(|entry| entry.id == device)
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
                .connection
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .iter()
                .any(|entry| entry.id == device)
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
                .connection
                .management
                .status
                .as_ref()
                .unwrap()
                .devices
                .iter()
                .any(|entry| entry.id == device)
        );
        assert!(
            paired
                .peer
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .is_err()
        );
        paired.endpoint.close().await;
        let revoked = fixture.connect(Identity::from_bytes(key)).await.unwrap();
        assert!(
            revoked
                .peer
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
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
        let params = rpc::Pair {
            invitation: invitation.invitation,
        };
        let (a, b) = tokio::join!(first.peer.call(&params), second.peer.call(&params));
        assert_ne!(a.is_ok(), b.is_ok());
        let status = local.peer.call(&rpc::ReadHostStatus {}).await.unwrap();
        let paired = if a.is_ok() { &first } else { &second };
        assert_eq!(status.devices.len(), 1);
        assert_eq!(status.devices[0].id, paired.endpoint.node_id().to_string());
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


        let started = conversations::created(&first.peer,conversations::create_request(ProviderKind::Codex,Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),None)).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let id = &started["thread"]["id"];
        let (_, mut first_events) = open_conversation(&first.peer, id).await;
        let (_, mut second_events) = open_conversation(&second.peer, id).await;
        conversations::submit(&first.peer,&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[approval]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let a = next_request(&mut first_events).await;
        let b = next_request(&mut second_events).await;
        assert_eq!(
            a["id"], b["id"],
            "all devices share one native execution request"
        );
        let session=serde_json::from_value(id.clone()).unwrap();
        let request_id=serde_json::from_value(a["id"].clone()).unwrap();
        assert!(conversations::command(&first.peer,&session,agent_protocol::conversation::Command::Answer {
            id:request_id,answer:agent_protocol::requests::Answer::Approval {choice_id:"invalid".into()},
        }).await.is_err());
        let answer=agent_protocol::conversation::Command::Answer {
            id:serde_json::from_value(a["id"].clone()).unwrap(),
            answer:agent_protocol::requests::Answer::Approval {choice_id:a["body"]["approval"]["choices"][0]["id"].as_str().unwrap().into()},
        };
        let (one,two)=tokio::join!(
            conversations::command(&first.peer,&session,answer.clone()),
            conversations::command(&second.peer,&session,answer),
        );
        assert_ne!(one.is_ok(), two.is_ok(), "one validated answer must win");
        for events in [&mut first_events, &mut second_events] {
            resolved(events,&a["id"]).await;
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
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            Default::default(),
            None,
            "Fixture device",
            None,
        )
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
            }))
            .await
            .unwrap();
        let uploaded = PathBuf::from(
            &store.snapshot().input.drafts[&agent_core::state::DraftKey::from("transfer-draft")]
                .attachments[0]
                .path,
        );
        let other = fixture.local().await.unwrap();
        other
            .peer
            .call(&agent_protocol::conversation::records::SubscribeList {
                refresh: false,
                subscription: uuid::Uuid::new_v4(),
                cached: None,
            })
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
        assert!(store.snapshot().panes.primary().navigation.cwd.is_empty());
        store
            .dispatch(Intent::UploadAttachment(op::UploadAttachment {
                draft_key: "unscoped-draft".into(),
                attachment: Attachment {
                    path: source.to_str().unwrap().into(),
                    name: "unscoped.bin".into(),
                    is_image: false,
                },
            }))
            .await
            .unwrap();
        let uploaded = PathBuf::from(
            &store.snapshot().input.drafts[&agent_core::state::DraftKey::from("unscoped-draft")]
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
        assert!(store.snapshot().connection.connected);
        store
            .dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                base_ref: None,
                cwd: directory.path().to_str().unwrap().into(),
            }))
            .await
            .unwrap();
        let snapshot = store.snapshot();
        assert!(
            snapshot
                .panes
                .primary()
                .workspace
                .review
                .as_ref()
                .unwrap()
                .files
                .is_empty()
        );
        assert!(snapshot.panes.primary().error.is_none());
        let retained = store.snapshot().input.drafts
            [&agent_core::state::DraftKey::from("transfer-draft")]
            .clone();
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
        };
        assert!(
            store
                .dispatch(Intent::UploadAttachment(retry.clone()))
                .await
                .is_err()
        );
        assert_eq!(
            store.snapshot().input.drafts[&agent_core::state::DraftKey::from("transfer-draft")],
            retained
        );
        let upload = op::OperationKey::Attachment {
            draft_key: retry.draft_key.clone(),
        };
        assert!(store.snapshot().operation_error(upload.clone()).is_some());
        std::fs::write(&retry.attachment.path, &content).unwrap();
        store
            .dispatch(Intent::UploadAttachment(retry))
            .await
            .unwrap();
        let recovered = store.snapshot();
        let attachments = &recovered.input.drafts
            [&agent_core::state::DraftKey::from("transfer-draft")]
            .attachments;
        assert_eq!(attachments.len(), retained.attachments.len() + 1);
        assert_eq!(attachments[0], retained.attachments[0]);
        assert_eq!(std::fs::read(&attachments[1].path).unwrap(), content);
        assert!(
            recovered.operation_error(upload).is_none(),
            "successful retry retained the upload error"
        );

        store.close().await.unwrap();
        assert!(!store.snapshot().connection.connected);
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
        // Repetition exercises accept cancellation during prior-session cleanup;
        // a sequential reconnect or an imitation of the accept loop misses it.
        for attempt in 0..128 {
            assert_eq!(
                peer.call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .map(|output| conversations::list_ids(output.initial))
                .unwrap(),
                Vec::<SessionRef>::new()
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
            peer.call(&agent_protocol::conversation::records::SubscribeList {
                refresh: false,
                subscription: uuid::Uuid::new_v4(),
                cached: None
            })
            .await
            .map(|output| conversations::list_ids(output.initial))
            .unwrap(),
            Vec::<SessionRef>::new()
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
        let program = host_fixture::fixture::Config {
            trace: true,
            stream_delay_ms: 5,
            ..Default::default()
        }
        .install(
            Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")),
            directory.path(),
        )
        .unwrap();
        let fixture = HostFixture::start(
            directory.path(),
            AppServerConfig {
                program,
                ..Default::default()
            },
            Arc::new(Memory::default()),
            "isolated Host",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            codex_chat_snapshot(),
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let prompt = "[delayed-input] Preserve this input through reconnect";
        store
            .dispatch(Intent::SetDraftText {
                thread_id: store
                    .snapshot()
                    .panes
                    .primary()
                    .navigation
                    .draft_key
                    .clone(),
                text: prompt.into(),
            })
            .await
            .unwrap();
        store.disconnect().await.unwrap();
        assert!(
            store
                .dispatch(Intent::Submit {
                    thread_id: None,
                    client_user_message_id: input_id("offline"),
                    alternate: false,
                })
                .await
                .is_err()
        );
        assert_eq!(
            store.snapshot().input.drafts[&store.snapshot().panes.primary().navigation.draft_key]
                .text,
            prompt
        );
        let restored =
            serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap()).unwrap();
        store.close().await.unwrap();
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            restored,
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: input_id("sent"),
                alternate: false,
            })
            .await
            .unwrap();
        let id = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .clone()
            .unwrap();
        assert!(
            store.snapshot().sessions.subscriptions.contains_key(&id),
            "input must have an active subscription before it reaches the provider"
        );
        let mut updates = store.subscribe();
        // Admission precedes provider acknowledgement; native history is still
        // deliberately unmaterialized after that acknowledgement.
        updates
            .wait_for(|snapshot| {
                snapshot
                    .sessions
                    .conversations
                    .get(&id)
                    .is_some_and(|thread| {
                        thread.preparations.is_empty()
                            && thread.turns.as_ref().is_some_and(|turns| {
                                turns.iter().any(|turn| {
                                    turn.status == agent_protocol::execution::TurnStatus::Running
                                })
                            })
                    })
            })
            .await
            .unwrap();
        let trace = std::fs::read_to_string(directory.path().join("rpc-trace.jsonl")).unwrap();
        let methods: Vec<Value> = trace
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(methods.iter().any(|entry| entry["method"] == "turn/start"));
        assert!(
            methods.iter().all(|entry| !matches!(
                entry["method"].as_str(),
                Some("thread/turns/list" | "thread/items/list" | "thread/timeline/list")
            )),
            "first submission must not wait for an empty history read"
        );
        let client = fixture.local().await.unwrap();
        let current = open_conversation(&client.peer, &json!(id)).await.0;
        assert_eq!(current["thread"]["id"], json!(id));
        assert_eq!(
            current["thread"]["historyReadState"]["type"], "complete",
            "a natively unmaterialized history is empty, not a failed history read"
        );
        assert!(
            current["thread"]["turns"]
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
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            restored,
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        assert!(store.snapshot().panes.primary().error.is_none());
        assert!(store.snapshot().sessions.subscriptions.contains_key(&id));
        assert!(
            store.snapshot().sessions.conversations[&id]
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
            assert!(
                current.panes.primary().error.is_none(),
                "{:?}",
                current.panes.primary().error
            );
            if current.sessions.conversations[&id]
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
        assert!(current.panes.primary().error.is_none());
        assert!(current.sessions.pending_submissions.is_empty());
        assert!(
            current.input.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
                && current.input.drafts[&agent_core::state::DraftKey::from(&id)]
                    .attachments
                    .is_empty()
        );
        let turn = &current.sessions.conversations[&id].turns.as_ref().unwrap()[0];
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

fn assert_submission_items(
    turn: &models::Turn,
    prompt: &str,
    uploaded: Option<&str>,
    bytes: &[u8],
) {
    assert!(turn.error.is_none());
    let items = turn.items.as_ref().unwrap();
    let user = items
        .iter()
        .find(|item| {
            matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            )
        })
        .unwrap();
    assert_eq!(item_text(user), Some(prompt));
    let agent_protocol::items::ItemBody::UserMessage { content, .. } = user.body() else {
        panic!("user body")
    };
    let mut images = content.iter().filter_map(|input| match input {
        agent_protocol::items::MessagePart::Image { source } => Some(source),
        _ => None,
    });
    if let Some(path) = uploaded {
        assert_eq!(images.next().unwrap(), path);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert!(images.next().is_none());
    assert!(items.iter().any(|item| matches!(
        item.body(),
        agent_protocol::items::ItemBody::AssistantText {
            phase: agent_protocol::items::AssistantPhase::Final,
            ..
        }
    ) && item_text(item).is_some_and(|text| !text.is_empty())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_preparation_recovers_in_the_same_conversation_after_reconnecting() {
    use agent_core::{
        state::Intent,
        store::{Outcome, Store},
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let fixture = start_host(&root).await;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, codex_chat_snapshot(), None, "Fixture device", None).await.unwrap();
        let blocked = root.join("bex-chats");
        std::fs::write(&blocked, "not a directory").unwrap();
        store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
        store.dispatch(Intent::SetDraftText { thread_id: store.snapshot().panes.primary().navigation.draft_key.clone(), text: "[success] recover preparation".into() }).await.unwrap();
        assert!(matches!(store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: input_id("recover-chat"), alternate: false }).await.unwrap(), Outcome::StartedThread { .. }));
        let id = store.snapshot().panes.primary().navigation.thread_id.clone().unwrap();
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if let Some(preparation) = snapshot.sessions.conversations[&id].preparations.iter().find(|preparation| matches!(preparation.status, agent_protocol::session::PreparationStatus::Failed { .. })) {
                assert!(preparation.submission.input.iter().any(|input| matches!(input, rpc::Input::Text { text } if text == "[success] recover preparation")));
                assert_eq!(snapshot.sessions.conversations[&id].submissions.get(&input_id("recover-chat")), Some(&agent_protocol::session::SubmissionDelivery::Rejected));
                assert!(snapshot.selected_directory(snapshot.panes.primary()).is_empty());
                break;
            }
            updates.changed().await.unwrap();
        }
        std::fs::remove_file(&blocked).unwrap();
        store.disconnect().await.unwrap();
        store.reconnect(&endpoint, &fixture.ticket, None, "Fixture device").await.unwrap();
        store.dispatch(Intent::RetryPreparation(op::RetryPreparation { thread_id: id.clone(), input_id: input_id("recover-chat"), work_locally: false })).await.unwrap();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if snapshot.sessions.conversations[&id].preparations.is_empty() && snapshot.sessions.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 1 && turns[0].status == agent_protocol::execution::TurnStatus::Completed) {
                assert_eq!(snapshot.panes.primary().navigation.thread_id.as_ref(), Some(&id));
                assert_eq!(Path::new(&snapshot.panes.primary().navigation.cwd), blocked);
                break;
            }
            updates.changed().await.unwrap();
        }
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.unwrap();
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submissions_complete_across_saved_worktree_settings_and_chat_scopes(
    #[values(true, false)] automatic: bool,
    #[values(false, true)] project: bool,
    #[values(true, false)] photo: bool,
) {
    use agent_core::state::Attachment;
    use agent_core::state::Intent;
    use agent_core::store::Outcome;
    use agent_core::store::Store;
    tokio::time::timeout(Duration::from_secs(60), async {
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
        let store = Store::connect(&endpoint, &fixture.ticket, codex_chat_snapshot(), None, "Fixture device", None).await.unwrap();
        // Compare submission effects after asynchronous model defaults
        // have loaded, so catalog normalization cannot change the draft.
        let mut updates = store.subscribe();
        while updates.borrow_and_update().catalog.models.is_empty() {
            updates.changed().await.unwrap();
        }
        store.dispatch(Intent::NewChat { cwd: if project { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
        let mut thread_id = None;
        let mut session_cwd = None;
        for number in 0..2 {
            let key = store.snapshot().panes.primary().navigation.draft_key.clone();
            let prompt = format!("[success] automatic={automatic}, project={project}, photo={photo}, message={number}");
            store.dispatch(Intent::SetDraftText { thread_id: key.clone(), text: prompt.clone() }).await.unwrap();
            let uploaded = if photo {
                store.dispatch(Intent::UploadAttachment(op::UploadAttachment {
                    draft_key: key,
                    attachment: Attachment { path: source.to_str().unwrap().into(), name: "photo.png".into(), is_image: true },
                })).await.unwrap();
                let current = store.snapshot();
                Some(current.input.drafts[&current.panes.primary().navigation.draft_key].attachments[0].path.clone())
            } else { None };
            let sent = store.dispatch(Intent::Submit { thread_id: thread_id.clone(), client_user_message_id: input_id(&format!("client-{number}")), alternate: false }).await.unwrap_or_else(|error| panic!("{prompt}: {error}"));
            assert!(if number == 0 { matches!(sent, Outcome::StartedThread { .. }) } else { matches!(sent, Outcome::Submitted) });
            let id = store.snapshot().panes.primary().navigation.thread_id.clone().unwrap();
            if let Some(previous) = &thread_id { assert_eq!(&id, previous); }
            let mut updates = store.subscribe();
            loop {
                let completed = updates.borrow_and_update().sessions.conversations.get(&id).is_some_and(|thread| {
                    thread.turns.as_ref().is_some_and(|turns| turns.len() == number + 1 && turns[number].status.label() == "completed")
                });
                if completed { break; }
                updates.changed().await.unwrap();
            }
            let cwd = store.snapshot().panes.primary().navigation.cwd.clone();
            if let Some(previous) = &session_cwd { assert_eq!(&cwd, previous); }
            if project {
                assert_project_directory(Path::new(&cwd), &workspace, automatic.then_some(destination.as_path()));
            }
            else {
                assert!(store.snapshot().selected_directory(store.snapshot().panes.primary()).is_empty(), "an unselected chat must remain unselected: {cwd}");
                assert_eq!(Path::new(&cwd), root.join("bex-chats"));
                let snapshot = store.snapshot();
                assert_eq!(snapshot.sessions.conversations[&id].project_id, models::ProjectMembership::Unassigned {});
                assert_eq!(snapshot.sessions.conversations[&id].cwd.as_deref(), root.join("bex-chats").to_str());
            }
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
            if project {
                store.dispatch(Intent::SetProjectExpanded { project_id: "default".into(), expanded: true }).await.unwrap();
                store.dispatch(Intent::ListSessions(op::ListSessions::new((*store.snapshot().lists.list_query).clone()))).await.unwrap();
                loop {
                    if updates.borrow_and_update().lists.project_threads.get("default").is_some_and(|page| page.data.iter().any(|thread| thread.id.as_ref() == Some(&id))) { break; }
                    updates.changed().await.unwrap();
                }
            }
            store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
            if project {
                store.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace { base_ref: None, cwd: cwd.clone() })).await.unwrap();
            } else {
                assert!(store.snapshot().panes.primary().workspace.review_cwd.is_none());
                assert!(store.snapshot().panes.primary().workspace.review.is_none());
            }
            let agent_core::store::Outcome::Directory { listing } = store.dispatch(Intent::ListFiles(op::ListFiles { path: cwd.clone() })).await.unwrap() else { panic!("expected directory result") };
            assert_eq!(listing.path, cwd);
            assert_eq!(store.snapshot().panes.primary().navigation.cwd, cwd, "reopening must preserve the execution directory");
            assert_eq!(store.snapshot().selected_directory(store.snapshot().panes.primary()), if project { cwd.clone() } else { String::new() });
            let snapshot = store.snapshot();
            assert!(snapshot.connection.connected);
            assert!(snapshot.panes.primary().error.is_none(), "{prompt}: {:?}", snapshot.panes.primary().error);
            assert!(snapshot.sessions.pending_submissions.is_empty());
            let page = if project { &snapshot.lists.project_threads["default"] } else { snapshot.lists.threads.as_ref().unwrap() };
            let listed = page.data.iter().find(|thread| thread.id.as_ref() == Some(&id)).expect("sent conversation must be listed");
            assert_eq!(listed.project_id, if project { models::ProjectMembership::Assigned("default".into()) } else { models::ProjectMembership::Unassigned {} });
            let restored: agent_core::state::Snapshot = serde_json::from_slice(&serde_json::to_vec(snapshot.as_ref()).unwrap()).unwrap();
            assert_eq!(restored.selected_directory(restored.panes.primary()), snapshot.selected_directory(snapshot.panes.primary()));

            let draft = &snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)];
            assert!(draft.text.is_empty() && draft.attachments.is_empty());
            let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[number];
            assert_submission_items(turn, &prompt, uploaded.as_deref(), bytes);
            thread_id = Some(id);
            session_cwd = Some(cwd);
        }
        let worktrees = String::from_utf8(git(&["worktree", "list", "--porcelain"])).unwrap();
        assert_eq!(worktrees.lines().filter(|line| line.starts_with("worktree ")).count(), if automatic && project { 2 } else { 1 });
        store.close().await.unwrap();
        drop(store);
        endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("submission case exceeded 60 seconds");
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
        let manager_a = agent_core::store::Store::connect(
            &endpoint_a,
            &first.ticket,
            Default::default(),
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        let manager_b = agent_core::store::Store::connect(
            &endpoint_b,
            &second.ticket,
            Default::default(),
            None,
            "Fixture device",
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
            .connection
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
        assert_eq!(manager_a.snapshot().connection.management.remotes.len(), 1);
        assert_eq!(manager_a.snapshot().connection.management.remotes[0].id, id);
        assert!(manager_a.snapshot().panes.primary().error.is_none());
        let direct_session = endpoint_a.connect(&second.ticket).await.unwrap();
        let (direct_peer, _direct_events) = direct_session
            .open_peer(Duration::from_secs(10), 128)
            .await
            .unwrap();
        assert_eq!(
            direct_peer
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .map(|output| conversations::list_ids(output.initial))
                .unwrap(),
            Vec::<SessionRef>::new()
        );
        assert!((direct_peer).call(&op::CreateInvitation {}).await.is_err());
        manager_a
            .dispatch(Intent::LoadHostManagement(op::LoadHostManagement {}))
            .await
            .unwrap();
        let snapshot = manager_a.snapshot();
        assert_eq!(snapshot.connection.management.remotes.len(), 1);
        assert_eq!(snapshot.connection.management.remotes[0].id, id);
        assert_eq!(
            snapshot.connection.management.remotes[0]
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
        assert!(
            manager_a
                .snapshot()
                .connection
                .management
                .remotes
                .is_empty()
        );
        manager_a
            .dispatch(Intent::LoadHostManagement(op::LoadHostManagement {}))
            .await
            .unwrap();
        assert!(
            manager_a
                .snapshot()
                .connection
                .management
                .remotes
                .is_empty()
        );
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
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), Some(invitation.invitation), "Fixture device", None).await.unwrap();
        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
        // A paired mobile client must resume without local management privileges.
        tokio::time::timeout(Duration::from_millis(500), store.resume(&endpoint, &fixture.ticket, "Fixture device")).await.unwrap().unwrap();
        store.dispatch(Intent::SetDraftText { thread_id: agent_protocol::session::SessionRef {provider:Some(agent_protocol::session::ProviderKind::Codex),id:"selected".into()}.into(), text: "Unsent draft".into() }).await.unwrap();
        let mut saved = None;
        let mut owner = Some(store);
        for cached in [false, true] {
            let store = if let Some(store) = owner.take() { store } else {
                Store::connect(&endpoint, &fixture.ticket, saved.take().unwrap(), None, "Fixture device", None).await.unwrap()
            };
            assert_eq!(store.snapshot().sessions.conversations.contains_key(&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() }), cached);
            if cached {
                // The imported transcript answers; a native change made outside
                // the Host waits for the project to be imported again.
                imported(&fixture, &agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() }).await;
                std::fs::write(root.join("background-reply"), "Latest reply from another client").unwrap();
            } else {
                std::fs::write(root.join("hold-history-reads"), []).unwrap();
            }
            let opening = store.dispatch(Intent::ReadThread(op::ReadThread::open(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() })));
            tokio::pin!(opening);
            assert_eq!(store.snapshot().panes.primary().navigation.thread_id.as_ref().map(|session| session.id.as_str()), Some("selected"));
            assert_eq!(store.snapshot().panes.primary().navigation.draft_key, agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() }));
            assert_eq!(store.snapshot().input.drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() })].text, "Unsent draft");
            if !cached {
                while !root.join("history-read-held").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                tokio::time::timeout(Duration::from_millis(500), &mut opening).await.expect("subscribe does not wait for import").unwrap();
                assert_eq!(store.snapshot().sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(ProviderKind::Codex), id: "selected".into() }].history_read_state.as_ref().unwrap().kind, agent_protocol::session::HistoryReadKind::Loading);
            } else {
                let thread = &store.snapshot().sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() }];
                assert_eq!(item_text(&(thread.turns.as_ref().unwrap().last().unwrap().items.as_ref().unwrap()[0])), Some("History for Selected task"));
            }
            // List refresh shares the production transport but must not wait for history.
            store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
            assert!(store.snapshot().lists.threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_ref().map(|session| session.id.as_str()) == Some("selected")));
            if !cached {
                std::fs::remove_file(root.join("hold-history-reads")).unwrap();
                std::fs::remove_file(root.join("history-read-held")).unwrap();
            }
            if cached { opening.await.unwrap(); }
            let mut ready = store.subscribe();
            ready.wait_for(|snapshot| snapshot.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(ProviderKind::Codex), id: "selected".into() }].turns.as_ref().is_some_and(|turns| !turns.is_empty())).await.unwrap();
            let current = store.snapshot();
            let last = current.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() }].turns.as_ref().unwrap().last().unwrap();
            assert_eq!(last.status.label(), "completed");
            assert_eq!(item_text(&(last.items.as_ref().unwrap()[0])), Some("History for Selected task"));
            assert_eq!(current.panes.primary().navigation.thread_id.as_ref().map(|session| session.id.as_str()), Some("selected"));
            assert_eq!(current.input.drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "selected".into() })].text, "Unsent draft");
            assert!(current.panes.primary().error.is_none(), "{:?}", current.panes.primary().error);
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
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            codex_chat_snapshot(),
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        store
            .dispatch(Intent::NewChat {
                cwd: directory.path().to_str().unwrap().into(),
            })
            .await
            .unwrap();
        let key = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone();
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
                client_user_message_id: input_id("duplicate-message"),
                alternate: false,
            })
            .await
            .unwrap();
        let id = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .clone()
            .unwrap();
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let thread = &snapshot.sessions.conversations[&id];
            if thread.preparations.is_empty()
                && thread.turns.as_ref().is_some_and(|turns| {
                    turns.first().is_some_and(|turn| {
                        turn.status == agent_protocol::execution::TurnStatus::Completed
                    })
                })
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        let management = fixture.local().await.unwrap();
        let cwd = store.snapshot().sessions.conversations[&id]
            .cwd
            .clone()
            .unwrap();
        management
            .peer
            .request::<agent_protocol::project_import::ProjectImportResult>(
                &agent_protocol::protocol::Call::ImportProjects(
                    agent_protocol::project_import::ImportProjects { paths: vec![cwd] },
                ),
            )
            .await
            .unwrap();
        let mut imported_updates = store.subscribe();
        imported_updates
            .wait_for(|snapshot| {
                snapshot.sessions.conversations[&id]
                    .turns
                    .iter()
                    .flatten()
                    .flat_map(|turn| turn.items.iter().flatten())
                    .any(|item| item_text(item) == Some("Older AI response must remain visible."))
            })
            .await
            .unwrap();
        management.close().await;
        let mut store = store;
        for restore in [false, true] {
            store.dispatch(Intent::ShowThreadList).await.unwrap();
            if restore {
                let saved = serde_json::from_slice(&serde_json::to_vec(&store.snapshot()).unwrap())
                    .unwrap();
                store.close().await.unwrap();
                store = Store::connect(
                    &endpoint,
                    &fixture.ticket,
                    saved,
                    None,
                    "Fixture device",
                    None,
                )
                .await
                .unwrap();
            }
            store
                .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                .await
                .unwrap();
            let snapshot = store.snapshot();
            let items: Vec<_> = snapshot.sessions.conversations[&id]
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
                    .any(|item| item_text(item) == Some("シミュレータで完了しました。")),
                "latest response missing after restore={restore}"
            );
            assert!(
                items
                    .iter()
                    .any(|item| item_text(item) == Some("Older AI response must remain visible.")),
                "older response missing after restore={restore}"
            );
            let rendered = agent_core::presentation::conversation::project_conversation(
                agent_core::localization::UiLanguage::Japanese,
                &snapshot.panes.primary().navigation.draft_key,
                &snapshot.sessions.pending_submissions,
                snapshot.sessions.conversations[&id].clone(),
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
                    } => Some(item.data.body.as_deref().unwrap_or_default()),
                    _ => None,
                })
                .collect();
            assert!(
                responses.contains(&"シミュレータで完了しました。"),
                "latest response collapsed after restore={restore}"
            );
            assert!(responses.contains(&"Older AI response must remain visible."));
            assert!(snapshot.sessions.pending_submissions.is_empty());
            assert!(
                snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
            );
            assert!(
                snapshot.panes.primary().error.is_none(),
                "{:?}",
                snapshot.panes.primary().error
            );
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
        let program = host_fixture::fixture::Config { trace: true, ..Default::default() }
            .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), directory.path()).unwrap();
        let fixture = HostFixture::start(directory.path(), AppServerConfig { program, ..Default::default() },
            Arc::new(Memory::default()), "isolated Host", false, None, None).await.unwrap();
        let local = fixture.local().await.unwrap();
        let previous: models::Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"history"},"historyCursor":null,
            "turns":[{"id":"turn-0","status":"completed","items":[{"id":"item-0-0","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"answer 0/0","phase":"unknown"}}}}}]}, {"id":"turn-11","status":"completed","items":[{"id":"item-11-0","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"answer 11/0","phase":"unknown"}}}}}],
                "itemsSummary":true}]})).unwrap();
        let initial = Snapshot {
            sessions: agent_core::state::Sessions {
                conversations: Arc::new(std::collections::BTreeMap::from([(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "history".into() }, Arc::new(previous))])),
                ..Default::default()
            },
            ..Default::default()
        };
        let store = Store::new((local.peer, local.events), initial, None);
        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
        let session = agent_protocol::session::SessionRef::new(agent_protocol::session::ProviderKind::Codex, "history".into()).unwrap();
        // The first open reads native history and imports it once.
        store.dispatch(Intent::ReadThread(op::ReadThread::new(session.clone()))).await.unwrap();
        imported(&fixture, &session).await;
        let read = |directory: &Path| -> Vec<Value> {
            std::fs::read_to_string(directory.join("rpc-trace.jsonl")).unwrap()
                .lines().map(|line| serde_json::from_str(line).unwrap()).collect()
        };
        let timeline: Vec<_> = read(directory.path()).into_iter().filter(|entry| entry["method"] == "thread/turns/list").collect();
        assert!(timeline.len() > 1);
        assert!(timeline.iter().any(|entry| entry["cursor"].is_string()), "the import reads every older page");
        let imported_reads = read(directory.path()).len();
        store.dispatch(Intent::ReadThread(op::ReadThread::new(session.clone()))).await.unwrap();
        let snapshot = store.snapshot();
        let thread = &snapshot.sessions.conversations[&session];
        let window = op::HISTORY_WINDOW_TURNS as usize;
        assert_eq!(thread.turns.as_ref().unwrap().len(), window, "initial display is a bounded turn summary page");
        assert_eq!(thread.history_has_more, Some(true));
        let latest = thread.turns.as_ref().unwrap().last().unwrap();
        assert_eq!(latest.items.as_ref().unwrap().len(), 620);

        assert_eq!(latest.items.as_ref().unwrap().last().unwrap().id, "item-11-619".into());
        let rendered = agent_core::presentation::conversation::project_conversation(agent_core::localization::UiLanguage::Japanese, &snapshot.panes.primary().navigation.draft_key, &snapshot.sessions.pending_submissions, thread.clone(), &None);
        assert_eq!(rendered.turns.len(), window, "the rendered history must contain no empty turn placeholders");
        for _ in 0..20 {
            let snapshot = store.snapshot();
            if snapshot.sessions.conversations[&session].history_has_more != Some(true) { break; }
            store.dispatch(Intent::ReadOlder { thread_id: session.clone() }).await.unwrap();
        }
        assert_eq!(read(directory.path()).len(), imported_reads, "imported history is read from the transcript");
        for reopen in [false, true] {
            if reopen {
                store.dispatch(Intent::ReadThread(op::ReadThread::open(agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "history".into() }))).await.unwrap();
            }
            let snapshot = store.snapshot();
            let thread = &snapshot.sessions.conversations[&agent_protocol::session::SessionRef { provider: Some(agent_protocol::session::ProviderKind::Codex), id: "history".into() }];
            assert_eq!(thread.history_has_more, Some(false));
            let loaded = thread.turns.as_ref().unwrap();
            assert_eq!(loaded.len(), turns.len());
            for (loaded, expected) in loaded.iter().zip(&turns) {
                assert_eq!(json!(loaded.id), expected["id"]);

                let actual = loaded.items.as_ref().unwrap();
                let expected = expected["items"].as_array().unwrap();
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(expected) {assert_eq!(json!(actual.id),expected["id"]); assert!(matches!(actual.body(),agent_protocol::items::ItemBody::AssistantText {text,..} if text == expected["text"].as_str().unwrap()));}
            }
            assert_eq!(snapshot.panes.primary().error, None);
        }
        store.close().await.unwrap();
        local.endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("history refresh and recovery exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_images_are_visible_on_open_reopen_and_older_pages_without_expanding_work() {
    use agent_core::presentation::conversation::{
        ConversationRowContent, activity_is_expanded, project_conversation,
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aX1sAAAAASUVORK5CYII=";
        let turns = (0..12).map(|index| json!({"id":format!("turn-{index}"),"status":"completed","items":[
            {"id":format!("user-{index}"),"type":"userMessage","content":[{"type":"text","text":"Generate images"}]},
            {"id":format!("command-{index}"),"type":"commandExecution","command":"generate","status":"completed","aggregatedOutput":"hidden output\n".repeat(1000)},
            {"id":format!("saved-{index}"),"type":"imageGeneration","status":"completed","savedPath":format!("/generated-{index}.png"),"result":data},
            {"id":format!("inline-{index}"),"type":"imageGeneration","status":"completed","result":data},
            {"id":format!("answer-{index}"),"type":"agentMessage","phase":"final_answer","text":"Here are the images"}
        ]})).collect::<Vec<_>>();
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([{
            "id":"images","cwd":directory.path(),"historyMode":"paginated","turns":turns
        }])).unwrap()).unwrap();
        let program = host_fixture::fixture::Config { trace: true, ..Default::default() }
            .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), directory.path()).unwrap();
        let fixture = HostFixture::start(directory.path(), AppServerConfig { program, ..Default::default() },
            Arc::new(Memory::default()), "isolated Host", false, None, None).await.unwrap();
        let local = fixture.local().await.unwrap();
        let _catalog=conversations::Catalog::open(&local.peer).await;
        let session = SessionRef::new(ProviderKind::Codex, "images".into()).unwrap();
        for _ in 0..2 {
            let mut opened=conversations::Conversation::open(&local.peer,&session).await.unwrap();
            assert_eq!(opened.view().thread.turns.as_ref().unwrap().len(),10);
            let response=opened.extend(&local.peer).await;
            let rendered = project_conversation(agent_core::localization::UiLanguage::Japanese, &Default::default(), &Default::default(), Arc::new(response.thread), &None);
            assert_eq!(rendered.turns.len(), 12);
            for (index, turn) in rendered.turns.iter().enumerate() {

                let images: Vec<_> = turn.rows.iter().filter_map(|row| match &row.content {
                    ConversationRowContent::Response { item, .. } if !item.data.image_sources.is_empty() => Some(item),
                    ConversationRowContent::ActivityHeader { activity } => {
                        assert!(!activity_is_expanded(&activity.status, None));
                        None
                    }
                    ConversationRowContent::Activity { .. } => panic!("work details were loaded"),
                    _ => None,
                }).collect();
                assert_eq!(images.len(), 2, "images must be visible before work is expanded");
                assert_eq!(images[0].data.image_sources, [format!("/generated-{index}.png")]);
                assert_eq!(images[1].data.image_sources, [format!("data:image/png;base64,{data}")]);
            }
        }
        let trace = std::fs::read_to_string(directory.path().join("rpc-trace.jsonl")).unwrap();
        assert!(trace.lines().all(|line| serde_json::from_str::<Value>(line).unwrap()["method"] != "thread/items/list"),
            "showing images must not require expanding or hydrating work");
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("generated image history exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summary_history_rejects_repeated_cursors_and_oversized_pages() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let valid = |id: &str| json!({"id":id,"status":"completed","itemsView":"full","items":[]});
        let cases = [
            ("oversized", json!({"data":(0..11).map(|id| valid(&id.to_string())).collect::<Vec<_>>(),"nextCursor":null}), "turn page exceeds requested size"),
            ("not-full", json!({"data":[{"id":"turn","itemsView":"summary","items":[]}],"nextCursor":null}), "turn summary is missing"),
            ("missing-turn", json!({"data":[valid("")],"nextCursor":null}), "turn summary is missing"),
            ("missing-item", json!({"data":[{"id":"turn","itemsView":"full","items":[{"id":"","type":"agentMessage","text":"answer"}]}],"nextCursor":null}), "turn summary is missing"),
        ];
        let mut threads: Vec<_> = cases.iter().map(|(id, response, _)| json!({
            "id":id,"cwd":directory.path(),"historyMode":"paginated","fixtureHistoryResponse":response,
            "turns":[{"id":"turn","status":"completed","items":[{"id":"answer","type":"agentMessage","text":"saved answer"}]}]
        })).collect();
        threads.push(json!({"id":"loop","cwd":directory.path(),"historyMode":"paginated",
            "fixtureHistoryResponse":{"data":[],"nextCursor":"opaque-loop"},
            "turns":[{"id":"turn","status":"completed","items":[{"id":"answer","type":"agentMessage","text":"saved"}]}]}));
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        let fixture = start_host(directory.path()).await;
        let local = fixture.local().await.unwrap();
        let _catalog=conversations::Catalog::open(&local.peer).await;
        for (id, _, message) in cases {
            let result=conversations::Conversation::open(&local.peer,&SessionRef::new(ProviderKind::Codex,id.into()).unwrap()).await;
            let thread = result.unwrap().view().thread;
            let failure = thread.history_read_state.unwrap();
            assert_eq!(failure.kind, agent_protocol::session::HistoryReadKind::Unavailable, "invalid summary was accepted: {id}");
            assert_eq!(failure.issues, [message]);
            assert!(thread.turns.unwrap().is_empty());
        }
        let failure=conversations::Conversation::open(&local.peer,&SessionRef::new(ProviderKind::Codex,"loop".into()).unwrap()).await.unwrap().view().thread.history_read_state.unwrap();
        assert_eq!(failure.kind,agent_protocol::session::HistoryReadKind::Unavailable);
        assert!(failure.issues[0].contains("history cursor repeated"));
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("summary validation exceeded its deadline");
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
        let _catalog=conversations::Catalog::open(&local.peer).await;
        let opened=conversations::Conversation::open(&local.peer,&SessionRef::new(ProviderKind::Codex,"command-history".into()).unwrap()).await.unwrap();
        let bytes=agent_protocol::protocol::encode(opened.replica.snapshot().unwrap()).unwrap().len();
        assert!(bytes < 100 * 1024, "collapsed command bodies delayed history: {bytes} bytes");
        let view=opened.view();
        let turn=&view.thread.turns.as_ref().unwrap()[0];
        let loaded = turn.items.as_ref().unwrap();
        // Imported history keeps the conversation's messages, not past commands.
        assert_eq!(loaded.len(), 2);

        assert_eq!(item_text(&loaded[1]), Some("The build passed"));
        assert!(loaded.iter().all(|item|!item.is_deferred()));
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("command history exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_history_loads_conversation_before_lossless_item_details() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("bex-projects.json"),
            json!([{"id":"workspace", "name":"Workspace", "roots":[{"path":directory.path()}]}])
                .to_string(),
        )
        .unwrap();
        std::fs::write(directory.path().join("list-fixture.json"), json!([
            {"id":"large","cwd":directory.path().join("large-history"),"historyMode":"paginated"},
            {"id":"long","cwd":directory.path(),"historyMode":"paginated","fixtureHistory":"long"}
        ]).to_string()).unwrap();
        let fixture = start_host(directory.path()).await;
        let mobile = fixture.local().await.unwrap();
        let mut catalog = conversations::Catalog::open(&mobile.peer).await;
        catalog.wait_rows(2).await;
        let id = SessionRef::new(ProviderKind::Codex, "large".into()).unwrap();
        let page = catalog.page(Default::default());
        let listed = &page.lists.project_threads["workspace"];
        let listed = listed
            .data
            .iter()
            .find(|thread| thread.id.as_ref() == Some(&id))
            .unwrap();
        assert_eq!(listed.project_id.as_deref(), Some("workspace"));
        let preview = conversations::Conversation::open(&mobile.peer, &id)
            .await
            .unwrap();
        let preview_bytes = agent_protocol::protocol::encode(preview.replica.snapshot().unwrap())
            .unwrap()
            .len();
        assert!(
            preview_bytes < 16 * 1024,
            "past command output must not delay messages"
        );
        let view = preview.view();
        assert_eq!(view.thread.project_id.as_deref(), Some("workspace"));
        assert_eq!(view.thread.capabilities, listed.capabilities);
        let items = view.thread.turns.as_ref().unwrap()[0]
            .items
            .as_ref()
            .unwrap();
        assert_eq!(items.len(), 2, "past tool activity stays native");
        assert_eq!(item_text(&items[0]), Some("Read the whole output"));
        assert_eq!(item_text(&items[1]), Some("Large history is complete"));

        let history_id = SessionRef::new(ProviderKind::Codex, "long".into()).unwrap();
        let mut history = conversations::Conversation::open(&mobile.peer, &history_id)
            .await
            .unwrap();
        assert_eq!(history.view().thread.turns.as_ref().unwrap().len(), 10);
        history.extend(&mobile.peer).await;
        let history = history.view();
        let turns = history.thread.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 15);
        let items: Vec<_> = turns
            .iter()
            .flat_map(|turn| turn.items.iter().flatten())
            .collect();
        assert_eq!(
            items
                .iter()
                .map(|item| &item.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            items.len()
        );
        assert!(items.iter().all(|item| !matches!(
            item.body(),
            agent_protocol::items::ItemBody::CommandExecution { .. }
        )));

        let started = conversations::created(
            &mobile.peer,
            conversations::create_request(
                ProviderKind::Codex,
                Some(directory.path().to_str().unwrap().into()),
                None,
            ),
        )
        .await
        .unwrap();
        let image_id = started.thread.id.unwrap();
        let mut images = conversations::Conversation::open(&mobile.peer, &image_id)
            .await
            .unwrap();
        conversations::submit(
            &mobile.peer,
            &rpc::Submission {
                thread_id: image_id,
                client_user_message_id: next_submission_id().into(),
                input: vec![rpc::Input::Text {
                    text: "[generated-images]".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        )
        .await
        .unwrap();
        let turn = images.finished().await;
        let items: Vec<_> = turn
            .items
            .iter()
            .flatten()
            .filter(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::ImageGeneration { .. }
                )
            })
            .collect();
        assert_eq!(items.len(), 2);
        let original = std::fs::read(directory.path().join("fixture image.png")).unwrap();
        for item in items {
            let (full, _, _) = images
                .hydrate(&mobile.peer, &mobile.session, &item.id)
                .await;
            let agent_protocol::items::ItemBody::ImageGeneration {
                data: Some(data), ..
            } = full.body()
            else {
                panic!("image body missing")
            };
            assert_eq!(STANDARD.decode(data).unwrap(), original);
        }
        mobile.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("large history loop exceeded deadline");
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
            json!({"id":"explicit","projectId":"ignored-native-assignment","cwd":directory.path().join("project-3"),"name":"Explicit assignment","updatedAt":90000}),
            json!({"id":"projectless","projectId":"ignored-native-assignment","cwd":directory.path().join("bex-chats"),"name":"Explicit chat","updatedAt":90001}),
            json!({"id":"worktree","projectId":"ignored-native-assignment","cwd":directory.path().join("worktree"),"name":"Worktree conversation","updatedAt":90002}),
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
        let program = host_fixture::fixture::Config { trace: true, stream_delay_ms: 5, ..Default::default() }
            .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), directory.path()).unwrap();
        let fixture = HostFixture::start(directory.path(), AppServerConfig { program, ..Default::default() }, Arc::new(Memory::default()), "isolated Host", false, None, None).await.unwrap();
        let mobile = fixture.local().await.unwrap();

        use agent_core::state::{Event, Intent, reduce};
        let query = models::ListQuery { limit:5, project_limit:5, ..Default::default() };
        let mut catalog = conversations::Catalog::open(&mobile.peer).await;
        catalog.wait_rows(threads.len()).await;
        let mut initial = catalog.page(query.clone());
        assert_eq!(initial.thread_list().unwrap().threads.len(), 5);
        let projects = initial.lists.threads.as_ref().unwrap().projects.iter().map(|project|project.id.clone()).collect::<Vec<_>>();
        for project_id in projects {
            initial = reduce(&initial, "", Event::Intent(Intent::SetProjectExpanded { project_id, expanded: true })).0;
        }
        let first = initial.lists.threads.as_ref().unwrap();
        let displayed = initial.thread_list().unwrap();
        assert_eq!(displayed.threads.len() + displayed.projects.iter().map(|project| project.threads.len()).sum::<usize>(), 30);
        // The shared catalog carries one small header per native row; bodies
        // remain behind the conversation subscription/hydration boundary.
        assert!(catalog.max_wire_bytes < threads.len() * 1024 + 16 * 1024);
        assert!(catalog.replica.rows().values().all(|row| agent_protocol::protocol::encode(row).unwrap().len() < 1024));
        assert_eq!(first.projects.len(), 5);
        assert!(first.has_more && first.has_more_projects);
        assert_eq!(first.data.iter().map(|thread| thread.id.as_ref().unwrap().id.as_str()).collect::<Vec<_>>(), ["projectless", "chat-18", "chat-17", "chat-16", "chat-15"]);
        assert_eq!(first.projects.iter().map(|project| project.id.as_str()).collect::<Vec<_>>(), ["project-5", "project-3", "project-7", "project-6", "project-4"]);
        assert!(first.data.iter().all(|thread| thread.turns.is_none() && thread.preview.is_none()));
        let project_page = |project: &str, limit| {
            let (all, _) = reduce(&initial, "", Event::Intent(Intent::ExpandProjects));
            let (opened, _) = reduce(&all, "", Event::Intent(Intent::SetProjectExpanded { project_id:project.into(), expanded:true }));
            let (page, _) = reduce(&opened, "", Event::Intent(Intent::ListProjectSessions(op::ListProjectSessions { project_id:project.into(),limit,search_term:String::new() })));
            page.lists.project_threads[project].clone()
        };
        let more = project_page("project-5", 5);
        assert_eq!(more.data.len(), 5);
        assert_eq!(more.data[0].id.as_ref().unwrap().id, "worktree");
        assert_eq!(more.data[4].id.as_ref().unwrap().id, "p5-15");
        assert!(more.has_more);
        assert!(more.data.iter().all(|thread| thread.project_id.as_deref() == Some("project-5") && thread.turns.is_none()));
        let expanded = project_page("project-5", 15);
        assert_eq!(expanded.data.len(), 15);
        assert_eq!(expanded.data[14].id.as_ref().unwrap().id, "p5-5");
        assert!(expanded.has_more);
        let end = project_page("project-5", 25);
        assert_eq!(end.data.len(), 19);
        assert!(!end.has_more);
        let old = project_page("project-1", 5);
        assert_eq!(old.data[0].id.as_ref().unwrap().id, "p1-18");
        assert!(old.has_more);
        let found = mobile.peer.call(&agent_protocol::conversation::records::SearchList { term:"Project 01".into(),limit: Some(10_000) }).await.unwrap();
        assert_eq!(found.data.len(),18);
        assert!(found.data.iter().all(|thread| thread.project_id.as_deref() == Some("project-1")));
        assert_eq!(found.projects.len(),1);
        let filtered = catalog.page(models::ListQuery { search_term:"Project 01".into(), ..query.clone() });
        assert_eq!(filtered.lists.project_threads["project-1"].data.len(),5);
        assert!(!filtered.lists.threads.as_ref().unwrap().has_more_projects);
        let exact = mobile.peer.call(&agent_protocol::conversation::records::SearchList { term:"Project 01 conversation 18".into(),limit: Some(10_000) }).await.unwrap();
        assert_eq!(exact.data.len(),1);
        assert_eq!(exact.data[0].id.as_ref().unwrap().id,"p1-18");
        assert!(!exact.has_more);
        let body = open_conversation(&mobile.peer, &json!({"provider":"codex","id":"p5-1"})).await.0;
        assert_eq!(body["thread"]["projectId"], json!({"Assigned":"project-5"}));
        assert_eq!(body_json(&body["thread"]["turns"][0]["items"][0])["assistantText"]["text"], "History for Project 05 conversation 01");
        assert_eq!(body["thread"]["status"], "unknown");
        assert!(agent_protocol::protocol::json_boundary::call("host/thread/watch", json!({"watchId":1,"threadId":{"provider":"codex","id":"p5-1"},"path":rollout})).is_err());
        threads.extend([
            json!({"id":"older-child","parentThreadId":"worktree","cwd":directory.path().join("worktree"),"name":null,"agentNickname":"Curie","canAcceptDirectInput":false,"updatedAt":-1}),
            json!({"id":"older-grandchild","parentThreadId":"older-child","name":null,"agentRole":"reviewer","updatedAt":-2}),
            json!({"id":"hidden-child","parentThreadId":"p1-1","updatedAt":-3}),
        ]);
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        let native_reads = || std::fs::read_to_string(directory.path().join("rpc-trace.jsonl")).unwrap().lines().map(|line| serde_json::from_str::<Value>(line).unwrap()["method"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        assert!(!native_reads().iter().any(|method| method == "project/list"));
        let before = native_reads().len();
        let agents = mobile.peer.call(&rpc::ListAgents { thread_id:SessionRef::new(ProviderKind::Codex,"worktree".into()).unwrap() }).await.unwrap();
        assert_eq!(&native_reads()[before..],["thread/list"],"fleet discovery must not read history");
        let child = agents.iter().find(|agent| agent.id.id == "older-child").unwrap();
        assert_eq!(child.parent_id.id,"worktree");
        assert_eq!(child.name.as_deref(),Some("Curie"));
        assert_eq!(agents.iter().map(|agent| agent.id.id.as_str()).collect::<Vec<_>>(),["older-child","older-grandchild"]);
        let child_body = open_conversation(&mobile.peer,&json!({"provider":"codex","id":"older-child"})).await.0;
        assert_eq!(child_body["thread"]["canAcceptDirectInput"],false);
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
            let output = std::process::Command::new("git")
                .current_dir(&workspace)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "--quiet"]);
        std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
        git(&["add", "tracked.txt"]);
        git(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ]);
        std::fs::write(workspace.join(".env"), "FIXTURE_VALUE=isolated\n").unwrap();
        let project_state = root.join("bex-projects.json");
        std::fs::write(
            &project_state,
            serde_json::to_vec(
                &json!([{"id":"workspace","name":"Workspace","roots":[{"path":workspace}]}]),
            )
            .unwrap(),
        )
        .unwrap();
        let fixture = start_host(&root).await;
        let local = fixture.local().await.unwrap();
        let initial = completed_chat(&local.peer, Some(workspace.to_str().unwrap().into())).await;
        assert_eq!(initial.cwd.as_deref(), workspace.to_str());
        let destination = root.join("worktree storage");
        let settings = models::WorktreeSettings {
            create_on_new_session: true,
            copy_on_create: true,
            copy_paths: vec![".env".into()],
            worktree_directory: destination.to_str().unwrap().into(),
            ..Default::default()
        };
        assert_eq!(
            local
                .peer
                .call(&op::UpdateWorktreeSettings {
                    settings: settings.clone()
                })
                .await
                .unwrap(),
            settings
        );
        let mut ids = Vec::new();
        let mut paths = Vec::new();
        for _ in 0..2 {
            let thread =
                completed_chat(&local.peer, Some(workspace.to_str().unwrap().into())).await;
            let cwd = PathBuf::from(thread.cwd.unwrap());
            assert_ne!(cwd, workspace);
            assert_project_directory(&cwd, &workspace, Some(&destination));
            assert_eq!(
                std::fs::read(cwd.join(".env")).unwrap(),
                std::fs::read(workspace.join(".env")).unwrap()
            );
            assert_eq!(thread.project_id.as_deref(), Some("workspace"));
            ids.push(thread.id.unwrap());
            paths.push(cwd);
        }
        assert_ne!(paths[0], paths[1]);
        let before = std::process::Command::new("git")
            .current_dir(&workspace)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap()
            .stdout;
        for id in &ids {
            let read = conversations::Conversation::open(&local.peer, id)
                .await
                .unwrap()
                .view();
            assert_eq!(read.thread.project_id.as_deref(), Some("workspace"));
        }
        let mut chat_ids = Vec::new();
        for cwd in [None, Some("  ".into()), Some(String::new())] {
            let thread = completed_chat(&local.peer, cwd).await;
            assert_eq!(thread.cwd.as_deref(), root.join("bex-chats").to_str());
            assert!(thread.project_id.is_none());
            chat_ids.push(thread.id.unwrap());
        }
        let memory = fixture.memory.clone();
        local.close().await;
        fixture.close().await.unwrap();
        let fixture = HostFixture::start(
            &root,
            codex_fixture::config(&root),
            memory,
            "isolated Host",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let local = fixture.local().await.unwrap();
        assert_eq!(
            local.peer.call(&op::ReadWorktreeSettings {}).await.unwrap(),
            settings
        );
        for id in &chat_ids {
            let thread = conversations::Conversation::open(&local.peer, id)
                .await
                .unwrap()
                .view()
                .thread;
            assert_eq!(thread.cwd.as_deref(), root.join("bex-chats").to_str());
            assert!(thread.project_id.is_none());
        }
        let catalog = conversations::Catalog::open(&local.peer).await;
        let listed = catalog.page(models::ListQuery {
            limit: 30,
            ..Default::default()
        });
        let project = &listed.lists.project_threads["workspace"];
        assert!(ids.iter().all(|id| {
            project
                .data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(id))
        }));
        assert!(chat_ids.iter().all(|id| {
            listed
                .lists
                .threads
                .as_ref()
                .unwrap()
                .data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(id) && thread.project_id.is_none())
        }));
        let after = std::process::Command::new("git")
            .current_dir(&workspace)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .unwrap()
            .stdout;
        assert_eq!(
            before, after,
            "opening and listing must not create worktrees"
        );
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("worktree integration exceeded 40 seconds");
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
        assert_eq!(original["text"], "first\r\n");
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
        let started = conversations::created(&local.peer,conversations::create_request(ProviderKind::Codex,Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),None)).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let thread_id = &started["thread"]["id"];

        let (_, mut events) = open_conversation(&local.peer, thread_id).await;
        conversations::submit(&local.peer,&serde_json::from_value::<rpc::Submission>(json!({"threadId":thread_id,"clientUserMessageId":next_submission_id(),"input":[{"text":{"text":"[items]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        completed_turn(&mut events).await;
        let catalog = conversations::Catalog::open(&local.peer).await;
        let list = catalog.page(Default::default()).lists.threads.unwrap();
        let history = open_conversation(&local.peer, thread_id).await.0;
        assert_eq!(list.projects[0].roots[0].path, directory.path().to_str().unwrap());
        assert!(history["thread"]["turns"][0]["items"].as_array().unwrap().iter().any(|item| item["body"]["inline"]["body"]["toolCall"]["result"].is_object()));
        let reopened = conversations::Conversation::open(&local.peer, &serde_json::from_value(thread_id.clone()).unwrap()).await.unwrap().view().thread;
        let tool_ids = reopened.turns.iter().flatten().flat_map(|turn| turn.items.iter().flatten()).filter(|item| matches!(item.body(), agent_protocol::items::ItemBody::ToolCall { kind: agent_protocol::items::ToolKind::Mcp | agent_protocol::items::ToolKind::Dynamic, .. })).map(|item| item.id.clone()).collect::<std::collections::HashSet<_>>();
        let snapshot = agent_core::state::Snapshot::default();
        let rendered = agent_core::presentation::conversation::project_conversation(
            agent_core::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(reopened), &None,
        );
        let tools: Vec<_> = rendered.turns[0].rows.iter().filter_map(|row| match &row.content {
            agent_core::presentation::conversation::ConversationRowContent::Activity { item, .. } => Some(item),
            _ => None,
        }).filter(|item| item.data.native_id.as_ref().is_some_and(|id| tool_ids.contains(id))).collect();
        assert_eq!(tools.len(), 2);
        for tool in tools {
            let blocks = tool.detail_blocks();
            let images: Vec<_> = blocks.iter().filter_map(|block| match block {
                agent_core::presentation::markdown::MarkdownBlock::Paragraph { runs, .. } => Some(runs), _ => None,
            }).flatten().filter_map(|run| run.image.as_deref()).collect();
            assert_eq!(images.len(), 1);
            let bytes = STANDARD.decode(images[0].strip_prefix("data:image/png;base64,").unwrap()).unwrap();
            let decoded = png::Decoder::new(std::io::Cursor::new(bytes)).read_info().unwrap();
            assert_eq!((decoded.info().width, decoded.info().height), (1, 1));
        }
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
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .map(|output| conversations::list_ids(output.initial))
                .unwrap(),
            Vec::<SessionRef>::new()
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
        let store = Store::new((local.peer, local.events), Snapshot::default(), None);
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        assert!(store.snapshot().connection.connected);
        std::fs::write(directory.path().join("exit-on-list"), "").unwrap();
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        let mut updates = store.subscribe();
        updates
            .wait_for(|snapshot| {
                snapshot.lists.threads.as_ref().is_some_and(|list| {
                    list.provider_errors
                        .as_ref()
                        .is_some_and(|errors| errors.contains_key("codex"))
                })
            })
            .await
            .unwrap();
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
        assert!(store.snapshot().connection.connected);
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
        std::fs::remove_file(directory.path().join(format!(
            "agent-codex-fixture{}",
            std::env::consts::EXE_SUFFIX
        )))
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
        let status = local.peer.call(&rpc::ReadHostStatus {}).await.unwrap();
        assert!(status.devices.is_empty());
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
        let started = conversations::created(&sender.peer,conversations::create_request(ProviderKind::Codex,Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),None)).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();

        let id = &started["thread"]["id"];
        let (_, mut sender_events) = open_conversation(&sender.peer, id).await;
        conversations::submit(&sender.peer,&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[approval]"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let original = next_request(&mut sender_events).await;
        sender.peer.close().await;
        let passive = fixture.local().await.unwrap();

        let (opened, mut events) = open_conversation(&passive.peer, id).await;
        let request = opened["thread"]["requests"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap();
        assert_eq!(request["id"], original["id"]);
        tokio::time::sleep(delay).await;
        conversations::command(&passive.peer,&serde_json::from_value(id.clone()).unwrap(),agent_protocol::conversation::Command::Answer {
            id:serde_json::from_value(request["id"].clone()).unwrap(),
            answer:agent_protocol::requests::Answer::Approval {choice_id:request["body"]["approval"]["choices"][0]["id"].as_str().unwrap().into()},
        }).await.unwrap();
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
        // Exceed the former shared 64-slot limit while keeping connections alive.
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
                .call(&agent_protocol::conversation::records::SubscribeList {
                    refresh: false,
                    subscription: uuid::Uuid::new_v4(),
                    cached: None
                })
                .await
                .map(|output| conversations::list_ids(output.initial))
                .unwrap(),
            Vec::<SessionRef>::new()
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
    use agent_local_host::{LocalHostRegistry, LocalHostState};
    use agent_protocol::requests::Answer;
    use host_daemon::{FileKeyStore, HostRuntime};

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
            [host_daemon::providers::codex::Codex::new(Ok(server.clone())).into()],
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
        let running = tokio::spawn(runtime.clone().run(stop.clone(), Duration::from_secs(60)));

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
            agent_local_host::load_local_identity(&location.directory).unwrap(),
            Relays::Disabled,
        )
        .await
        .unwrap();
        let mut desktop = Store::connect(
            &desktop_endpoint,
            &ticket,
            Snapshot::default(),
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        desktop
            .dispatch(Intent::CreateInvitation(op::CreateInvitation {}))
            .await
            .unwrap();
        let expires_at = desktop
            .snapshot()
            .connection
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
            .connection
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
            codex_chat_snapshot(),
            Some(invitation),
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        mobile
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();

        for (index, final_status) in ["completed", "interrupted"].into_iter().enumerate() {
            let previous_id = mobile
                .snapshot()
                .panes
                .primary()
                .navigation
                .thread_id
                .clone();
            let draft_key = mobile
                .snapshot()
                .panes
                .primary()
                .navigation
                .draft_key
                .clone();
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
                    client_user_message_id: input_id(&format!("shared-{index}")),
                    alternate: false,
                })
                .await
                .unwrap();
            let id = mobile
                .snapshot()
                .panes
                .primary()
                .navigation
                .thread_id
                .clone()
                .unwrap();
            wait_for(&mobile, |snapshot| snapshot.requests().next().is_some()).await;
            desktop
                .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                .await
                .unwrap();
            let active = |snapshot: &Snapshot| {
                snapshot
                    .sessions
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
                "Fixture device",
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
            // The fixture interrupts the turn for any answer that does not allow it.
            let effect = if final_status == "completed" {
                agent_protocol::requests::ChoiceEffect::Allow
            } else {
                agent_protocol::requests::ChoiceEffect::Deny
            };
            let choice_id = request
                .body
                .choices()
                .iter()
                .find(|choice| choice.effect == effect)
                .unwrap()
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
                    snapshot.sessions.conversations[&id]
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
                assert!(snapshot.panes.primary().error.is_none());
                let turns = snapshot.sessions.conversations[&id].turns.as_ref().unwrap();
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
            assert!(mobile.snapshot().sessions.pending_submissions.is_empty());
            let snapshot = mobile.snapshot();
            assert!(
                snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
            );
            assert!(
                snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
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
                let program = host_fixture::fixture::Config { trace: true, deferred_thread_metadata: true, stream_delay_ms: 10, ..Default::default() }
                    .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), &root).unwrap();
                let fixture = HostFixture::start(&root, AppServerConfig { program, ..Default::default() }, Arc::new(Memory::default()), "isolated", false, None, None).await.unwrap();
                let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
                let store = Store::connect(&endpoint, &fixture.ticket, codex_chat_snapshot(), None, "Fixture device", None).await.unwrap();
                let observer = Store::connect(&endpoint, &fixture.ticket, codex_chat_snapshot(), None, "Fixture device", None).await.unwrap();
                for client in [&store, &observer] {
                    let mut initial = client.subscribe();
                    while initial.borrow_and_update().lists.threads.is_none() || !initial.borrow().operations.is_empty() { initial.changed().await.unwrap(); }
                }
                let list_reads = || std::fs::read_to_string(root.join("rpc-trace.jsonl")).unwrap().lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).filter(|entry| entry["method"] == "thread/list").count();
                let before = list_reads();
                store.dispatch(Intent::NewChat { cwd: if scoped { project.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                if scoped {
                    store.dispatch(Intent::SetProjectExpanded { project_id: "project".into(), expanded: true }).await.unwrap();
                }
                let key = store.snapshot().panes.primary().navigation.draft_key.clone();
                store.dispatch(Intent::SetDraftText { thread_id: key, text: "[success] list automatically".into() }).await.unwrap();
                store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: input_id("sidebar-message"), alternate: false }).await.unwrap();
                let id = store.snapshot().panes.primary().navigation.thread_id.clone().unwrap();
                let mut updates = store.subscribe();
                let reflected = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let snapshot = updates.borrow_and_update().clone();
                        let complete = snapshot.sessions.conversations[&id].turns.as_ref().is_some_and(|turns| turns.first().is_some_and(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed));
                        let listed = snapshot.thread_list().is_some_and(|list| list.threads.iter().chain(list.projects.iter().flat_map(|project| &project.threads)).any(|thread| thread.id == id && thread.title == "Completed conversation" && thread.project_id == scoped.then(|| "project".into())));
                        if complete && listed {
                            assert!(snapshot.sessions.pending_submissions.is_empty());
                            assert!(snapshot.panes.primary().error.is_none(), "{:?}", snapshot.panes.primary().error);
                            assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty());
                            if scoped {
                                assert_project_directory(Path::new(&snapshot.selected_directory(snapshot.panes.primary())), &project, automatic.then_some(project.join(".worktree").as_path()));
                            }
                            break;
                        }
                        updates.changed().await.unwrap();
                    }
                }).await;
                let mut observed = observer.subscribe();
                tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let snapshot = observed.borrow_and_update().clone();
                        if snapshot.lists.threads.iter().flat_map(|page| &page.data).chain(snapshot.lists.project_threads.values().flat_map(|page| &page.data)).any(|row| row.id.as_ref() == Some(&id) && row.name.as_deref() == Some("Completed conversation")) { break; }
                        observed.changed().await.unwrap();
                    }
                }).await.expect("passive client must receive metadata without a list refresh");
                assert_eq!(list_reads(), before, "creation, submission, and rename must not fetch lists");
                observer.close().await.unwrap();
                store.close().await.unwrap();
                endpoint.close().await;
                fixture.close().await.unwrap();
                assert!(reflected.is_ok(),
                    "completed conversation and title did not appear automatically (worktree={automatic}, scoped={scoped}); turns={:?}; list={:?}",
                    store.snapshot().sessions.conversations[&id].turns.as_ref().map(|turns| turns.iter().map(|turn| &turn.status).collect::<Vec<_>>()),
                    store.snapshot().thread_list().map(|list| list.threads.iter().chain(list.projects.iter().flat_map(|project| &project.threads)).map(|thread| (thread.id.clone(), thread.title.clone(), thread.project_id.clone())).collect::<Vec<_>>()));
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
        let store = Store::connect(&endpoint, &fixture.ticket, codex_chat_snapshot(), None, "Fixture device", None).await.unwrap();
        store.dispatch(Intent::UpdateWorktreeSettings(op::UpdateWorktreeSettings { settings: models::WorktreeSettings { create_on_new_session: true, ..Default::default() } })).await.unwrap();
        store.dispatch(Intent::NewChat { cwd: project.to_str().unwrap().into() }).await.unwrap();
        let key = store.snapshot().panes.primary().navigation.draft_key.clone();
        store.dispatch(Intent::SetDraftText { thread_id: key, text: "[success] [delayed-input] keep running".into() }).await.unwrap();
        store.dispatch(Intent::Submit { thread_id: None, client_user_message_id: input_id("managed"), alternate: false }).await.unwrap();
        let id = store.snapshot().panes.primary().navigation.thread_id.clone().unwrap();
        let mut updates = store.subscribe();
        let turn_id = loop {
            let snapshot = updates.borrow_and_update().clone();
            if let Some(turn) = snapshot.sessions.conversations[&id].turns.as_ref().and_then(|turns| turns.first()) { break turn.id.clone(); }
            updates.changed().await.unwrap();
        };
        let path = store.snapshot().panes.primary().navigation.cwd.clone();
        store.dispatch(Intent::ListWorktrees(op::ListWorktrees {})).await.unwrap();
        let snapshot = store.snapshot();
        let entries = snapshot.files.workspace.worktrees.as_ref().unwrap();
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
            if snapshot.sessions.conversations[&id].turns.as_ref().is_some_and(|turns| turns[0].status.label() == "interrupted") { break; }
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
        local.peer.call(&rpc::StartTerminal { command: None, handle: "managed-terminal".into(), cwd: path.clone(), size: rpc::TerminalSize { rows: 24, cols: 80 } }).await.unwrap();
        assert!(store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.is_err());
        local.peer.request::<models::Empty>(&agent_protocol::protocol::Call::KillTerminal(serde_json::from_value::<rpc::TerminalKill>(json!({"processHandle":"managed-terminal"})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
        store.dispatch(Intent::RemoveWorktree(op::RemoveWorktree { path: path.clone() })).await.unwrap();
        assert!(!Path::new(&path).exists());
        assert_eq!(store.snapshot().files.workspace.worktrees.as_ref().unwrap().len(), 1);
        store.disconnect().await.unwrap();
        store.reconnect(&endpoint, &fixture.ticket, None, "Fixture device").await.unwrap();
        store.dispatch(Intent::ListWorktrees(op::ListWorktrees {})).await.unwrap();
        assert_eq!(store.snapshot().files.workspace.worktrees.as_ref().unwrap().len(), 1);
        assert!(store.snapshot().panes.primary().error.is_none());
        assert!(String::from_utf8(git(&["branch", "--list", "bex/*"])).unwrap().contains("bex/session-"));
        let catalog=conversations::Catalog::open(&local.peer).await;
        assert!(catalog.replica.rows().contains_key(&agent_protocol::sync::Key::Thread(id.clone())), "removal must preserve conversation history");
        let review = local.peer.call(&rpc::ReviewWorkspace { cwd: path.clone(), include_options: false, base_ref: None }).await.unwrap();
        assert!(review.files.is_empty());
        assert!(!Path::new(&path).exists(), "reading history must not recreate the worktree");
        git(&["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--allow-empty", "--quiet", "-m", "advance main"]);
        let mut events=conversations::Conversation::open(&local.peer,&id).await.unwrap();
        conversations::submit(&local.peer,&rpc::Submission {
            thread_id: id.clone(),
            client_user_message_id: input_id("after-removal"),
            input: vec![rpc::Input::Text { text: "[success] continue after removal".into() }],
            model: None,
            effort: None,
            service_tier: None,
            follow_up: Default::default(),
        }).await.unwrap();
        events.input_turn(&input_id("after-removal"),true).await;
        assert!(Path::new(&path).is_dir(), "sending must recreate the checkout");
        assert_eq!(git(&["-C", &path, "rev-parse", "HEAD"]), git(&["rev-parse", "main"]));
        let (opened, _) = open_conversation(&local.peer, &json!(id)).await;
        assert_eq!(opened["thread"]["cwd"], path);
        assert!(opened.to_string().contains("[success] continue after removal"));
        local.close().await;
        store.close().await.unwrap();
        drop(store);
        endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("worktree management exceeded its deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unchanged_worktree_cleanup_defers_live_work_and_rechecks_new_commits() {
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
    async fn removed(path: &Path) {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    tokio::time::timeout(Duration::from_secs(210), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        git(&project, &["init", "--initial-branch=main"]);
        git(&project, &["commit", "--allow-empty", "-m", "base"]);
        let program = host_fixture::fixture::Config {
            deferred_thread_metadata: true,
            ..Default::default()
        }
        .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), &root)
        .unwrap();
        let fixture = HostFixture::start(
            &root,
            AppServerConfig {
                program,
                ..Default::default()
            },
            Arc::new(Memory::default()),
            "isolated",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let local = fixture.local().await.unwrap();
        let mut settings = models::WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        };
        local
            .peer
            .call(&op::UpdateWorktreeSettings {
                settings: settings.clone(),
            })
            .await
            .unwrap();
        let mut checkouts = Vec::new();
        for name in ["running", "terminal", "dirty", "clean"] {
            let response =
                completed_chat(&local.peer, Some(project.to_str().unwrap().into())).await;
            let id = response.id.unwrap();
            let path = PathBuf::from(response.cwd.unwrap());
            git(&path, &["commit", "--allow-empty", "-m", name]);
            let branch = git(&path, &["branch", "--show-current"]);
            git(&project, &["merge", "--ff-only", &branch]);
            checkouts.push((id, path, branch));
        }
        let (active, active_path, active_branch) = &checkouts[0];
        let mut events = conversations::Conversation::open(&local.peer, active)
            .await
            .unwrap();
        conversations::submit(
            &local.peer,
            &rpc::Submission {
                thread_id: active.clone(),
                client_user_message_id: input_id("cleanup-running"),
                input: vec![rpc::Input::Text {
                    text: "[request] keep running until interrupted".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        )
        .await
        .unwrap();
        events.input_turn(&input_id("cleanup-running"), false).await;
        let catalog = conversations::Catalog::open(&local.peer).await;
        assert!(
            catalog
                .replica
                .rows()
                .contains_key(&agent_protocol::sync::Key::Thread(active.clone()))
        );
        let terminal_path = &checkouts[1].1;
        local
            .peer
            .call(&rpc::StartTerminal {
                command: None,
                handle: "cleanup-terminal".into(),
                cwd: terminal_path.to_str().unwrap().into(),
                size: rpc::TerminalSize { rows: 24, cols: 80 },
            })
            .await
            .unwrap();
        local
            .peer
            .call(&rpc::DetachTerminal {
                handle: "cleanup-terminal".into(),
            })
            .await
            .unwrap();
        let dirty_path = &checkouts[2].1;
        std::fs::write(dirty_path.join("untracked.txt"), "preserve local data").unwrap();
        assert!(
            checkouts.iter().all(|(_, path, _)| path.exists()),
            "cleanup defaults to disabled"
        );
        settings.delete_unchanged = true;
        local
            .peer
            .call(&op::UpdateWorktreeSettings { settings })
            .await
            .unwrap();
        removed(&checkouts[3].1).await;
        assert!(active_path.exists(), "a merged running turn must stay");
        assert!(
            terminal_path.exists(),
            "a detached terminal still owns its worktree"
        );
        assert_eq!(
            std::fs::read_to_string(dirty_path.join("untracked.txt")).unwrap(),
            "preserve local data"
        );
        // Commit new work while the turn still owns the checkout, before
        // cleanup can remove it.
        std::fs::write(active_path.join("new-work.txt"), "new work after merge").unwrap();
        git(active_path, &["add", "new-work.txt"]);
        git(active_path, &["commit", "-m", "new work after merge"]);
        conversations::command(
            &local.peer,
            active,
            agent_protocol::conversation::Command::Interrupt { turn: None },
        )
        .await
        .unwrap();
        assert_eq!(completed_turn(&mut events).await["status"], "interrupted");
        local
            .peer
            .call(&rpc::StartTerminal {
                command: None,
                handle: "cleanup-terminal".into(),
                cwd: terminal_path.to_str().unwrap().into(),
                size: rpc::TerminalSize { rows: 24, cols: 80 },
            })
            .await
            .unwrap();
        local
            .peer
            .request::<models::Empty>(&agent_protocol::protocol::Call::KillTerminal(
                rpc::TerminalKill {
                    process_handle: "cleanup-terminal".into(),
                },
            ))
            .await
            .unwrap();
        std::fs::remove_file(dirty_path.join("untracked.txt")).unwrap();
        removed(terminal_path).await;
        removed(dirty_path).await;
        assert!(
            active_path.exists(),
            "completion must not remove a new unmerged commit"
        );
        git(
            &project,
            &[
                "merge",
                "--no-ff",
                "-m",
                "merge additional work",
                active_branch,
            ],
        );
        removed(active_path).await;
        for (id, path, branch) in &checkouts {
            let (opened, _) = open_conversation(&local.peer, &json!(id)).await;
            assert_eq!(
                opened["thread"]["id"],
                json!(id),
                "cleanup preserves conversations"
            );
            assert_eq!(opened["thread"]["cwd"], path.to_str().unwrap());
            assert!(
                !path.exists(),
                "reading retained history must not recreate checkouts"
            );
            assert!(
                !git(&project, &["rev-parse", branch]).is_empty(),
                "cleanup preserves branches"
            );
        }
        local.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("unchanged worktree cleanup deadline");
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
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            codex_chat_snapshot(),
            None,
            "Fixture device",
            None,
        )
        .await
        .unwrap();
        let prompt = "[success] [visualize] Compare twelve icons";
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        store
            .dispatch(Intent::SetDraftText {
                thread_id: store
                    .snapshot()
                    .panes
                    .primary()
                    .navigation
                    .draft_key
                    .clone(),
                text: prompt.into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: input_id("visualize"),
                alternate: false,
            })
            .await
            .unwrap();
        let id = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .thread_id
            .clone()
            .unwrap();
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            assert!(
                snapshot.panes.primary().error.is_none(),
                "{:?}",
                snapshot.panes.primary().error
            );
            if snapshot.sessions.conversations[&id]
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
            assert!(snapshot.panes.primary().error.is_none());
            assert!(snapshot.sessions.pending_submissions.is_empty());
            assert!(
                snapshot.input.drafts[&agent_core::state::DraftKey::from(id)]
                    .text
                    .is_empty()
            );
            let turns = snapshot.sessions.conversations[id].turns.as_ref().unwrap();
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
                    MarkdownBlock::Visualization { path, .. } => Some(path),
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
        let store = Store::connect(
            &endpoint,
            &fixture.ticket,
            persisted,
            None,
            "Fixture device",
            None,
        )
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
async fn session_list_tracks_real_worktree_changes_and_merges_through_host_and_store() {
    use agent_core::state::Intent;
    use agent_core::state::Snapshot;
    use agent_core::store::Store;
    async fn wait_decorations(store: &Store) {
        let mut updates = store.subscribe();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            let key = agent_core::state::operations::OperationKey::ListDecorations {
                scope: rpc::ListDecorationScope::Root,
            };
            assert!(snapshot.operation_error(key.clone()).is_none());
            if !snapshot.operation_running(key) {
                return;
            }
            updates.changed().await.unwrap();
        }
    }
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
        git(&repo, &["config", "branch.task.bex-base-ref", "refs/heads/main"]);
        std::fs::create_dir(checkout.join("nested")).unwrap();
        std::fs::write(
            root.join("bex-worktrees.json"),
            json!({"workspaceRoots": {checkout.to_str().unwrap(): repo}}).to_string(),
        ).unwrap();
        let fixture = start_host(&root).await;
        let local = fixture.local().await.unwrap();
        let mut ids = Vec::new();
        for cwd in [&checkout, &checkout.join("nested"), &repo, &root] {
            let reply = conversations::created(&local.peer,conversations::create_request(ProviderKind::Codex,Some(serde_json::from_value::<String>(json!(cwd)).unwrap()),None))
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
        let mut native_rows: Vec<_> = ids.iter().zip([&checkout, &checkout.join("nested"), &repo, &root]).map(|(id, cwd)| {
            json!({"id":id.id,"cwd":cwd,"name":"worktree history","gitInfo":{"branch":"task"}})
        }).collect();
        std::fs::write(root.join("list-fixture.json"), serde_json::to_vec(&native_rows).unwrap()).unwrap();
        let store = Store::new((local.peer, local.events), Snapshot::default(), None);
        for (step, expected) in [
            ("fresh", Some(false)),
            ("empty", Some(false)),
            ("untracked", Some(true)),
            ("staged", Some(true)),
            ("commit", Some(true)),
            ("merge", Some(false)),
            ("edited", Some(true)),
            ("reverted", Some(false)),
            ("new-work", Some(true)),
            ("main-only", Some(true)),
            ("no-diff", Some(false)),
        ] {
            match step {
                "untracked" => std::fs::write(checkout.join("work.txt"), "work\n").unwrap(),
                "staged" => {
                    git(&checkout, &["add", "work.txt"]);
                }
                "edited" => std::fs::write(checkout.join("work.txt"), "changed\n").unwrap(),
                "reverted" => {
                    git(&checkout, &["restore", "work.txt"]);
                }
                "empty" => {
                    git(&checkout, &["commit", "--allow-empty", "-m", step]);
                }
                "commit" => {
                    git(&checkout, &["commit", "-m", step]);
                }
                "new-work" => {
                    std::fs::write(checkout.join("work.txt"), "new work\n").unwrap();
                    git(&checkout, &["commit", "-am", step]);
                }
                "no-diff" => {
                    git(&checkout, &["revert", "--no-edit", "HEAD"]);
                }
                "main-only" => {
                    std::fs::write(repo.join("main.txt"), "only on main\n").unwrap();
                    git(&repo, &["add", "main.txt"]);
                    git(&repo, &["commit", "-m", step]);
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
            wait_decorations(&store).await;
            let snapshot = store.snapshot();
            let list = snapshot.thread_list().unwrap();
            for (index, id) in ids.iter().enumerate() {
                let row = list.threads.iter().find(|row| &row.id == id).unwrap();
                assert_eq!(
                    row.has_changes,
                    if index < 2 { expected } else if index == 2 { Some(false) } else { None },
                    "{step}: {index}"
                );
            }
            assert_eq!(snapshot.panes.primary().error, None);
        }
        git(&repo, &["merge", "--no-ff", "-m", "merge task", "task"]);
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        wait_decorations(&store).await;
        assert_eq!(
            store
                .snapshot()
                .thread_list()
                .unwrap()
                .threads
                .iter()
                .find(|row| row.id == ids[0])
                .unwrap()
                .has_changes,
            Some(false)
        );
        std::fs::remove_dir(checkout.join("nested")).unwrap();
        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
        wait_decorations(&store).await;
        assert_eq!(store.snapshot().thread_list().unwrap().threads.iter().find(|row| row.id == ids[1]).unwrap().has_changes, None);
        std::fs::create_dir(checkout.join("nested")).unwrap();
        git(&checkout, &["checkout", "--detach"]);
        store
            .dispatch(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            )))
            .await
            .unwrap();
        wait_decorations(&store).await;
        assert_eq!(
            store
                .snapshot()
                .thread_list()
                .unwrap()
                .threads
                .iter()
                .find(|row| row.id == ids[0])
                .unwrap()
                .has_changes,
            Some(false)
        );
        git(&checkout, &["checkout", "task"]);
        std::fs::write(checkout.join("work.txt"), "remaining work\n").unwrap();
        git(&checkout, &["commit", "-am", "remaining work"]);
        git(&repo, &["worktree", "remove", checkout.to_str().unwrap()]);
        for (step, expected) in [
            ("deleted-unmerged", None),
            ("deleted-merged", None),
            ("missing-metadata", None),
            ("deleted-branch", None),
        ] {
            match step {
                "deleted-merged" => { git(&repo, &["merge", "--no-ff", "-m", "merge remaining work", "task"]); }
                "missing-metadata" => {
                    native_rows[1]["gitInfo"] = Value::Null;
                    std::fs::write(root.join("list-fixture.json"), serde_json::to_vec(&native_rows).unwrap()).unwrap();
                }
                "deleted-branch" => { git(&repo, &["branch", "-d", "task"]); }
                _ => {}
            }
            store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
            wait_decorations(&store).await;
            let snapshot = store.snapshot();
            let list = snapshot.thread_list().unwrap();
            for (index, id) in ids.iter().enumerate() {
                let row = list.threads.iter().find(|row| &row.id == id).unwrap();
                assert_eq!(row.has_changes, if index == 2 { Some(false) } else { expected }, "{step}: {index}");
            }
            assert_eq!(snapshot.panes.primary().error, None);
        }
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
        let created = conversations::created(&client.peer,conversations::create_request(ProviderKind::Codex,Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),None)).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
        let id = &created["thread"]["id"];
        let session = serde_json::from_value(id.clone()).unwrap();
        let mut events = conversations::Conversation::open(&client.peer, &session).await.unwrap();
        assert_eq!(json!(events.view().thread.id), *id);
        conversations::submit(&client.peer, &serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"session update fixture"}}]})).unwrap()).await.unwrap();
        events.finished().await;
        let reopened = conversations::Conversation::open(&client.peer, &session).await.unwrap();
        assert_eq!(reopened.view().thread.turns, events.view().thread.turns);
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

    let started = conversations::created(
        &mobile.peer,
        conversations::create_request(
            ProviderKind::Codex,
            Some(serde_json::from_value::<String>(json!(directory.path())).unwrap()),
            None,
        ),
    )
    .await
    .map(|output| serde_json::to_value(output).unwrap())
    .unwrap();
    let id = &started["thread"]["id"];
    let (_, mut events) = open_conversation(&mobile.peer, id).await;
    conversations::submit(&mobile.peer,&serde_json::from_value::<rpc::Submission>(json!({"clientUserMessageId":next_submission_id(),"threadId":id,"input":[{"text":{"text":"[generated-images] [gallery] Browse all generated images"}}]})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
    completed_turn(&mut events).await;
    // Native history the turn added outside its live stream arrives when the
    // project is imported again.
    let session: agent_protocol::session::SessionRef = serde_json::from_value(id.clone()).unwrap();
    imported(&fixture, &session).await;
    mobile
        .peer
        .request::<agent_protocol::project_import::ProjectImportResult>(
            &agent_protocol::protocol::Call::ImportProjects(
                agent_protocol::project_import::ImportProjects {
                    paths: vec![directory.path().to_str().unwrap().into()],
                },
            ),
        )
        .await
        .unwrap();
    let peer = Arc::new(mobile.peer);
    let mut images = Vec::new();
    for _ in 0..500 {
        images = peer
            .session_images(&session, Some(&mobile.session))
            .await
            .unwrap();
        if images.len() == 8 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        std::fs::write(directory.path().join("list-fixture.json"), json!([{
            "id":"oversized", "cwd":directory.path().join("oversized-history"), "historyMode":"paginated"
        }]).to_string()).unwrap();
        let fixture = start_host(directory.path()).await;
        let mobile = fixture.local().await.unwrap();
        let mut catalog = conversations::Catalog::open(&mobile.peer).await;
        catalog.wait_rows(1).await;
        let thread_id = SessionRef::new(ProviderKind::Codex, "oversized".into()).unwrap();
        let mut conversation = conversations::Conversation::open(&mobile.peer, &thread_id).await.unwrap();
        for _ in 0..2 {
            conversation = conversations::Conversation::open(&mobile.peer, &thread_id).await.unwrap();
            let opened = serde_json::to_value(conversation.view()).unwrap();
            assert!(agent_protocol::protocol::encode(conversation.replica.snapshot().unwrap()).unwrap().len() < 16 * 1024);
            let turn = &opened["thread"]["turns"][0];
            // An import copies messages and images; past tool activity stays native.
            let deferred = ["oversized-text", "oversized-image"];
            assert_eq!(
                turn["items"].as_array().unwrap().iter().filter(|item| item["body"]["deferred"].is_object()).map(|item| item["id"].as_str().unwrap()).collect::<Vec<_>>(),
                deferred
            );
            assert!(
                body_json(&turn["items"][1])["imageGeneration"]["data"].is_null(),
                "base64 must be absent, never truncated"
            );
        }
        for (id, kind, field, byte) in [
            ("oversized-text", "assistantText", "text", b'x'),
            ("oversized-image", "imageGeneration", "data", b'A'),
        ] {
            let (item, wire_bytes, transfer_size) = conversation.hydrate(&mobile.peer, &mobile.session, &id.into()).await;
            assert!(transfer_size.unwrap() > 16 * 1024 * 1024);
            assert!(wire_bytes < 16 * 1024);
            let item = serde_json::to_value(item).unwrap();
            let body = body_json(&item)[kind][field].as_str().unwrap();
            assert_eq!(body.len(), 17 * 1024 * 1024);
            assert!(body.bytes().all(|value| value == byte));
        }
        // Visible messages and images hydrate through the shared Store path
        // used by all native clients.
        use agent_core::state::Intent;
        use agent_core::store::Store;
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, Default::default(), None, "Fixture device", None)
            .await
            .unwrap();
        let mut updates = store.subscribe();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(thread_id.clone())))
            .await
            .unwrap();
        loop {
            let snapshot = updates.borrow_and_update().clone();
            assert!(snapshot.panes.primary().error.is_none(), "{:?}", snapshot.panes.primary().error);
            let items = &snapshot.sessions.conversations[&thread_id].turns.as_ref().unwrap()[0]
                .items
                .as_ref()
                .unwrap();
            // The summary's body can finish loading before the detail stream
            // delivers the remaining item metadata.
            if let [text, image, ..] = items.as_slice()
                && item_text(text).is_some_and(|text| text.len() == 17 * 1024 * 1024)
                && matches!(image.body(), agent_protocol::items::ItemBody::ImageGeneration { data: Some(data), .. } if data.len() == 17 * 1024 * 1024)
            {
                break;
            }
            updates.changed().await.unwrap();
        }
        assert_eq!(
            store.snapshot().sessions.conversations[&thread_id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().len(),
            2,
            "imported turn details hold messages and images"
        );
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
        let store = Store::new((local.peer, local.events), Snapshot::default(), None);
        let mut updates = store.subscribe();
        updates
            .wait_for(|snapshot| snapshot.connection.connected && snapshot.lists.threads.is_some())
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
                    snapshot.panes.primary().navigation.cwd == folder.to_str().unwrap()
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
                snapshot.panes.primary().navigation.draft_key,
                agent_core::state::Navigation::default().draft_key
            );
            assert_eq!(snapshot.panes.primary().error, None);
        }
        let project_id = store.snapshot().thread_list().unwrap().projects[0]
            .id
            .clone();
        let created = conversations::created(
            peer,
            conversations::create_request(
                agent_protocol::session::ProviderKind::Codex,
                Some(folder.to_str().unwrap().into()),
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            created.thread.project_id.as_deref(),
            Some(project_id.as_str())
        );
        assert!(root.join("bex-conversations.sqlite").is_file());
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
            None,
        )
        .await
        .unwrap();
        let client = reopened.local().await.unwrap();
        let catalog = conversations::Catalog::open(&client.peer).await;
        let list = catalog.page(Default::default()).lists.threads.unwrap();
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
        assert_eq!(catalog.candidates.len(), 3);
        let plugin = catalog
            .candidates
            .iter()
            .find(|c| c.invocation.kind == agent_protocol::composer::InvocationKind::Plugin)
            .unwrap();
        assert_eq!(plugin.invocation.name, "Fixture Plugin");
        assert_eq!(plugin.invocation.path, "plugin://fixture@local");
        let mut skills: Vec<_> = catalog
            .candidates
            .iter()
            .filter(|c| c.invocation.kind == agent_protocol::composer::InvocationKind::Skill)
            .map(|c| c.invocation.path.as_str())
            .collect();
        skills.sort_unstable();
        assert_eq!(
            skills,
            [
                "/fixture/skills/explain/SKILL.md",
                "/fixture/skills/review/SKILL.md"
            ]
        );
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
        let thread = conversations::created(
            &first.peer,
            conversations::create_request(agent_protocol::session::ProviderKind::Codex, None, None),
        )
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
        let input = rpc::Submission {
            thread_id: thread.clone(),
            client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
            input: vec![rpc::Input::Text {
                text: "[delayed-input] wait for another client".into(),
            }],
            model: None,
            effort: None,
            service_tier: None,
            follow_up: rpc::FollowUp::Queue,
        };
        let (started, duplicate) = tokio::join!(
            conversations::submit(&first.peer, &input),
            conversations::submit(&first.peer, &input),
        );
        let started = started.unwrap();
        let duplicate = duplicate.unwrap();
        assert_eq!(
            (&duplicate.outcome, duplicate.cursor),
            (&started.outcome, started.cursor),
            "one durable receipt per input"
        );
        first.peer.close().await;
        let second = fixture.local().await.unwrap();
        let replay = conversations::submit(&second.peer, &input).await.unwrap();
        assert_eq!(
            (&replay.outcome, replay.cursor),
            (&started.outcome, started.cursor)
        );
        let mut events = conversations::Conversation::open(&second.peer, &thread)
            .await
            .unwrap();
        let first_turn = events
            .input_turn(&input.client_user_message_id, false)
            .await;
        let follow_up = rpc::Submission {
            client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
            input: vec![rpc::Input::Text {
                text: "additional input".into(),
            }],
            follow_up: rpc::FollowUp::Steer,
            ..input.clone()
        };
        conversations::submit(&second.peer, &follow_up)
            .await
            .unwrap();
        let steered = events
            .input_turn(&follow_up.client_user_message_id, false)
            .await;
        assert_eq!(steered.id, first_turn.id, "steer joins the live turn");
        std::fs::write(directory.path().join("release-inputs"), "").unwrap();
        events.input_turn(&input.client_user_message_id, true).await;
        // Final-turn completion and the native steer echo use separate tasks.
        while !events
            .view()
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .any(|item| item_text(item) == Some("Additional input recorded"))
        {
            events.next().await;
        }

        let reopened = conversations::Conversation::open(&second.peer, &thread)
            .await
            .unwrap();
        let history = reopened.view();
        for id in [
            &input.client_user_message_id,
            &follow_up.client_user_message_id,
        ] {
            assert_eq!(
                history
                    .thread
                    .turns
                    .iter()
                    .flatten()
                    .flat_map(|turn| turn.items.iter().flatten())
                    .filter(|item| item.client_input_id.as_ref() == Some(id))
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
        agent_protocol::items::ItemBody::UserMessage { text, content } => {
            text.as_deref().or_else(|| {
                content.iter().find_map(|part| match part {
                    agent_protocol::items::MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
        }
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
            (-32601, "unavailable", "pagination unavailable", false, true),
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
        let mut catalog = conversations::Catalog::open(&local.peer).await;
        catalog.wait_rows(cases.len()).await;
        for ((_, id, _, succeeds, _), native) in cases.iter().zip(&threads) {
            let result = conversations::Conversation::open(&local.peer, &SessionRef::new(ProviderKind::Codex, id.to_string()).unwrap()).await;
            if *succeeds {
                let thread = result.unwrap().view().thread;
                assert!(thread.turns.unwrap().is_empty());
                let state = thread.history_read_state.unwrap();
                assert_eq!(state.kind, agent_protocol::session::HistoryReadKind::Complete);
                assert!(state.issues.is_empty());
            } else {
                let thread = result.unwrap().view().thread;
                let failure = thread.history_read_state.unwrap();
                assert_eq!(failure.kind, agent_protocol::session::HistoryReadKind::Unavailable, "native history failure was replaced with empty history: {id}");
                assert_eq!(failure.issues[0],native["fixtureHistoryError"]["message"]);
                assert!(thread.turns.unwrap().is_empty());
            }
        }
        local.close().await;
        fixture.close().await.unwrap();
    }).await.expect("native history classification deadline");
}
