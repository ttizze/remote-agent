use agent_transport::{
    peer::{JsonlReader, JsonlWriter},
    transport::Identity,
};
use host_fixture::{
    fixture::Config,
    pairing::PairingServer,
    test_support::{HostFixture, Memory},
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Stdio, sync::Arc, time::Duration};
use tokio::process::Command;
mod codex_fixture;
#[allow(dead_code)]
#[path = "support/conversations.rs"]
mod conversations;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn history_controls_publish_once_and_gate_canonical_reads_after_startup() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        fs::create_dir(&project).unwrap();
        host_fixture::test_support::seed_projects(
            &root,
            serde_json::from_value(json!([
                {"id":"project", "name":"Project", "roots":[{"path":project}]}
            ]))
            .unwrap(),
        )
        .unwrap();
        let host = HostFixture::start(
            &root,
            codex_fixture::config(&root),
            Arc::new(Memory::default()),
            "isolated Host",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let local = host.local().await.unwrap();
        assert!(
            conversations::Catalog::open(&local.peer)
                .await
                .snapshot
                .threads
                .is_empty()
        );
        let server = PairingServer::start(
            &root,
            host.service.clone(),
            host.ticket.clone(),
            host.credentials.local_identity().await,
        )
        .unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut public = None;
        for _ in 0..2 {
            let response = client
                .post(format!(
                    "http://127.0.0.1:{}/reading-conversation/after-startup",
                    server.port
                ))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 204);
            let identity: Value = client
                .post(format!(
                    "http://127.0.0.1:{}/session-identity/after-startup",
                    server.port
                ))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let session: agent_protocol::session::SessionRef =
                serde_json::from_value(identity["threadId"].clone()).unwrap();
            if let Some(previous) = public.replace(session.clone()) {
                assert_eq!(previous, session);
            }
            let catalog = conversations::Catalog::open(&local.peer).await;
            assert_eq!(catalog.snapshot.threads.len(), 1);
            assert_eq!(
                catalog.snapshot.threads[0].thread.id,
                session.application_id()
            );
            assert_eq!(
                catalog.snapshot.threads[0].thread.title,
                "Reading conversation"
            );
            assert_eq!(
                catalog.snapshot.threads[0].thread.project_id.as_deref(),
                Some("project")
            );
        }
        let session = public.unwrap();
        let subscribe = || agent_protocol::conversation::records::Subscribe {
            session: session.clone(),
            subscription: uuid::Uuid::new_v4(),
            after_sequence: None,
        };
        let base = format!("http://127.0.0.1:{}", server.port);
        assert_eq!(
            client
                .post(format!("{base}/hold-history-reads"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        let held_client = host.local().await.unwrap();
        let request = subscribe();
        let pending = tokio::spawn(async move {
            let result = held_client.peer.call(&request).await;
            held_client.close().await;
            result
        });
        while !root.join("conversation-read-held").exists() {
            tokio::task::yield_now().await;
        }
        assert!(!pending.is_finished());
        assert_eq!(
            client
                .post(format!("{base}/release-history-reads"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        let opened = pending.await.unwrap().unwrap();
        let snapshot = opened
            .initial
            .iter()
            .find_map(|item| match item {
                agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(snapshot) => {
                    Some(snapshot)
                }
                _ => None,
            })
            .expect("opening publishes the canonical history snapshot");
        assert!(snapshot.projection.visible_turn_items.iter().any(|row| {
            row.item
                .native_item_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some("after-startup:reading-35-user")
        }));
        assert!(snapshot.projection.visible_turn_items.iter().any(|row| {
            matches!(
                row.item.body,
                agent_protocol::orchestration::TurnItemBody::CommandExecution { .. }
            )
        }));
        assert!(snapshot.projection.provider_turns.iter().any(|turn| {
            turn.status == agent_protocol::orchestration::ProviderTurnStatus::Completed
        }));
        let cursor = snapshot
            .history_cursor
            .clone()
            .expect("the long conversation has an earlier page");
        assert_eq!(
            client
                .post(format!("{base}/fail-next-history-read"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert!(local.peer.call(&subscribe()).await.is_err());
        local.peer.call(&subscribe()).await.unwrap();
        assert_eq!(
            client
                .post(format!("{base}/fail-next-history-page"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        let earlier = agent_protocol::conversation::records::Extend { session, cursor };
        assert!(local.peer.call(&earlier).await.is_err());
        let page = local.peer.call(&earlier).await.unwrap();
        assert!(!page.nodes.is_empty());
        for row in &page.items {
            if let Some(id) = &row.item.node_id {
                let node = page
                    .nodes
                    .iter()
                    .find(|node| &node.id == id)
                    .expect("older items retain their execution node");
                assert!(page.nodes.iter().any(|root| root.id == node.root_node_id));
            }
        }
        server.shutdown().unwrap();
        local.close().await;
        host.close().await.unwrap();
    })
    .await
    .expect("late fixture history must reach the canonical list");
}

#[tokio::test]
async fn client_reply_controls_publish_live_events_and_persist_the_same_conversation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        fs::create_dir(root.join("project")).unwrap();
        let host = HostFixture::start(&root, codex_fixture::config(&root), Arc::new(Memory::default()),
            "isolated Host", false, None, None).await.unwrap();
        let local = host.local().await.unwrap();
        let server = PairingServer::start(&root, host.service.clone(), host.ticket.clone(),
            host.credentials.local_identity().await).unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://127.0.0.1:{}", server.port);
        let seeded: Value = http.post(format!("{base}/background-task")).send().await.unwrap()
            .error_for_status().unwrap().json().await.unwrap();
        let session: agent_protocol::session::SessionRef = serde_json::from_value(seeded["threadId"].clone()).unwrap();
        let mut watched = conversations::Conversation::open(&local.peer, &session).await.unwrap();
        assert_eq!(http.post(format!("{base}/background-reply/codex:{}", session.id)).send().await.unwrap().status(), 204);
        loop {
            let view = watched.next().await;
            if view.thread.turns.iter().flatten().any(|turn| {
                turn.status == agent_protocol::execution::TurnStatus::Completed && turn.items.iter().flatten()
                .any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { text, .. }
                    if text == "Latest reply from another client"))
            }) { break; }
        }
        let reopened = conversations::Conversation::open(&local.peer, &session).await.unwrap().view();
        assert_eq!(reopened.thread.id.as_ref(), Some(&session));
        assert!(reopened.thread.turns.iter().flatten().flat_map(|turn| turn.items.iter().flatten())
            .any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { text, .. }
                if text == "Latest reply from another client")));
        server.shutdown().unwrap();
        local.close().await;
        host.close().await.unwrap();
    }).await.expect("a real second-client reply must reach canonical history");
}

#[tokio::test]
async fn codex_fixture_uses_its_own_directory_instead_of_inherited_user_configuration() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let fixture = tempfile::tempdir().unwrap();
        let ambient = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(fixture.path()).unwrap();
        let expected = root.clone();
        let config = Config {
            trace: true,
            expected_cwd: Some(expected.clone()),
            stream_delay_ms: 0,
            ..Default::default()
        };
        let program = config
            .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), &root)
            .unwrap();
        let mut child = Command::new(program)
            .arg("app-server")
            .current_dir(ambient.path())
            .env("CODEX_HOME", ambient.path())
            .env(
                "AGENT_FAKE_CODEX_TRACE",
                ambient.path().join("unwanted-trace"),
            )
            .env("AGENT_FAKE_CODEX_EXPECTED_CWD", ambient.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut writer = JsonlWriter::new(child.stdin.take().unwrap());
        let mut reader = JsonlReader::new(child.stdout.take().unwrap());
        for (id, method, params) in [
            (
                1,
                "initialize",
                json!({"capabilities":{"experimentalApi":true}}),
            ),
            (2, "thread/start", json!({"cwd":expected})),
            (3, "thread/start", json!({"cwd":ambient.path()})),
            (4, "thread/start", json!({})),
        ] {
            writer
                .write_line(&json!({"id":id,"method":method,"params":params}).to_string())
                .await
                .unwrap();
            let response: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(response["id"], id);
            match id {
                1 => assert_eq!(response["result"]["codexHome"], json!(root)),
                2 | 4 => assert_eq!(response["result"]["thread"]["cwd"], json!(expected)),
                3 => assert_eq!(response["error"]["message"], "invalid cwd"),
                _ => unreachable!(),
            }
        }
        #[cfg(unix)]
        {
            // Updating this fixture root must leave its existing executable
            // image usable until the owning Host has closed that subprocess.
            config
                .install(Path::new(env!("CARGO_BIN_EXE_agent-codex-fixture")), &root)
                .unwrap();
            writer
                .write_line(&json!({"id":5,"method":"model/list","params":{}}).to_string())
                .await
                .unwrap();
            let response: Value =
                serde_json::from_str(&reader.read_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(response["id"], 5);
            assert_eq!(response["result"]["data"][0]["id"], "fixture-model");
        }
        child.kill().await.unwrap();
        assert!(root.join("rpc-trace.jsonl").exists());
        assert_eq!(
            fs::read_dir(ambient.path()).unwrap().count(),
            0,
            "inherited user paths must remain untouched"
        );
    })
    .await
    .expect("isolated Codex fixture exceeded its deadline");
}

#[tokio::test]
async fn pairing_controls_restore_the_original_project_store_and_survive_rejected_host_requests() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let fixture = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(fixture.path()).unwrap();
        let host = HostFixture::start(
            &root,
            codex_fixture::config(&root),
            Arc::new(Memory::default()),
            "isolated Host",
            false,
            None,
            None,
        )
        .await
        .unwrap();
        let server = PairingServer::start(
            &root,
            host.service.clone(),
            host.ticket.clone(),
            Identity::generate(),
        )
        .unwrap();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://127.0.0.1:{}", server.port);
        assert_eq!(
            client
                .get(format!("{base}/pairing"))
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
        assert_eq!(
            client
                .post(format!("{base}/list-fixture"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        let threads: Vec<Value> =
            serde_json::from_slice(&fs::read(root.join("list-fixture.json")).unwrap()).unwrap();
        assert!(!threads.is_empty());
        assert_eq!(
            host.service
                .fixture_projects()
                .fixture_projects()
                .unwrap()
                .len(),
            16
        );
        assert!(
            threads
                .iter()
                .all(|thread| Path::new(thread["cwd"].as_str().unwrap()).starts_with(&root))
        );
        assert_eq!(
            client
                .post(format!("{base}/list-fixture/reset"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert!(
            host.service
                .fixture_projects()
                .fixture_projects()
                .unwrap()
                .is_empty()
        );
        assert!(!root.join("list-fixture.json").exists());
        assert!(!root.join("projects-before-list-fixture.json").exists());
        assert_eq!(
            client
                .post(format!("{base}/list-fixture/reset"))
                .send()
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            client
                .get(format!("{base}/unknown"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        server.shutdown().unwrap();
        host.close().await.unwrap();
    })
    .await
    .expect("pairing controls exceeded their deadline");
}
