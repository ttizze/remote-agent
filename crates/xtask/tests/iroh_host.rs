use agent_core::{
    peer::{PeerEvent, RpcPeer},
    transport::{Endpoint, Identity, PairingTicket, Relays, Session, Ticket},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use codex_app_server::{AppServerConfig, CodexAppServer};
use host_daemon::{
    CodexRpcService, CredentialStore, DesktopProjectStore, HostCredentials, HostRuntime,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Default)]
struct Memory {
    bytes: Mutex<Option<Zeroizing<Vec<u8>>>>,
}
impl CredentialStore for Memory {
    fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
        Ok(self.bytes.lock().unwrap().clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), String> {
        *self.bytes.lock().unwrap() = Some(Zeroizing::new(bytes.to_vec()));
        Ok(())
    }
}
fn fixture_program(directory: &Path) -> PathBuf {
    xtask::fixture::Config {
        stream_delay_ms: 5,
        ..Default::default()
    }
    .install(
        Path::new(env!("CARGO_BIN_EXE_bex-codex-fixture")),
        directory,
    )
    .unwrap()
}
struct Fixture {
    server: Arc<CodexAppServer>,
    credentials: Arc<HostCredentials>,
    memory: Arc<Memory>,
    ticket: Ticket,
    stop: CancellationToken,
    running: tokio::task::JoinHandle<Result<(), String>>,
}
impl Fixture {
    async fn start(directory: &Path) -> Self {
        Self::start_with_memory(directory, Arc::new(Memory::default())).await
    }
    async fn start_with_memory(directory: &Path, memory: Arc<Memory>) -> Self {
        let credentials = Arc::new(
            HostCredentials::load(memory.clone(), directory.join("state"))
                .await
                .unwrap(),
        );
        let endpoint = Endpoint::bind(credentials.host_identity().await, Relays::Disabled)
            .await
            .unwrap();
        let ticket = endpoint.ticket();
        let server = Arc::new(
            CodexAppServer::spawn(AppServerConfig {
                program: fixture_program(directory),
                ..Default::default()
            })
            .await
            .unwrap(),
        );
        let service = CodexRpcService::new(
            server.clone(),
            DesktopProjectStore::new(directory.join("projects.json")),
        );
        let runtime = Arc::new(
            HostRuntime::new(
                service,
                endpoint,
                credentials.clone(),
                "isolated Host".into(),
            )
            .await,
        );
        let stop = CancellationToken::new();
        let running = tokio::spawn(runtime.run(stop.clone()));
        Self {
            server,
            credentials,
            memory,
            ticket,
            stop,
            running,
        }
    }
    async fn connect(&self, identity: Identity) -> Connection {
        let endpoint = Endpoint::bind(identity, Relays::Disabled).await.unwrap();
        let session = endpoint.connect(&self.ticket).await.unwrap();
        let peer = session
            .open_peer(Duration::from_secs(10), 128)
            .await
            .unwrap();
        Connection {
            endpoint,
            session,
            peer,
        }
    }
    async fn local(&self) -> Connection {
        self.connect(self.credentials.local_identity().await).await
    }
    async fn close(self) {
        self.stop.cancel();
        self.running.await.unwrap().unwrap();
        Arc::try_unwrap(self.server)
            .ok()
            .expect("Codex process retained")
            .shutdown()
            .await
            .unwrap();
    }
}
struct Connection {
    endpoint: Endpoint,
    session: Session,
    peer: RpcPeer,
}
impl Connection {
    async fn close(self) {
        self.peer.close().await.unwrap();
        self.session.close();
        self.endpoint.close().await;
    }
}
async fn rpc(peer: &RpcPeer, method: &str, params: Value) -> Value {
    peer.request::<_, Value>(method, &params)
        .await
        .unwrap()
        .value
}
async fn next_message(events: &mut broadcast::Receiver<PeerEvent>) -> Value {
    loop {
        match events.recv().await.unwrap() {
            PeerEvent::Message(message) => return serde_json::from_str(&message.value).unwrap(),
            PeerEvent::Response { .. } => {}
            PeerEvent::Closed(reason) => panic!("unexpected closure: {reason}"),
        }
    }
}
async fn next_method(events: &mut broadcast::Receiver<PeerEvent>, method: &str) -> Value {
    loop {
        let message = next_message(events).await;
        if message["method"] == method {
            return message;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_is_atomic_local_management_is_private_and_revocation_closes_active_sessions() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let local = fixture.local().await;
        let stranger = fixture.connect(Identity::generate()).await;
        assert!(
            stranger
                .peer
                .request::<_, Value>("thread/start", &json!({"cwd":directory.path()}))
                .await
                .is_err()
        );
        stranger.endpoint.close().await;
        assert_eq!(
            rpc(&local.peer, "thread/list", json!({})).await["data"],
            json!([])
        );
        let invitation: PairingTicket =
            serde_json::from_value(rpc(&local.peer, "host/invite", json!({})).await).unwrap();
        let key = Identity::generate().to_bytes();
        let trust_path = directory.path().join("state/trust.json");
        let backup = directory.path().join("state/trust-backup.json");
        std::fs::rename(&trust_path, &backup).unwrap();
        std::fs::create_dir(&trust_path).unwrap();
        let failed = fixture.connect(Identity::from_bytes(key)).await;
        assert!(
            failed
                .peer
                .request::<_, Value>("host/pair", &json!({"invitation":invitation.invitation}))
                .await
                .is_err()
        );
        failed.endpoint.close().await;
        std::fs::remove_dir(&trust_path).unwrap();
        std::fs::rename(&backup, &trust_path).unwrap();
        let paired = fixture.connect(Identity::from_bytes(key)).await;
        rpc(
            &paired.peer,
            "host/pair",
            json!({"invitation":invitation.invitation}),
        )
        .await;
        assert!(
            paired
                .peer
                .request::<_, Value>("host/invite", &json!({}))
                .await
                .is_err()
        );
        assert!(
            paired
                .peer
                .request::<_, Value>("host/listRemotes", &json!({}))
                .await
                .is_err()
        );
        assert!(
            paired
                .peer
                .request::<_, Value>("host/revoke", &json!({"nodeId":local.endpoint.node_id()}))
                .await
                .is_err()
        );
        let reused = fixture.connect(Identity::generate()).await;
        assert!(
            reused
                .peer
                .request::<_, Value>("host/pair", &json!({"invitation":invitation.invitation}))
                .await
                .is_err()
        );
        reused.endpoint.close().await;
        let status = rpc(&local.peer, "host/status", json!({})).await;
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
        rpc(
            &local.peer,
            "host/revoke",
            json!({"nodeId":paired.endpoint.node_id()}),
        )
        .await;
        assert!(
            paired
                .peer
                .request::<_, Value>("thread/list", &json!({}))
                .await
                .is_err()
        );
        paired.endpoint.close().await;
        let revoked = fixture.connect(Identity::from_bytes(key)).await;
        assert!(
            revoked
                .peer
                .request::<_, Value>("thread/list", &json!({}))
                .await
                .is_err()
        );
        revoked.endpoint.close().await;
        local.close().await;
        fixture.close().await;
    })
    .await
    .expect("pairing contract deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_consumers_cannot_both_use_one_invitation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let local = fixture.local().await;
        let invitation: PairingTicket =
            serde_json::from_value(rpc(&local.peer, "host/invite", json!({})).await).unwrap();
        let first = fixture.connect(Identity::generate()).await;
        let second = fixture.connect(Identity::generate()).await;
        let params = json!({"invitation":invitation.invitation});
        let (a, b) = tokio::join!(
            first.peer.request::<_, Value>("host/pair", &params),
            second.peer.request::<_, Value>("host/pair", &params)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let status = rpc(&local.peer, "host/status", json!({})).await;
        assert_eq!(status["devices"].as_array().unwrap().len(), 2);
        first.endpoint.close().await;
        second.endpoint.close().await;
        local.close().await;
        fixture.close().await;
    })
    .await
    .expect("concurrent pairing deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_clients_receive_their_own_resolved_approval_id() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let first = fixture.local().await;
        let second = fixture.local().await;
        let mut first_events = first.peer.subscribe();
        let mut second_events = second.peer.subscribe();
        let started = rpc(&first.peer, "thread/start", json!({"cwd":directory.path()})).await;
        rpc(&second.peer, "thread/list", json!({})).await;
        let id = &started["thread"]["id"];
        rpc(
            &first.peer,
            "turn/start",
            json!({"threadId":id,"input":[{"type":"text","text":"[approval]"}]}),
        )
        .await;
        let a = next_method(&mut first_events, "item/commandExecution/requestApproval").await;
        let b = next_method(&mut second_events, "item/commandExecution/requestApproval").await;
        first
            .peer
            .respond_raw(&a["id"].to_string(), "result", r#"{"decision":"accept"}"#)
            .await
            .unwrap();
        let done_a = next_method(&mut first_events, "serverRequest/resolved").await;
        let done_b = next_method(&mut second_events, "serverRequest/resolved").await;
        assert_eq!(done_a["params"]["requestId"], a["id"]);
        assert_eq!(done_b["params"]["requestId"], b["id"]);
        let completed = next_method(&mut second_events, "turn/completed").await;
        assert_eq!(completed["params"]["turn"]["status"], "completed");
        first.close().await;
        second.close().await;
        fixture.close().await;
    })
    .await
    .expect("approval routing deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binary_transfers_use_the_issuing_iroh_session_and_preserve_bytes() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let client = fixture.local().await;
        let content: Vec<u8> = (0..65537).map(|n| (n % 251) as u8).collect();
        let source = directory.path().join("source.bin");
        std::fs::write(&source, &content).unwrap();
        let uploaded = agent_core::transfers::upload_file(
            &client.peer,
            || async {
                client
                    .session
                    .open_stream()
                    .await
                    .map_err(std::io::Error::other)
            },
            &source,
            directory.path(),
            "upload.bin",
        )
        .await
        .unwrap();
        let other = fixture.local().await;
        rpc(&other.peer, "thread/list", json!({})).await;
        let denied = directory.path().join("denied.bin");
        assert!(
            agent_core::transfers::download_file(
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
        agent_core::transfers::download_file(
            &client.peer,
            || async {
                client
                    .session
                    .open_stream()
                    .await
                    .map_err(std::io::Error::other)
            },
            Path::new(uploaded["path"].as_str().unwrap()),
            &destination,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), content);
        client.close().await;
        fixture.close().await;
    })
    .await
    .expect("binary transfer deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_registration_pairs_the_local_client_identity_for_direct_connections() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = Fixture::start(a.path()).await;
        let second = Fixture::start(b.path()).await;
        let local_a = first.local().await;
        let local_b = second.local().await;
        let invitation: PairingTicket =
            serde_json::from_value(rpc(&local_b.peer, "host/invite", json!({})).await).unwrap();
        let direct_session = local_a
            .endpoint
            .connect(&invitation.endpoint)
            .await
            .unwrap();
        let direct_peer = direct_session
            .open_peer(Duration::from_secs(10), 128)
            .await
            .unwrap();
        rpc(
            &direct_peer,
            "host/pair",
            json!({"invitation":invitation.invitation}),
        )
        .await;
        let profile = rpc(
            &local_a.peer,
            "host/registerRemote",
            json!({"ticket":invitation.endpoint,"name":"remote fixture"}),
        )
        .await;
        assert_eq!(profile["id"], json!(second.ticket.node_id()));
        assert_eq!(
            rpc(&direct_peer, "thread/list", json!({})).await["data"],
            json!([])
        );
        assert!(
            direct_peer
                .request::<_, Value>("host/invite", &json!({}))
                .await
                .is_err()
        );
        assert_eq!(
            rpc(&local_a.peer, "host/listRemotes", json!({})).await,
            json!([profile])
        );
        rpc(
            &local_a.peer,
            "host/removeRemote",
            json!({"id":second.ticket.node_id()}),
        )
        .await;
        assert_eq!(
            rpc(&local_a.peer, "host/listRemotes", json!({})).await,
            json!([])
        );
        direct_peer.close().await.unwrap();
        direct_session.close();
        local_a.close().await;
        local_b.close().await;
        first.close().await;
        second.close().await;
    })
    .await
    .expect("remote registration deadline");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn large_history_loads_conversation_before_lossless_item_details() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("projects.json"), serde_json::to_vec(&json!({
            "local-projects": {"workspace": {"id":"workspace", "name":"Workspace", "rootPaths":[directory.path()]}}
        })).unwrap()).unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let mobile = fixture.local().await;

        let started = mobile.peer.request::<_, Value>("host/thread/start", &json!({"cwd":directory.path().join("large-history")})).await.unwrap().value;
        assert_eq!(started["thread"]["projectId"], "workspace");
        let thread = &started["thread"]["id"];
        let listed = mobile.peer.request::<_, Value>("host/thread/list", &json!({"limit":20})).await.unwrap().value;
        assert_eq!(listed["data"][0]["id"], *thread);
        assert_eq!(listed["data"][0]["projectId"], "workspace");
        let start = std::time::Instant::now();
        let preview = mobile.peer.request::<_, Value>("host/thread/read", &json!({"threadId":thread,"includeTurns":true,"deferItemDetails":true})).await.unwrap().value;
        let preview_bytes = serde_json::to_vec(&preview).unwrap().len();
        assert_eq!(preview["thread"]["projectId"], "workspace");
        println!("large history preview: {preview_bytes} bytes, {} ms", start.elapsed().as_millis());
        // A byte budget is deterministic; machine speed and network scheduling are not.
        assert!(preview_bytes < 16 * 1024, "collapsed output must not delay the conversation: {preview_bytes} bytes");
        let turn = &preview["thread"]["turns"][0];
        assert_eq!(turn["items"][0]["content"][0]["text"], "Read the whole output");
        assert_eq!(turn["items"][2]["text"], "Large history is complete");
        assert_eq!(turn["deferredItemIds"], json!(["large-command"]));
        let full = mobile.peer.request::<_, Value>("host/thread/read", &json!({"threadId":thread,"includeTurns":true})).await.unwrap().value;
        assert!(full["thread"]["turns"][0].get("deferredItemIds").is_none(), "full-history clients must retain inline details");
        let detail = mobile.peer.request::<_, Value>("host/thread/item/read", &json!({"threadId":thread,"turnId":"large-turn","itemId":"large-command"})).await.unwrap().value;
        assert_eq!(detail["item"]["aggregatedOutput"], format!("{}END_OF_LARGE_OUTPUT", "output line\n".repeat(700000)));
        assert_eq!(detail["item"], full["thread"]["turns"][0]["items"][1]);
        assert!(mobile.peer.request::<_, Value>("host/thread/item/read", &json!({"threadId":thread,"turnId":"wrong-turn","itemId":"large-command"})).await.is_err());
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&json!([
            {"id":"fixture-long-history","cwd":directory.path(),"historyMode":"paginated","updatedAt":1}
        ])).unwrap()).unwrap();
        mobile.peer.request::<_, Value>("host/thread/list", &json!({"useStateDbOnly":true})).await.unwrap();
        let mut page = mobile.peer.request::<_, Value>("host/thread/read", &json!({"threadId":"fixture-long-history","includeTurns":true,"paginateHistory":true})).await.unwrap().value;
        assert_eq!(page["thread"]["turns"].as_array().unwrap().len(), 5);
        assert_eq!(page["thread"]["turns"].as_array().unwrap().iter().map(|t| t["items"].as_array().unwrap().len()).sum::<usize>(), 500);
        assert_eq!(page["thread"]["turns"][4]["items"][153]["id"], "long-latest-message");
        let mut turn_ids = std::collections::HashSet::new();
        let mut item_ids = std::collections::HashSet::new();
        loop {
            for turn in page["thread"]["turns"].as_array().unwrap() {
                assert!(turn_ids.insert(turn["id"].as_str().unwrap().to_owned()));
                let mut items = turn.clone();
                loop {
                    assert!(items["items"].as_array().unwrap().len() <= 500);
                    for item in items["items"].as_array().unwrap() {
                        assert!(item_ids.insert(item["id"].as_str().unwrap().to_owned()), "repeated history item");
                    }
                    if items["itemsHasMore"] != true { break; }
                    let cursor = &items["itemsNextCursor"];
                    items = mobile.peer.request::<_, Value>("host/thread/items/list", &json!({"threadId":"fixture-long-history","turnId":turn["id"],"cursor":cursor})).await.unwrap().value["thread"]["turns"][0].take();
                }
            }
            let Some(cursor) = page["thread"]["historyCursor"].as_str() else { break; };
            page = mobile.peer.request::<_, Value>("host/thread/turns/list", &json!({"threadId":"fixture-long-history","cursor":cursor})).await.unwrap().value;
        }
        assert_eq!(turn_ids.len(), 10);
        assert_eq!(item_ids.len(), 3718);

        let started = mobile.peer.request::<_, Value>("host/thread/start", &json!({"cwd":directory.path()})).await.unwrap().value;
        let image_thread = &started["thread"]["id"];
        let mut messages = mobile.peer.subscribe();
        mobile.peer.request::<_, Value>("turn/start", &json!({"threadId":image_thread,"input":[{"type":"text","text":"[generated-images]"}]})).await.unwrap();
        loop {
            let event: Value = next_message(&mut messages).await;
            if event["method"] == "turn/completed" && event["params"]["threadId"] == *image_thread { break; }
        }
        let history = mobile.peer.request::<_, Value>("host/thread/read", &json!({"threadId":image_thread,"includeTurns":true,"paginateHistory":true,"deferItemDetails":true})).await.unwrap().value;
        let turn = &history["thread"]["turns"][0];
        let images: Vec<_> = turn["items"].as_array().unwrap().iter().filter(|item| item["type"] == "imageGeneration").collect();
        assert_eq!(images.len(), 2);
        let original = std::fs::read(directory.path().join("fixture image.png")).unwrap();
        for item in images {
            assert_eq!(item["status"], "completed");
            assert_eq!(STANDARD.decode(item["result"].as_str().unwrap()).unwrap(), original);
            assert!(!turn["deferredItemIds"].as_array().is_some_and(|ids| ids.contains(&item["id"])));
        }
        mobile.close().await;
        fixture.close().await;
    }).await.expect("large history loop exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn title_lists_are_recent_scoped_small_and_expand_without_loading_bodies() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let mut projects = serde_json::Map::new();
        let mut threads = Vec::new();
        for project in 1..=7 {
            let id = format!("project-{project}");
            let cwd = directory.path().join(&id);
            projects.insert(id.clone(), json!({"id":id,"name":format!("Project {project:02}"),"rootPaths":[cwd]}));
            for index in 1..=18 {
                threads.push(json!({"id":format!("p{project}-{index}"),"cwd":cwd,"name":format!("Project {project:02} conversation {index:02}"),"updatedAt":project*100+index,"preview":"unused history".repeat(1000)}));
            }
        }
        for index in 1..=18 {
            threads.push(json!({"id":format!("chat-{index}"),"cwd":directory.path().join("unassigned"),"name":format!("Chat {index:02}"),"updatedAt":index}));
        }
        threads.extend([
            json!({"id":"explicit","cwd":directory.path().join("unassigned"),"name":"Explicit assignment","updatedAt":90000}),
            json!({"id":"projectless","cwd":directory.path().join("project-7"),"name":"Explicit chat","updatedAt":90001}),
            json!({"id":"worktree","cwd":directory.path().join("worktree"),"name":"Worktree conversation","updatedAt":90002}),
        ]);
        let rollout = directory.path().join("external-rollout.jsonl");
        std::fs::write(&rollout, "initial\n").unwrap();
        let external = threads.iter_mut().find(|thread| thread["id"] == "p5-1").unwrap();
        external["historyMode"] = json!("paginated");
        external["status"] = json!({"type":"notLoaded"});
        external["path"] = json!(rollout);
        std::fs::write(directory.path().join("list-fixture.json"), serde_json::to_vec(&threads).unwrap()).unwrap();
        std::fs::write(directory.path().join("projects.json"), serde_json::to_vec(&json!({
            "local-projects":projects,
            "thread-project-assignments":{"explicit":{"projectId":"project-3"}},
            "projectless-thread-ids":["projectless"],
            "thread-workspace-root-hints":{"worktree":directory.path().join("project-5")}
        })).unwrap()).unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let mobile = fixture.local().await;

        let request = |project_limit, chat_limit, thread_limit| json!({"titleOnly":true,"projectLimit":project_limit,"chatLimit":chat_limit,"projectThreadLimits":{"project-5":thread_limit}});
        let start = std::time::Instant::now();
        let first = mobile.peer.request::<_, Value>("host/thread/list", &request(5, 5, 5)).await.unwrap().value;
        let bytes = serde_json::to_vec(&first).unwrap().len();
        println!("title list through iroh: {bytes} bytes, {} ms", start.elapsed().as_millis());
        assert!(bytes < 16 * 1024, "initial titles exceeded the transfer budget");
        let rows = first["data"].as_array().unwrap();
        assert_eq!(rows.len(), 30);
        assert_eq!(first["projects"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreProjects"], true);
        assert_eq!(first["projects"].as_array().unwrap().iter().take(5).map(|project| project["id"].as_str().unwrap()).collect::<Vec<_>>(), ["project-5", "project-3", "project-7", "project-6", "project-4"]);
        assert_eq!(rows[0]["id"], "worktree");
        assert_eq!(rows[4]["id"], "p5-15");
        assert_eq!(rows[5]["id"], "explicit");
        assert_eq!(rows[25]["id"], "projectless");
        assert_eq!(rows[29]["id"], "chat-15");
        assert!(rows.iter().all(|thread| thread.get("turns").is_none() && thread.get("preview").is_none()));
        assert_eq!(first["moreProjectIds"].as_array().unwrap().len(), 5);
        assert_eq!(first["hasMoreChats"], true);

        let more = mobile.peer.request::<_, Value>("host/thread/list", &request(5, 5, 15)).await.unwrap().value;
        assert_eq!(more["data"].as_array().unwrap().len(), 40);
        assert_eq!(more["data"][14]["id"], "p5-5");
        let end = mobile.peer.request::<_, Value>("host/thread/list", &request(15, 25, 25)).await.unwrap().value;
        assert_eq!(end["data"].as_array().unwrap().len(), 19 + 6*5 + 19);
        assert_eq!(end["hasMoreChats"], false);
        assert_eq!(end["hasMoreProjects"], false);
        assert!(!end["moreProjectIds"].as_array().unwrap().contains(&json!("project-5")));
        let found = mobile.peer.request::<_, Value>("host/thread/list", &json!({"titleOnly":true,"searchTerm":"Project 01"})).await.unwrap().value;
        assert_eq!(found["projects"].as_array().unwrap().len(), 1);
        assert_eq!(found["data"].as_array().unwrap().len(), 5);
        assert_eq!(found["data"][0]["id"], "p1-18");
        let body = mobile.peer.request::<_, Value>("host/thread/read", &json!({"threadId":"p5-1","includeTurns":true})).await.unwrap().value;
        assert_eq!(body["thread"]["turns"][0]["items"][0]["text"], "History for Project 05 conversation 01");
        assert_eq!(body["thread"]["status"]["type"], "notLoaded");
        let item = mobile.peer.request::<_, Value>("host/thread/item/read", &json!({"threadId":"p5-1","turnId":"turn-p5-1","itemId":"answer-p5-1"})).await.unwrap().value;
        assert_eq!(item["item"]["text"], "History for Project 05 conversation 01");
        let mut changes = mobile.peer.subscribe();
        mobile.peer.request::<_, Value>("host/thread/watch", &json!({"watchId":1,"threadId":"p5-1","path":rollout})).await.unwrap();
        std::fs::write(&rollout, "external client persisted a reply\n").unwrap();
        let changed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event: Value = next_message(&mut changes).await;
                if event["method"] == "host/thread/changed" { break event; }
            }
        }).await.expect("rollout changes must cross iroh");
        assert_eq!(changed["params"], json!({"watchId":1,"threadId":"p5-1"}));
        mobile.peer.request::<_, Value>("host/thread/unwatch", &json!({"watchId":1})).await.unwrap();
        mobile.close().await;
        fixture.close().await;
    }).await.expect("title list loop exceeded deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_worktree_settings_route_both_start_methods_and_preserve_project_membership() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
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
        let project_state = root.join("projects.json");
        std::fs::write(&project_state, serde_json::to_vec(&json!({
            "local-projects":{"workspace":{"id":"workspace","name":"Workspace","rootPaths":[workspace]}}
        })).unwrap()).unwrap();
        let server = Arc::new(CodexAppServer::spawn(AppServerConfig { program: fixture_program(&root), ..Default::default() }).await.unwrap());
        let service = CodexRpcService::new(server.clone(), DesktopProjectStore::new(&project_state));
        let mut session = service.open_session(64);
        async fn request(service: &CodexRpcService, session: &mut host_daemon::CodexSession, method: &str, params: Value) -> Value {
            service.dispatch_request(session.id(), json!({"id":42,"method":method,"params":params}).to_string()).await.unwrap();
            loop {
                let response: Value = serde_json::from_str(&session.recv().await.unwrap()).unwrap();
                if response["id"] == 42 {
                    assert!(response.get("error").is_none(), "{response}");
                    return response["result"].clone();
                }
            }
        }
        let initial = request(&service, &mut session, "thread/start", json!({"cwd":workspace})).await;
        assert_eq!(initial["thread"]["cwd"], workspace.to_str().unwrap());
        let destination = root.join("worktree storage");
        let settings = json!({"createOnNewSession":true,"copyOnCreate":true,"copyPaths":[".env"],"worktreeDirectory":destination});
        assert_eq!(request(&service, &mut session, "host/worktree/settings/update", settings.clone()).await, settings);
        let mut ids = Vec::new();
        let mut paths = Vec::new();
        for method in ["thread/start", "host/thread/start"] {
            let started = request(&service, &mut session, method, json!({"cwd":workspace,"model":"fixture-model"})).await;
            let thread = &started["thread"];
            let cwd = std::path::PathBuf::from(thread["cwd"].as_str().unwrap());
            assert_ne!(cwd, workspace);
            assert_eq!(cwd.parent().unwrap(), destination);
            assert_eq!(std::fs::read(cwd.join(".env")).unwrap(), std::fs::read(workspace.join(".env")).unwrap());
            assert_eq!(thread["projectId"], "workspace");
            assert_eq!(thread["model"], "fixture-model");
            ids.push(thread["id"].clone());
            paths.push(cwd);
        }
        assert_ne!(paths[0], paths[1]);
        let before = std::process::Command::new("git").current_dir(&workspace).args(["worktree", "list", "--porcelain"]).output().unwrap().stdout;
        for id in &ids {
            let read = request(&service, &mut session, "host/thread/read", json!({"threadId":id,"includeTurns":false})).await;
            assert_eq!(read["thread"]["projectId"], "workspace");
        }
        let restarted = CodexRpcService::new(server.clone(), DesktopProjectStore::new(&project_state));
        let mut restarted_session = restarted.open_session(64);
        assert_eq!(request(&restarted, &mut restarted_session, "host/worktree/settings/read", json!({})).await, settings);
        let listed = request(&restarted, &mut restarted_session, "host/thread/list", json!({"titleOnly":true})).await;
        for id in &ids {
            let thread = listed["data"].as_array().unwrap().iter().find(|thread| thread["id"] == *id).expect("worktree task must remain in the project list after restart");
            assert_eq!(thread["projectId"], "workspace");
        }
        let after = std::process::Command::new("git").current_dir(&workspace).args(["worktree", "list", "--porcelain"]).output().unwrap().stdout;
        assert_eq!(before, after, "opening and listing must not create worktrees");
        service.close_session(session.id());
        restarted.close_session(restarted_session.id());
        drop(session); drop(restarted_session); drop(service); drop(restarted);
        Arc::try_unwrap(server).ok().unwrap().shutdown().await.unwrap();
    }).await.expect("worktree integration exceeded 40 seconds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_edits_preserve_encoding_and_reject_stale_revisions() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let client = fixture.local().await;
        let files = directory.path().join("editable");
        std::fs::create_dir(&files).unwrap();
        let path = files.join("document.txt");
        std::fs::write(&path, b"\xef\xbb\xbffirst\r\n").unwrap();
        let original = rpc(&client.peer, "host/file/read", json!({"path":path})).await;
        assert_eq!(original["bom"], true);
        assert_eq!(original["lineEnding"], "crlf");
        let saved = rpc(
            &client.peer,
            "host/file/write",
            json!({"path":path,"revision":original["revision"],"text":"second\n"}),
        )
        .await;
        assert_eq!(saved["text"], "second\r\n");
        assert_eq!(std::fs::read(&path).unwrap(), b"\xef\xbb\xbfsecond\r\n");
        std::fs::write(&path, b"external edit").unwrap();
        assert!(
            client
                .peer
                .request::<_, Value>(
                    "host/file/write",
                    &json!({"path":path,"revision":saved["revision"],"text":"stale"})
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
        fixture.close().await;
    })
    .await
    .expect("file edit deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_model_wire_fixture() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("projects.json"), serde_json::to_vec(&json!({
            "local-projects":{"workspace":{"id":"workspace","name":"Workspace","rootPaths":[directory.path()]}}
        })).unwrap()).unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let local = fixture.local().await;
        let started = rpc(&local.peer, "host/thread/start", json!({"cwd":directory.path()})).await;
        let thread_id = &started["thread"]["id"];
        let mut events = local.peer.subscribe();
        rpc(&local.peer, "turn/start", json!({"threadId":thread_id,"input":[{"type":"text","text":"[items]"}]})).await;
        next_method(&mut events, "turn/completed").await;
        let list = rpc(&local.peer, "host/thread/list", json!({"titleOnly":true})).await;
        let history = rpc(&local.peer, "host/thread/read", json!({"threadId":thread_id,"includeTurns":true})).await;
        assert_eq!(list["projects"][0]["roots"][0]["path"], directory.path().to_str().unwrap());
        assert!(history["thread"]["turns"][0]["items"].as_array().unwrap().iter().any(|item| item["result"].is_object()));
        let capture = serde_json::to_string_pretty(&json!({"list":list,"history":history})).unwrap()
            .replace(directory.path().to_str().unwrap(), "/fixture/workspace");
        assert_eq!(serde_json::from_str::<Value>(&capture).unwrap(), serde_json::from_str::<Value>(include_str!("../../agent-core/tests/fixtures/daemon-wire.json")).unwrap());
        local.close().await;
        fixture.close().await;
    }).await.expect("wire fixture deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_handshakes_do_not_stop_the_host() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
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
        let local = fixture.local().await;
        assert_eq!(
            rpc(&local.peer, "thread/list", json!({})).await["data"],
            json!([])
        );
        stranger.close().await;
        local.close().await;
        fixture.close().await;
    })
    .await
    .expect("handshake isolation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upstream_exit_disconnects_store_and_stops_host() {
    use agent_core::{
        state::{Intent, Snapshot},
        store::Store,
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let local = fixture.local().await;
        let store = Store::new(local.peer, Snapshot::default());
        store
            .dispatch(Intent::ListThreads(Default::default()))
            .await
            .unwrap();
        assert!(store.snapshot().connected);
        let mut updates = store.subscribe();
        std::fs::write(directory.path().join("exit-on-list"), "").unwrap();
        assert!(
            store
                .dispatch(Intent::ListThreads(Default::default()))
                .await
                .is_err()
        );
        while updates.borrow_and_update().connected {
            updates.changed().await.unwrap();
        }
        assert!(updates.borrow().error.is_some());
        assert!(
            fixture
                .running
                .await
                .unwrap()
                .unwrap_err()
                .contains("event stream stopped")
        );
        store.close().await.ok();
        local.session.close();
        local.endpoint.close().await;
        Arc::try_unwrap(fixture.server)
            .ok()
            .unwrap()
            .shutdown()
            .await
            .unwrap();
    })
    .await
    .expect("upstream shutdown deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_invitation_is_rejected_by_daemon_and_remains_unconsumed() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let local = fixture.local().await;
        let invitation: PairingTicket =
            serde_json::from_value(rpc(&local.peer, "host/invite", json!({})).await).unwrap();
        let saved_keys = fixture.memory.clone();
        local.close().await;
        fixture.close().await;
        std::fs::remove_file(directory.path().join("bex-codex-fixture")).unwrap();
        let path = directory.path().join("state/trust.json");
        let mut trust: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        trust["trust"]["invitations"][invitation.invitation.to_string()] = json!(0);
        std::fs::write(&path, serde_json::to_vec(&trust).unwrap()).unwrap();
        // Reload this isolated Host's expired trust while preserving its key store.
        let fixture = Fixture::start_with_memory(directory.path(), saved_keys).await;
        let stranger = fixture.connect(Identity::generate()).await;
        assert!(
            stranger
                .peer
                .request::<_, Value>("host/pair", &json!({"invitation":invitation.invitation}))
                .await
                .is_err()
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["trust"]["invitations"][invitation.invitation.to_string()],
            0
        );
        stranger.endpoint.close().await;
        let local = fixture.local().await;
        assert_eq!(
            rpc(&local.peer, "host/status", json!({})).await["devices"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        local.close().await;
        fixture.close().await;
    })
    .await
    .expect("expired invitation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn passive_client_can_approve_after_five_minutes_without_reconnecting() {
    tokio::time::timeout(Duration::from_secs(330), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let sender = fixture.local().await;
        let started = rpc(&sender.peer, "thread/start", json!({"cwd":directory.path()})).await;
        let mut sender_events = sender.peer.subscribe();
        rpc(&sender.peer, "turn/start", json!({"threadId":started["thread"]["id"],"input":[{"type":"text","text":"[approval]"}]})).await;
        next_method(&mut sender_events, "item/commandExecution/requestApproval").await;
        // No dummy request: opening this peer must register it and replay approval.
        let passive = fixture.local().await;
        let mut events = passive.peer.subscribe();
        let request = next_method(&mut events, "item/commandExecution/requestApproval").await;
        tokio::time::sleep(Duration::from_secs(301)).await;
        passive.peer.respond_raw(&request["id"].to_string(), "result", r#"{"decision":"accept"}"#).await.unwrap();
        assert_eq!(next_method(&mut events, "turn/completed").await["params"]["turn"]["status"], "completed");
        passive.close().await;
        sender.close().await;
        fixture.close().await;
    }).await.expect("unattended approval deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpaired_connections_cannot_exhaust_authorized_session_slots() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let directory = tempfile::tempdir().unwrap();
        let fixture = Fixture::start(directory.path()).await;
        let mut strangers = Vec::new();
        for _ in 0..80 {
            let endpoint = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let session = endpoint.connect(&fixture.ticket).await.ok();
            strangers.push((endpoint, session));
        }
        let local = fixture.local().await;
        assert_eq!(
            rpc(&local.peer, "thread/list", json!({})).await["data"],
            json!([])
        );
        for (endpoint, session) in strangers {
            if let Some(session) = session {
                session.close();
            }
            endpoint.close().await;
        }
        local.close().await;
        fixture.close().await;
    })
    .await
    .expect("authorized admission deadline");
}
