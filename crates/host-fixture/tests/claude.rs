use agent_core::{client as rpc, models};
use std::{path::Path, sync::Arc, time::Duration};

use agent_core::{
    client::Answer,
    state::{Attachment, Intent, Snapshot, operations as op},
    store::{Outcome, Store},
    transport::{Endpoint, Relays},
};
use codex_app_server::AppServerConfig;
use host_fixture::test_support::{HostFixture, Memory};
use serde_json::{Value, json};

mod codex_fixture;

async fn host(root: &Path, memory: Arc<Memory>, program: &Path) -> HostFixture {
    let existing = root.join("bex-codex-fixture");
    let config = if existing.exists() {
        AppServerConfig {
            program: existing,
            ..Default::default()
        }
    } else {
        codex_fixture::config(root)
    };
    HostFixture::start(
        root,
        config,
        memory,
        "Claude fixture Host",
        false,
        Some(program),
    )
    .await
    .unwrap()
}

async fn connect(host: &HostFixture, snapshot: Snapshot) -> (Store, Endpoint) {
    let endpoint = Endpoint::bind(host.credentials.local_identity().await, Relays::Disabled)
        .await
        .unwrap();
    let store = Store::connect(&endpoint, &host.ticket, snapshot, None)
        .await
        .unwrap();
    store
        .dispatch(Intent::LoadModels(op::LoadModels {}))
        .await
        .unwrap();
    (store, endpoint)
}

async fn draft(store: &Store, text: &str) {
    let key = store.snapshot().navigation.draft_key.clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: key,
            text: text.into(),
        })
        .await
        .unwrap();
}

async fn send(store: &Store, text: &str, client_id: &str) -> String {
    draft(store, text).await;
    let outcome = store
        .dispatch(Intent::Submit {
            thread_id: store.snapshot().navigation.thread_id.clone(),
            client_user_message_id: client_id.into(),
        })
        .await
        .unwrap();
    assert!(matches!(outcome, Outcome::Submitted { .. }));
    store.snapshot().navigation.thread_id.clone().unwrap()
}

async fn until(store: &Store, condition: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut updates = store.subscribe();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = updates.borrow_and_update().clone();
            if condition(&snapshot) {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Claude state did not settle: error={:?}, turns={:?}, requests={:?}",
            store.snapshot().error,
            store
                .snapshot()
                .conversations
                .values()
                .flat_map(|thread| thread.turns.iter().flatten())
                .map(|turn| (&turn.id, &turn.status, &turn.error))
                .collect::<Vec<_>>(),
            store
                .snapshot()
                .requests
                .values()
                .map(|request| (&request.id, &request.params))
                .collect::<Vec<_>>()
        )
    })
}

async fn completed(store: &Store, id: &str, count: usize, status: &str) -> Arc<Snapshot> {
    until(store, |snapshot| {
        snapshot
            .conversations
            .get(id)
            .and_then(|thread| thread.turns.as_ref())
            .is_some_and(|turns| {
                turns.len() == count && turns.last().unwrap().status.as_deref() == Some(status)
            })
    })
    .await
}

fn fixture_program() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_bex-claude-fixture"))
}

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_submission_preserves_inputs_settings_workspaces_and_history_across_host_restart() {
    tokio::time::timeout(Duration::from_secs(120), async {
        for automatic in [false, true] {
            for selected in [false, true] {
                for attachment in ["none", "image", "file"] {
                    let root = tempfile::tempdir().unwrap();
                    let root = root.path().canonicalize().unwrap();
                    let workspace = root.join("project");
                    std::fs::create_dir(&workspace).unwrap();
                    git(&workspace, &["init", "--quiet"]);
                    std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
                    git(&workspace, &["add", "tracked.txt"]);
                    git(&workspace, &["-c","user.name=Fixture","-c","user.email=fixture@example.invalid","-c","commit.gpgsign=false","commit","--quiet","-m","fixture"]);
                    std::fs::write(root.join("projects.json"), json!([{"id":"project","name":"Project","roots":[{"path":workspace}]}]).to_string()).unwrap();
                    std::fs::write(root.join("bex-worktrees.json"), json!({"settings":{"createOnNewSession":automatic,"worktreeDirectory":root.join("worktrees")}}).to_string()).unwrap();
                    let memory = Arc::new(Memory::default());
                    let mut fixture = host(&root, memory.clone(), fixture_program()).await;
                    let (mut store, mut endpoint) = connect(&fixture, Snapshot::default()).await;
                    store.dispatch(Intent::NewChat { cwd: if selected { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                    let key = store.snapshot().navigation.draft_key.clone();
                    store.dispatch(Intent::SelectModel { thread_id: key.clone(), model: "claude:default".into() }).await.unwrap();
                    store.dispatch(Intent::SelectEffort { thread_id: key, effort: "low".into() }).await.unwrap();
                    let mut previous_id = None;
                    let mut previous_cwd = None;
                    for number in 0..2 {
                        let key = store.snapshot().navigation.draft_key.clone();
                        if attachment != "none" {
                            let path = root.join(if attachment == "image" { "photo.png" } else { "note.txt" });
                            if attachment == "image" {
                                std::fs::write(&path, include_bytes!("../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png")).unwrap();
                            } else { std::fs::write(&path, "attachment content").unwrap(); }
                            store.dispatch(Intent::UploadAttachment(op::UploadAttachment {
                                draft_key: key, directory: store.snapshot().navigation.cwd.clone(),
                                attachment: Attachment { path: path.to_str().unwrap().into(), name: path.file_name().unwrap().to_str().unwrap().into(), is_image: attachment == "image" },
                            })).await.unwrap();
                        }
                        let id = send(&store, &format!("message {number}"), &format!("client-{number}")).await;
                        if let Some(previous) = &previous_id { assert_eq!(&id, previous); }
                        let snapshot = completed(&store, &id, number + 1, "completed").await;
                        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
                        assert!(snapshot.pending_submissions.is_empty());
                        assert!(snapshot.drafts[&id].text.is_empty() && snapshot.drafts[&id].attachments.is_empty());
                        assert_eq!(snapshot.drafts[&id].model.as_deref(), Some("claude:default"));
                        assert_eq!(snapshot.drafts[&id].effort.as_deref(), Some("low"));
                        let cwd = snapshot.navigation.cwd.clone();
                        if let Some(previous) = &previous_cwd { assert_eq!(&cwd, previous); }
                        if selected && automatic {
                            assert_eq!(Path::new(&cwd).file_name(), workspace.file_name());
                            assert_eq!(Path::new(&cwd).parent().unwrap().parent().unwrap(), root.join("worktrees"));
                        }
                        else { assert_eq!(Path::new(&cwd), if selected { workspace.clone() } else { root.join("bex-chats") }); }
                        let turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[number];
                        let items = turn.items.as_ref().unwrap();
                        assert!(items.iter().any(|item| item.kind.as_deref() == Some("reasoning") && item.text.as_deref() == Some("Fixture reasoning")));
                        let responses: Vec<_> = items.iter().filter(|item| item.kind.as_deref() == Some("agentMessage")).collect();
                        assert_eq!(responses.len(), 1, "streaming and completed blocks must not duplicate");
                        assert!(responses[0].text.as_ref().unwrap().starts_with(&format!("reply {}: message {number}", number + 1)));
                        let user = items.iter().find(|item| item.kind.as_deref() == Some("userMessage")).unwrap();
                        assert_eq!(user.client_id.as_deref(), Some(format!("client-{number}").as_str()));
                        assert_eq!(user.content.as_ref().unwrap()[0]["text"], format!("message {number}"));
                        let session = id.strip_prefix("claude:").unwrap();
                        let inputs: Value = serde_json::from_slice(&std::fs::read(Path::new(&cwd).join(format!("claude-session-{session}.json"))).unwrap()).unwrap();
                        assert_eq!(inputs.as_array().unwrap().len(), number + 1);
                        assert_eq!(inputs[number]["effort"], "low");
                        assert_eq!(inputs[number]["content"].as_array().unwrap().len(), if attachment == "none" { 1 } else { 2 });
                        if attachment == "image" { assert_eq!(inputs[number]["content"][1]["source"]["media_type"], "image/png"); }
                        if attachment == "file" { assert!(inputs[number]["content"][1]["text"].as_str().unwrap().contains("note.txt")); }
                        store.dispatch(Intent::ListThreads(op::ListThreads::new(Default::default()))).await.unwrap();
                        assert!(store.snapshot().threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_deref() == Some(&id)));
                        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                        assert_eq!(store.snapshot().conversations[&id].turns.as_ref().unwrap().len(), number + 1);
                        let saved: Snapshot = serde_json::from_slice(&serde_json::to_vec(store.snapshot().as_ref()).unwrap()).unwrap();
                        store.close().await.unwrap();
                        endpoint.close().await;
                        fixture.close().await.unwrap();
                        fixture = host(&root, memory.clone(), fixture_program()).await;
                        (store, endpoint) = connect(&fixture, saved).await;
                        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                        assert_eq!(store.snapshot().conversations[&id].turns.as_ref().unwrap().len(), number + 1);
                        previous_id = Some(id);
                        previous_cwd = Some(cwd);
                    }
                    store.close().await.unwrap();
                    endpoint.close().await;
                    fixture.close().await.unwrap();
                }
            }
        }
    }).await.expect("Claude input and restart matrix deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_approval_snapshot_after_disconnect_denial_is_effective_and_interrupt_recovers() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: "claude:default".into(),
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "permission-1").await;
        until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        let cwd = store.snapshot().navigation.cwd.clone();
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None)
            .await
            .unwrap();
        let snapshot = until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        let request = snapshot.requests.values().next().unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Decision { index: 1 },
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        assert!(snapshot.requests.is_empty());
        send(&store, "permission", "permission-2").await;
        let snapshot = until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        let request = snapshot.requests.values().next().unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Decision { index: 0 },
            }))
            .await
            .unwrap();
        completed(&store, &id, 2, "completed").await;
        assert_eq!(
            std::fs::read_to_string(Path::new(&cwd).join("approved.txt")).unwrap(),
            "approved"
        );
        send(&store, "question", "question").await;
        let snapshot = until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        let request = snapshot.requests.values().next().unwrap();
        assert_eq!(request.method, "item/tool/requestUserInput");
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Questions {
                    answers: [("Which color?".into(), "Blue".into())].into(),
                },
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 3, "completed").await;
        assert!(
            snapshot.conversations[&id].turns.as_ref().unwrap()[2]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item.text.as_deref() == Some("Blue"))
        );
        send(&store, "wait", "wait").await;
        let snapshot = until(&store, |snapshot| {
            snapshot.conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .last()
                .unwrap()
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item.text.as_deref() == Some("Waiting for interruption"))
        })
        .await;
        let turn_id = snapshot.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .last()
            .unwrap()
            .id
            .clone();
        send(&store, "wait", "busy").await;
        assert!(store.snapshot().error.is_none());
        assert!(store.snapshot().drafts[&id].text.is_empty());
        until(&store, |snapshot| {
            !snapshot.pending_submissions.contains_key("busy")
        })
        .await;
        let local = fixture.local().await.unwrap();
        let previous_turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[2].id;
        assert!(
            local
                .peer
                .call(
                    &serde_json::from_value::<op::Interrupt>(
                        json!({"threadId": id, "turnId": previous_turn}),
                    )
                    .unwrap()
                )
                .await
                .is_err(),
            "a delayed stop must not interrupt the next native turn"
        );
        assert_eq!(
            store.snapshot().conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .last()
                .unwrap()
                .status
                .as_deref(),
            Some("inProgress")
        );
        store
            .dispatch(Intent::Interrupt(op::Interrupt {
                thread_id: id.clone(),
                turn_id,
            }))
            .await
            .unwrap();
        completed(&store, &id, 4, "interrupted").await;
        send(&store, "after interruption", "recovered").await;
        let snapshot = completed(&store, &id, 5, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.requests.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.drafts[&id].text.is_empty());
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("Claude permission and interruption deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provider_selection_cannot_redirect_an_existing_conversation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        for (model, display) in [
            ("default", "Default (recommended) · Opus 5 with 1M context"),
            ("opus[1m]", "Opus 5 with 1M context"),
            ("claude-fable-5-1[1m]", "Fable 5.1"),
            ("sonnet", "Sonnet 5"),
            ("haiku", "Haiku 4.5"),
            ("custom", "Custom model"),
        ] {
            let snapshot = store.snapshot();
            let entry = snapshot
                .models
                .iter()
                .find(|entry| entry.model == format!("claude:{model}"))
                .unwrap();
            assert_eq!(entry.display_name, format!("Claude · {display}"));
        }
        let codex_model = store
            .snapshot()
            .models
            .iter()
            .find(|model| !model.model.starts_with("claude:"))
            .unwrap()
            .model
            .clone();
        for (original, other) in [
            (codex_model.as_str(), "claude:default"),
            ("claude:default", codex_model.as_str()),
        ] {
            store
                .dispatch(Intent::NewChat { cwd: String::new() })
                .await
                .unwrap();
            let key = store.snapshot().navigation.draft_key.clone();
            store
                .dispatch(Intent::SelectModel {
                    thread_id: key,
                    model: original.into(),
                })
                .await
                .unwrap();
            let id = send(&store, "original provider", &format!("{original}-start")).await;
            completed(&store, &id, 1, "completed").await;

            store
                .dispatch(Intent::SelectModel {
                    thread_id: id.clone(),
                    model: other.into(),
                })
                .await
                .unwrap();
            draft(&store, "keep this input").await;
            assert!(
                store
                    .dispatch(Intent::Submit {
                        thread_id: Some(id.clone()),
                        client_user_message_id: format!("{original}-mismatch"),
                    })
                    .await
                    .is_err()
            );
            let snapshot = store.snapshot();
            assert_eq!(snapshot.navigation.thread_id.as_deref(), Some(id.as_str()));
            assert_eq!(snapshot.drafts[&id].text, "keep this input");
            assert!(snapshot.error.as_ref().unwrap().contains("新しい会話"));
            assert!(snapshot.pending_submissions.is_empty());
            assert_eq!(snapshot.conversations[&id].turns.as_ref().unwrap().len(), 1);

            store
                .dispatch(Intent::SelectModel {
                    thread_id: id.clone(),
                    model: original.into(),
                })
                .await
                .unwrap();
            send(
                &store,
                "continue with original provider",
                &format!("{original}-retry"),
            )
            .await;
            let snapshot = completed(&store, &id, 2, "completed").await;
            assert!(snapshot.error.is_none());
        }
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("provider selection deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_claude_keeps_codex_usable() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(
            root.path(),
            Arc::new(Memory::default()),
            &root.path().join("missing-claude"),
        )
        .await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        assert!(!store.snapshot().models.is_empty());
        assert!(
            store
                .snapshot()
                .models
                .iter()
                .all(|model| !model.model.starts_with("claude:"))
        );
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let id = send(&store, "Codex remains available", "codex-only").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&id].text.is_empty() && snapshot.pending_submissions.is_empty());
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("missing Claude deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_codex_keeps_claude_inputs_workspaces_and_resumed_history_usable() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for automatic in [false, true] {
            for selected in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let root = root.path().canonicalize().unwrap();
                let workspace = root.join("project");
                std::fs::create_dir(&workspace).unwrap();
                git(&workspace, &["init", "--quiet"]);
                std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
                git(&workspace, &["add", "tracked.txt"]);
                git(&workspace, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture"]);
                std::fs::write(root.join("projects.json"), json!([{"id":"project","name":"Project","roots":[{"path":workspace}]}]).to_string()).unwrap();
                std::fs::write(root.join("bex-worktrees.json"), json!({"settings":{"createOnNewSession":automatic,"worktreeDirectory":root.join("worktrees")}}).to_string()).unwrap();
                let config = AppServerConfig { program: root.join("missing-codex"), ..Default::default() };
                let memory = Arc::new(Memory::default());
                let mut saved = Snapshot::default();
                let mut thread_id: Option<String> = None;
                let mut cwd = None;
                for index in 0..2 {
                    let fixture = HostFixture::start(&root, config.clone(), memory.clone(), "Independent Host", false, Some(fixture_program())).await.unwrap();
                    let (store, endpoint) = connect(&fixture, saved).await;
                    assert!(store.snapshot().connected);
                    assert!(store.snapshot().model_errors.contains_key("codex"));
                    assert!(store.snapshot().models.iter().all(|model| model.model.starts_with("claude:")));
                    if let Some(id) = &thread_id {
                        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                    } else {
                        store.dispatch(Intent::NewChat { cwd: if selected { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                        let key = store.snapshot().navigation.draft_key.clone();
                        store.dispatch(Intent::SelectModel { thread_id: key, model: "claude:default".into() }).await.unwrap();
                    }
                    let id = send(&store, &format!("independent {index}"), &format!("independent-{index}")).await;
                    let snapshot = completed(&store, &id, index + 1, "completed").await;
                    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
                    assert!(snapshot.drafts[&id].text.is_empty() && snapshot.pending_submissions.is_empty());
                    let current = snapshot.conversations[&id].cwd.clone().unwrap();
                    if let Some(previous) = &cwd { assert_eq!(previous, &current); }
                    if selected && automatic {
                        assert_eq!(Path::new(&current).file_name(), workspace.file_name());
                        assert_eq!(Path::new(&current).parent().unwrap().parent().unwrap(), root.join("worktrees"));
                    }
                    else { assert_eq!(Path::new(&current), if selected { workspace.clone() } else { root.join("bex-chats") }); }
                    let turns = snapshot.conversations[&id].turns.as_ref().unwrap();
                    assert!(turns[index].items.as_ref().unwrap().iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| text.starts_with(&format!("reply {}: independent {index}", index + 1)))));
                    store.dispatch(Intent::ListThreads(op::ListThreads::new(Default::default()))).await.unwrap();
                    let snapshot = store.snapshot();
                    let list = snapshot.threads.as_ref().unwrap();
                    assert!(list.data.iter().any(|thread| thread.id.as_deref() == Some(&id)));
                    assert!(list.provider_errors.as_ref().unwrap()["codex"]["message"].is_string());
                    let management = fixture.local().await.unwrap();
                    let status = management.peer.call(&rpc::ReadHostStatus {}).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                    assert!(status["providerErrors"]["codex"]["message"].is_string());
                    if selected {
                        let path = Path::new(&current).join("tracked.txt");
                        let listed = management.peer.call(&serde_json::from_value::<op::ListFiles>(json!({"path":current})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        assert!(listed["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "tracked.txt"));
                        let read = management.peer.request::<models::FileContent>(&agent_core::protocol::Call::ReadFile(serde_json::from_value::<op::ListFiles>(json!({"path":path})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        let contents = format!("workspace edit {index}\n");
                        let saved_file = management.peer.call(&serde_json::from_value::<rpc::WriteFile>(json!({"path":path,"revision":read["revision"],"text":contents})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        assert_eq!(saved_file["text"], contents);
                        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
                        let review = management.peer.call(&serde_json::from_value::<rpc::ReviewWorkspace>(json!({"cwd":current})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        assert!(review["files"].as_array().unwrap().iter().any(|file| file["path"] == "tracked.txt"));
                    }
                    let drafts = store.snapshot().drafts.clone();
                    let dictation = management.peer.request::<rpc::Transcription>(&agent_core::protocol::Call::Transcribe(serde_json::from_value::<rpc::Transcribe>(json!({"audio":"AAA="})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap());
                    assert!(dictation.is_err());
                    assert_eq!(*store.snapshot().drafts, *drafts);
                    assert!(management.peer.call(&op::ReadWorktreeSettings {}).await.is_ok());
                    let count_worktrees = || std::fs::read_dir(root.join("worktrees")).map(|entries| entries.count()).unwrap_or_default();
                    let before = count_worktrees();
                    assert!(management.peer.call(&serde_json::from_value::<op::StartThread>(json!({"model":"fixture-model","cwd":current})).unwrap()).await.is_err());
                    assert_eq!(count_worktrees(), before, "an unavailable backend must not create a worktree");
                    assert!(management.peer.call(&rpc::ReadHostStatus {}).await.is_ok());
                    management.close().await;
                    saved = serde_json::from_slice(&serde_json::to_vec(snapshot.as_ref()).unwrap()).unwrap();
                    thread_id = Some(id);
                    cwd = Some(current);
                    store.close().await.unwrap();
                    endpoint.close().await;
                    fixture.close().await.unwrap();
                }
            }
        }
    }).await.expect("independent Claude deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_exit_preserves_claude_approval_and_completes_after_reconnect() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let codex_id = send(&store, "[approval]", "codex-approval").await;
        until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: "claude:default".into(),
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "claude-approval").await;
        until(&store, |snapshot| snapshot.requests.len() == 2).await;
        let cwd = store.snapshot().navigation.cwd.clone();
        store
            .dispatch(Intent::StartTerminal(
                serde_json::from_value(json!({
                    "processHandle":"codex-terminal", "cwd":cwd, "size":{"rows":24,"cols":80}
                }))
                .unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(
            store.snapshot().terminals["codex-terminal"].phase,
            agent_core::state::TerminalPhase::Running
        );
        let local = fixture.local().await.unwrap();
        std::fs::write(root.path().join("exit-on-list"), "").unwrap();
        assert!(
            local
                .peer
                .request::<agent_core::protocol::json_boundary::Opaque>(
                    &agent_core::protocol::Call::Provider(agent_core::protocol::ProviderCall {
                        method: "thread/list".into(),
                        params: json!({})
                    })
                )
                .await
                .is_err()
        );
        until(&store, |snapshot| {
            snapshot.requests.len() == 1
                && snapshot.conversations[&codex_id].turns.as_ref().unwrap()[0]
                    .status
                    .as_deref()
                    == Some("failed")
        })
        .await;
        assert!(store.snapshot().connected);
        assert!(!fixture.running.is_finished());
        assert!(!store.snapshot().activity.active[&codex_id]);
        assert!(
            matches!(
                store.snapshot().terminals["codex-terminal"].phase,
                agent_core::state::TerminalPhase::Running
            ),
            "Host-owned terminals must survive Codex exit"
        );
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None)
            .await
            .unwrap();
        let snapshot = until(&store, |snapshot| !snapshot.requests.is_empty()).await;
        assert_eq!(snapshot.requests.len(), 1);
        let request = snapshot.requests.values().next().unwrap();
        assert_eq!(request.params["threadId"], id);
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Decision { index: 0 },
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.requests.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.drafts[&id].text.is_empty());
        assert_eq!(
            std::fs::read_to_string(Path::new(&cwd).join("approved.txt")).unwrap(),
            "approved"
        );
        store
            .dispatch(Intent::LoadModels(op::LoadModels {}))
            .await
            .unwrap();
        assert!(store.snapshot().model_errors.contains_key("codex"));
        send(&store, "after Codex exit", "claude-after-exit").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.drafts[&id].text.is_empty());
        assert!(local.peer.call(&rpc::ReadHostStatus {}).await.is_ok());
        local.close().await;
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("Codex exit isolation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_authentication_and_inference_failures_are_visible_and_retry_preserves_the_conversation()
 {
    tokio::time::timeout(Duration::from_secs(45), async {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("bex-chats");
        std::fs::create_dir(&cwd).unwrap();
        let config = cwd.join("claude-fixture.json");
        std::fs::write(&config, json!({"unauthenticated":true}).to_string()).unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: "claude:default".into(),
            })
            .await
            .unwrap();
        draft(&store, "keep my draft").await;
        assert!(
            store
                .dispatch(Intent::Submit {
                    thread_id: None,
                    client_user_message_id: "auth-failure".into()
                })
                .await
                .is_err()
        );
        let snapshot = store.snapshot();
        let id = snapshot.navigation.thread_id.clone().unwrap();
        assert!(snapshot.error.as_ref().unwrap().contains("サブスク認証"));
        assert_eq!(snapshot.drafts[&id].text, "keep my draft");
        assert!(snapshot.pending_submissions.is_empty());
        assert!(
            snapshot.conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .is_empty()
        );
        std::fs::write(&config, json!({"initializeError":true}).to_string()).unwrap();
        assert!(
            store
                .dispatch(Intent::Submit {
                    thread_id: Some(id.clone()),
                    client_user_message_id: "init-failure".into()
                })
                .await
                .is_err()
        );
        assert!(
            store
                .snapshot()
                .error
                .as_ref()
                .unwrap()
                .contains("fixture initialization failed")
        );
        assert_eq!(store.snapshot().drafts[&id].text, "keep my draft");
        assert!(
            store.snapshot().conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .is_empty()
        );
        std::fs::write(&config, json!({"resultError":true}).to_string()).unwrap();
        send(&store, "inference failure", "failed-turn").await;
        let snapshot = completed(&store, &id, 1, "failed").await;
        assert_eq!(
            snapshot.conversations[&id].turns.as_ref().unwrap()[0]
                .error
                .as_ref()
                .unwrap()["message"],
            "fixture inference failed"
        );
        std::fs::write(&config, "{}").unwrap();
        send(&store, "retry", "retry").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.drafts[&id].text.is_empty());
        assert!(
            snapshot.conversations[&id].turns.as_ref().unwrap()[1]
                .error
                .is_none()
        );
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("Claude failure recovery deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "uses the Host user's authenticated Claude subscription; run explicitly"]
async fn live_claude_subscription_completes_and_resumes_through_store_and_host() {
    tokio::time::timeout(Duration::from_secs(150), async {
        let program = std::env::var_os("BEX_LIVE_CLAUDE_PROGRAM").expect("set an absolute Claude Code executable path");
        let root = tempfile::tempdir().unwrap();
        let memory = Arc::new(Memory::default());
        let fixture = host(root.path(), memory.clone(), Path::new(&program)).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key, model: "claude:haiku".into() }).await.unwrap();
        let id = send(&store, "Remember marker BEX_CLAUDE_STORE_OK. Reply with exactly that marker. Do not use tools.", "live-1").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&id].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| text.contains("BEX_CLAUDE_STORE_OK"))));
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
        let fixture = host(root.path(), memory, Path::new(&program)).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
        send(&store, "Reply with exactly the marker from my previous message. Do not use tools.", "live-2").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&id].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[1].items.as_ref().unwrap().iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| text.contains("BEX_CLAUDE_STORE_OK"))));
        send(&store, "Count from 1 to 10000, one number per line. Do not use tools.", "live-stop").await;
        let snapshot = until(&store, |snapshot| snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 3 && turns[2].items.as_ref().is_some_and(|items| items.iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| !text.is_empty()))))).await;
        let turn_id = snapshot.conversations[&id].turns.as_ref().unwrap()[2].id.clone();
        store.dispatch(Intent::Interrupt(op::Interrupt { thread_id: id.clone(), turn_id })).await.unwrap();
        let snapshot = completed(&store, &id, 3, "interrupted").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        send(&store, "Reply with exactly BEX_CLAUDE_RECOVERED. Do not use tools.", "live-recovery").await;
        let snapshot = completed(&store, &id, 4, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&id].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[3].items.as_ref().unwrap().iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| text.contains("BEX_CLAUDE_RECOVERED"))));
        send(&store, "Count from 1 to 100, one number per line. Do not use tools.", "live-before-additional").await;
        until(&store, |snapshot| snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 5 && turns[4].items.as_ref().is_some_and(|items| items.iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| !text.is_empty()))))).await;
        assert!(agent_core::session::input_unavailable_reason(&store.snapshot().conversations[&id]).is_none());
        send(&store, "Reply with exactly BEX_CLAUDE_ADDITIONAL_OK. Do not use tools.", "live-additional").await;
        let snapshot = completed(&store, &id, 5, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.pending_submissions.is_empty());
        let items = snapshot.conversations[&id].turns.as_ref().unwrap()[4].items.as_ref().unwrap();
        assert_eq!(items.iter().filter(|item| item.client_id.as_deref() == Some("live-additional")).count(), 1);
        assert!(items.iter().any(|item| item.kind.as_deref() == Some("agentMessage") && item.text.as_ref().is_some_and(|text| text.contains("BEX_CLAUDE_ADDITIONAL_OK"))));
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    }).await.expect("live Claude subscription deadline");
}

#[tokio::test]
async fn consecutive_claude_inputs_reuse_one_native_process() {
    let root = tempfile::tempdir().unwrap();
    let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
    let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    store
        .dispatch(Intent::SelectModel {
            thread_id: store.snapshot().navigation.draft_key.clone(),
            model: "claude:default".into(),
        })
        .await
        .unwrap();
    let id = send(&store, "first", "reuse-1").await;
    completed(&store, &id, 1, "completed").await;
    send(&store, "second", "reuse-2").await;
    let snapshot = completed(&store, &id, 2, "completed").await;
    let native = id.strip_prefix("claude:").unwrap();
    let inputs: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(&snapshot.navigation.cwd).join(format!("claude-session-{native}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(inputs[0]["pid"].is_u64());
    assert_eq!(
        inputs[0]["pid"], inputs[1]["pid"],
        "consecutive inputs must use the same CLI process"
    );
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_codex_terminal_is_owned_by_its_connection_and_supports_io_resize_and_kill() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = HostFixture::start(
            root.path(),
            AppServerConfig {
                program: root.path().join("missing-codex"),
                ..Default::default()
            },
            Arc::new(Memory::default()),
            "PTY fixture",
            false,
            Some(fixture_program()),
        )
        .await
        .unwrap();
        let mut owner = fixture.local().await.unwrap();
        let stranger = fixture.local().await.unwrap();
        owner
            .peer
            .call(&rpc::StartTerminal {
                handle: "owned".into(),
                cwd: root.path().to_str().unwrap().into(),
                size: rpc::TerminalSize { rows: 24, cols: 80 },
            })
            .await
            .unwrap();
        let resize = op::ResizeTerminal {
            handle: "owned".into(),
            size: rpc::TerminalSize { rows: 39, cols: 97 },
        };
        let kill = rpc::TerminalKill {
            process_handle: "owned".into(),
        };
        #[cfg(unix)]
        let input = b"printf 'BEX_%s\\n' 'PTY_READY'; stty size\n".as_slice();
        #[cfg(windows)]
        let input = b"echo BEX_PTY_READY\r\n".as_slice();
        let write = rpc::TerminalWrite {
            process_handle: "owned".into(),
            data: input.to_vec(),
        };
        assert!(
            stranger
                .peer
                .request::<models::Empty>(&agent_core::protocol::Call::WriteTerminal(write.clone()))
                .await
                .is_err()
        );
        assert!(stranger.peer.call(&resize).await.is_err());
        assert!(
            stranger
                .peer
                .request::<models::Empty>(&agent_core::protocol::Call::KillTerminal(kill.clone()))
                .await
                .is_err()
        );
        owner.peer.call(&resize).await.unwrap();
        owner
            .peer
            .request::<models::Empty>(&agent_core::protocol::Call::WriteTerminal(write))
            .await
            .unwrap();
        let mut output = Vec::new();
        loop {
            let message = owner
                .events
                .read::<agent_core::protocol::Notification>()
                .await
                .unwrap()
                .expect("Host event stream ended");
            if let agent_core::protocol::Notification::Output { data, .. } = message {
                output.extend(data);
                let text = String::from_utf8_lossy(&output);
                if text.contains("BEX_PTY_READY") {
                    #[cfg(unix)]
                    if !text.contains("39 97") {
                        continue;
                    }
                    break;
                }
            }
        }
        owner
            .peer
            .request::<models::Empty>(&agent_core::protocol::Call::KillTerminal(kill))
            .await
            .unwrap();
        // A successful kill includes process cleanup and ownership release.
        assert!(owner.peer.call(&resize).await.is_err());
        owner.peer.call(&rpc::ReadHostStatus {}).await.unwrap();
        stranger.close().await;
        owner.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("independent PTY deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_accounts_login_switch_resume_cancel_and_logout_without_codex() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let native = root.join("claude-native");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("fixture-auth.json"), json!({"loggedIn":true,"authMethod":"claude.ai","email":"native@example.invalid","subscriptionType":"pro"}).to_string()).unwrap();
        let memory = Arc::new(Memory::default());
        let start = || HostFixture::start(&root, AppServerConfig { program: root.join("missing-codex"), ..Default::default() }, memory.clone(), "Claude accounts", false, Some(fixture_program()));
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        std::fs::write(native.join("usage-paused"), "").unwrap();
        tokio::time::timeout(Duration::from_secs(2), store.dispatch(Intent::ListAccounts(op::ListAccounts {})))
            .await.expect("listing must not wait for usage").unwrap();
        while !native.join("usage-requested").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let key = store.snapshot().navigation.draft_key.clone();
        tokio::time::timeout(Duration::from_secs(2), store.dispatch(Intent::SelectAccountForDraft(op::SelectAccountForDraft {
            id: "claude:desktop".into(), thread_id: key,
        }))).await.expect("selection and model loading must not wait for usage").unwrap();
        assert!(store.snapshot().account.accounts.as_ref().unwrap().accounts[0].usage.is_none());
        std::fs::remove_file(native.join("usage-paused")).unwrap();
        until(&store, |snapshot| snapshot.account.accounts.as_ref().is_some_and(|accounts| accounts.accounts[0].usage.is_some())).await;
        let usage = store.snapshot().account.accounts.as_ref().unwrap().accounts[0].usage.clone().unwrap();
        assert_eq!(usage.windows[0].remaining_percent, 28);
        assert_eq!(usage.windows[1].remaining_percent, 61);

        assert_eq!(store.snapshot().account.accounts.as_ref().unwrap().selected_claude_id.as_deref(), Some("claude:desktop"));
        store.dispatch(Intent::NewChat { cwd: root.to_string_lossy().into() }).await.unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key, model: "claude:default".into() }).await.unwrap();
        let thread = send(&store, "first account", "account-first").await;
        completed(&store, &thread, 1, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { provider: agent_core::session::ProviderKind::Claude })).await.unwrap();
        let login = store.snapshot().account.login.clone().unwrap();
        assert!(login.requires_code_submission);
        assert!(login.verification_url.starts_with("https://claude.com/"));
        assert!(store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { id: "claude:wrong".into(), code: "fixture-code".into() })).await.is_err());
        store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { id: login.login_id.clone(), code: "fixture-code".into() })).await.unwrap();
        loop {
            store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin { id: login.login_id.clone() })).await.unwrap();
            if store.snapshot().account.login.is_none() { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        until(&store, |snapshot| snapshot.account.accounts.as_ref().is_some_and(|accounts| accounts.selected_claude_id.as_ref() == Some(&login.login_id))).await;
        send(&store, "second account, same history", "account-second").await;
        completed(&store, &thread, 2, "completed").await;
        let profile = root.join("claude/accounts").join(login.login_id.strip_prefix("claude:").unwrap());
        assert_eq!(std::fs::canonicalize(profile.join("projects")).unwrap(), std::fs::canonicalize(native.join("projects")).unwrap());
        let homes = std::fs::read_to_string(root.join("claude-auth-homes.jsonl")).unwrap();
        assert!(homes.lines().any(|line| line == native.to_string_lossy()));
        assert!(homes.lines().any(|line| line == profile.to_string_lossy()), "retained process must not reuse the previous account");
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, snapshot).await;
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert_eq!(store.snapshot().account.accounts.as_ref().unwrap().selected_claude_id.as_ref(), Some(&login.login_id));
        send(&store, "resumed after restart", "account-third").await;
        completed(&store, &thread, 3, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { provider: agent_core::session::ProviderKind::Claude })).await.unwrap();
        let canceled = store.snapshot().account.login.clone().unwrap();
        store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin { id: canceled.login_id.clone() })).await.unwrap();
        assert!(!root.join("claude/accounts").join(canceled.login_id.strip_prefix("claude:").unwrap()).exists());
        assert!(native.join("projects").exists(), "cancel must preserve shared history");
        store.dispatch(Intent::LogoutAccount(op::LogoutAccount { id: login.login_id.clone() })).await.unwrap();
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(store.snapshot().account.accounts.as_ref().unwrap().selected_claude_id.is_none());
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, snapshot, None).await.unwrap();
        assert!(store.dispatch(Intent::LoadModels(op::LoadModels {})).await.is_err(), "logged-out Claude and unavailable Codex must not expose models");
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(store.snapshot().account.accounts.as_ref().unwrap().selected_claude_id.is_none(), "restart must preserve logout without selecting the native account");
        store.dispatch(Intent::SelectAccount(op::SelectAccount { id: "claude:desktop".into() })).await.unwrap();
        until(&store, |snapshot| snapshot.models.iter().any(|model| model.model == "claude:default")).await;
        send(&store, "back to native account", "account-fourth").await;
        completed(&store, &thread, 4, "completed").await;
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires BEX_LIVE_CLAUDE_PROGRAM; starts and cancels an isolated login, never performs inference"]
async fn live_claude_account_login_url_and_cancellation() {
    let program = std::env::var_os("BEX_LIVE_CLAUDE_PROGRAM").expect("set BEX_LIVE_CLAUDE_PROGRAM");
    let root = tempfile::tempdir().unwrap();
    let fixture = HostFixture::start(
        root.path(),
        AppServerConfig {
            program: root.path().join("missing-codex"),
            ..Default::default()
        },
        Arc::new(Memory::default()),
        "Isolated Claude login",
        false,
        Some(Path::new(&program)),
    )
    .await
    .unwrap();
    let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled)
        .await
        .unwrap();
    let store = Store::connect(&endpoint, &fixture.ticket, Snapshot::default(), None)
        .await
        .unwrap();
    store
        .dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
            provider: agent_core::session::ProviderKind::Claude,
        }))
        .await
        .unwrap();
    let login = store.snapshot().account.login.clone().unwrap();
    assert!(login.requires_code_submission);
    assert!(login.verification_url.starts_with("https://claude.com/"));
    store
        .dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
            id: login.login_id.clone(),
        }))
        .await
        .unwrap();
    assert!(
        !root
            .path()
            .join("claude/accounts")
            .join(login.login_id.strip_prefix("claude:").unwrap())
            .exists()
    );
    drop(store);
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_accepts_running_input_and_reads_past_the_previous_result() {
    let root = tempfile::tempdir().unwrap();
    let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
    let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    let key = store.snapshot().navigation.draft_key.clone();
    store
        .dispatch(Intent::SelectModel {
            thread_id: key,
            model: "claude:default".into(),
        })
        .await
        .unwrap();
    let id = send(&store, "wait", "initial").await;
    until(&store, |snapshot| {
        snapshot
            .conversations
            .get(&id)
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.first())
            .and_then(|turn| turn.items.as_ref())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.text.as_deref() == Some("Waiting for interruption"))
            })
    })
    .await;
    assert!(
        store.snapshot().conversations[&id]
            .capabilities
            .unwrap()
            .additional_input
    );
    assert!(
        agent_core::session::input_unavailable_reason(&store.snapshot().conversations[&id])
            .is_none()
    );
    let local = fixture.local().await.unwrap();
    assert!(
        local
            .peer
            .call(&rpc::SteerTurn {
                thread_id: id.clone(),
                client_user_message_id: "stale".into(),
                input: vec![rpc::Input::Text {
                    text: "must not arrive".into()
                }],
                expected_turn_id: "stale".into(),
            })
            .await
            .is_err()
    );
    send(&store, "follow-up", "steered").await;
    let snapshot = completed(&store, &id, 1, "completed").await;
    let items = snapshot.conversations[&id].turns.as_ref().unwrap()[0]
        .items
        .as_ref()
        .unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| item.client_id.as_deref() == Some("steered"))
            .count(),
        1
    );
    assert!(
        items
            .iter()
            .any(|item| item.text.as_deref() == Some("reply 2: follow-up"))
    );
    assert!(snapshot.pending_submissions.is_empty());
    send(&store, "wait", "wait-again").await;
    until(&store, |snapshot| {
        snapshot
            .conversations
            .get(&id)
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.get(1))
            .and_then(|turn| turn.items.as_ref())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.text.as_deref() == Some("Waiting for interruption"))
            })
    })
    .await;
    let queued = local
        .peer
        .call(&rpc::QueueTurn {
            thread_id: id.clone(),
            client_user_message_id: "queued".into(),
            input: vec![rpc::Input::Text {
                text: "queued follow-up".into(),
            }],
        })
        .await
        .unwrap();
    let snapshot = completed(&store, &id, 2, "completed").await;
    let items = snapshot.conversations[&id].turns.as_ref().unwrap()[1]
        .items
        .as_ref()
        .unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.id == queued.queued_submission.id
                && item.client_id.as_deref() == Some("queued"))
    );
    assert!(
        items
            .iter()
            .any(|item| item.text.as_deref() == Some("reply 4: queued follow-up"))
    );
    send(&store, "after completion", "last").await;
    completed(&store, &id, 3, "completed").await;
    let native_id = id.strip_prefix("claude:").unwrap();
    let inputs: Vec<Value> = serde_json::from_slice(
        &std::fs::read(
            Path::new(&snapshot.navigation.cwd).join(format!("claude-session-{native_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(inputs.len(), 5);
    assert!(inputs.iter().all(|input| input["pid"] == inputs[0]["pid"]));
    local.close().await;
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}
