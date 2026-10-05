use agent_protocol::{models, operations as rpc};
use std::{path::Path, sync::Arc, time::Duration};

use agent_protocol::requests::Answer;

use agent_core::state::Attachment;

use agent_core::state::Intent;

use agent_core::state::Snapshot;

use agent_core::state::operations as op;

use agent_core::store::Outcome;

use agent_core::store::Store;

use agent_transport::transport::Endpoint;

use agent_transport::transport::Relays;
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

async fn send(store: &Store, text: &str, client_id: &str) -> agent_protocol::session::SessionRef {
    draft(store, text).await;
    let outcome = store
        .dispatch(Intent::Submit {
            alternate: false,
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
                .requests()
                .map(|request| (&request.id, &request.body))
                .collect::<Vec<_>>()
        )
    })
}

async fn completed(
    store: &Store,
    id: &agent_protocol::session::SessionRef,
    count: usize,
    status: &str,
) -> Arc<Snapshot> {
    until(store, |snapshot| {
        snapshot
            .conversations
            .get(id)
            .and_then(|thread| thread.turns.as_ref())
            .is_some_and(|turns| {
                turns.len() == count && turns.last().unwrap().status.label() == status
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
async fn model_refresh_observes_catalog_changes_without_restarting_host() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("claude-native")).unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        let models = |snapshot: &Snapshot| {
            snapshot
                .models
                .iter()
                .filter(|model| model.model.instance_id.as_str() == "claude")
                .cloned()
                .collect::<Vec<_>>()
        };
        let original = models(&store.snapshot());
        assert_eq!(original.len(), 10);
        assert_eq!(
            original
                .iter()
                .find(|model| model.is_default == Some(true))
                .unwrap()
                .model
                .id,
            "claude-fable-5-1"
        );
        let version = root.path().join("claude-native/fixture-version.txt");
        std::fs::write(&version, "2.1.284").unwrap();
        store
            .dispatch(Intent::LoadModels(op::LoadModels {}))
            .await
            .unwrap();
        let newer = models(&store.snapshot());
        assert_eq!(newer.len(), 12);
        assert_eq!(newer[0].model.id, "claude-opus-5-5");
        assert_eq!(
            newer[0]
                .capabilities
                .select(&["effort"])
                .unwrap()
                .selected(None),
            Some("medium")
        );
        std::fs::write(&version, "2.1.110").unwrap();
        store
            .dispatch(Intent::LoadModels(op::LoadModels {}))
            .await
            .unwrap();
        let older = models(&store.snapshot());
        assert_eq!(older.len(), 5, "unsupported models must disappear");
        assert!(
            !older
                .iter()
                .any(|model| model.model.id == "claude-opus-4-7")
        );
        assert!(store.snapshot().model_errors.is_empty());
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("model refresh deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_execution_compiles_declared_options_without_catalog_reads() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("claude-native")).unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        // Executing a turn must use the owned catalog without probing the CLI again.
        std::fs::write(root.path().join("claude-native/fixture-version-error"), "").unwrap();
        let local = fixture.local().await.unwrap();
        for (model, effort, api_model, api_effort) in [
            ("unlisted-model", "low", "unlisted-model", None),
            ("opus", "ultracode", "claude-opus-5[1m]", Some("xhigh")),
        ] {
            std::fs::write(
                root.path().join("claude-fixture.json"),
                json!({"expectedModel":api_model}).to_string(),
            )
            .unwrap();
            let response = local
                .peer
                .call(&op::CreateSession {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    cwd: Some(root.path().to_string_lossy().into()),
                    model: Some(agent_protocol::models::ModelRef {
                        instance_id: "claude"
                            .parse::<agent_protocol::session::ProviderInstanceId>()
                            .unwrap(),
                        id: model.into(),
                    }),
                })
                .await
                .unwrap();
            let id = response.response.thread.id.unwrap();
            store
                .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                .await
                .unwrap();
            local
                .peer
                .call(&rpc::Submission {
                    thread_id: id.clone(),
                    client_user_message_id: format!("delegate-{model}").into(),
                    input: vec![rpc::Input::Text {
                        text: "delegate settings".into(),
                    }],
                    model: Some(agent_protocol::models::ModelRef {
                        instance_id: "claude"
                            .parse::<agent_protocol::session::ProviderInstanceId>()
                            .unwrap(),
                        id: model.into(),
                    }),
                    options: vec![agent_protocol::models::ModelOptionSelection {
                        id: "effort".into(),
                        value: agent_protocol::models::ModelOptionValue::String(effort.into()),
                    }],
                })
                .await
                .unwrap();
            completed(&store, &id, 1, "completed").await;
            let session = host_fixture::test_support::native_id(root.path(), &id);
            let inputs: Value = serde_json::from_slice(
                &std::fs::read(root.path().join(format!("claude-session-{session}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(inputs[0]["model"], api_model);
            assert_eq!(inputs[0]["effort"], json!(api_effort));
            if effort == "ultracode" {
                assert_eq!(inputs[0]["settings"], json!({"ultracode":true}));
            }
        }
        local.close().await;
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("CLI settings delegation deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compiled_native_query_controls_reuse_and_prompt_effort_preserves_commands() {
    tokio::time::timeout(Duration::from_secs(45), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        let local = fixture.local().await.unwrap();
        let settings = local.peer.call(&agent_protocol::providers::ReadProviderSettings {}).await.unwrap();
        let config = agent_protocol::providers::ProviderConfig {
            driver: "claudeAgent".parse().unwrap(), display_name: None, accent_color: None,
            enabled: true, environment: Vec::new(),
            config: json!({"binaryPath":fixture_program(), "homePath":root.path().join("claude-native"), "launchArgs":"--"}),
        };
        local.peer.call(&agent_protocol::providers::UpdateProviderInstance {
            operation_id: uuid::Uuid::new_v4(), revision: settings.revision,
            mutation: agent_protocol::providers::ProviderMutation::Upsert { instance_id: "claude".parse().unwrap(), config },
        }).await.unwrap();
        let instance = "claude"
            .parse::<agent_protocol::session::ProviderInstanceId>()
            .unwrap();
        let response = local
            .peer
            .call(&op::CreateSession {
                instance_id: instance.clone(),
                cwd: Some(root.path().to_string_lossy().into()),
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: instance.clone(),
                    id: "opus".into(),
                }),
            })
            .await
            .unwrap();
        let id = response.response.thread.id.unwrap();
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        for (
            index,
            (model, context, effort, fast, thinking, text, api_model, api_effort, settings),
        ) in [
            (
                "opus",
                Some("1m"),
                Some("high"),
                Some(false),
                None,
                "first",
                "claude-opus-5[1m]",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "claude-opus-5",
                Some("1m"),
                Some("high"),
                Some(false),
                None,
                "same query",
                "claude-opus-5[1m]",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "opus",
                Some("1m"),
                Some("high"),
                Some(true),
                None,
                "fast on",
                "claude-opus-5[1m]",
                Some("high"),
                json!({"fastMode":true}),
            ),
            (
                "opus",
                Some("1m"),
                Some("high"),
                Some(false),
                None,
                "fast off",
                "claude-opus-5[1m]",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "opus",
                Some("200k"),
                Some("high"),
                Some(false),
                None,
                "smaller window",
                "claude-opus-5",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "opus",
                Some("200k"),
                Some("ultrathink"),
                Some(false),
                None,
                "investigate",
                "claude-opus-5",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "opus",
                Some("200k"),
                Some("ultrathink"),
                Some(false),
                None,
                "/plugin:skill now",
                "claude-opus-5",
                Some("high"),
                json!({"fastMode":false}),
            ),
            (
                "haiku",
                None,
                None,
                None,
                Some(false),
                "thinking off",
                "claude-haiku-4-5",
                None,
                json!({"alwaysThinkingEnabled":false}),
            ),
            (
                "haiku",
                None,
                None,
                Some(true),
                Some(false),
                "unsupported fast",
                "claude-haiku-4-5",
                None,
                json!({"alwaysThinkingEnabled":false}),
            ),
            (
                "haiku",
                None,
                None,
                None,
                Some(true),
                "thinking on",
                "claude-haiku-4-5",
                None,
                json!({"alwaysThinkingEnabled":true}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            std::fs::write(
                root.path().join("claude-fixture.json"),
                json!({"expectedModel":api_model}).to_string(),
            )
            .unwrap();
            let mut options = Vec::new();
            for (id, value) in [("contextWindow", context), ("effort", effort)] {
                if let Some(value) = value {
                    options.push(agent_protocol::models::ModelOptionSelection {
                        id: id.into(),
                        value: agent_protocol::models::ModelOptionValue::String(value.into()),
                    });
                }
            }
            for (id, value) in [("fastMode", fast), ("thinking", thinking)] {
                if let Some(value) = value {
                    options.push(agent_protocol::models::ModelOptionSelection {
                        id: id.into(),
                        value: agent_protocol::models::ModelOptionValue::Boolean(value),
                    });
                }
            }
            local
                .peer
                .call(&rpc::Submission {
                    thread_id: id.clone(),
                    client_user_message_id: format!("compiled-{index}").into(),
                    input: vec![rpc::Input::Text { text: text.into() }],
                    model: Some(agent_protocol::models::ModelRef {
                        instance_id: instance.clone(),
                        id: model.into(),
                    }),
                    options,
                })
                .await
                .unwrap();
            completed(&store, &id, index + 1, "completed").await;
            if index == 5 {
                store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                let snapshot = store.snapshot();
                let turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[index];
                let user = turn.items.as_ref().unwrap().iter().find(|item| item.client_input_id.as_deref() == Some("compiled-5")).unwrap();
                assert!(matches!(user.body(), agent_protocol::items::ItemBody::UserMessage { content, .. } if content.first() == Some(&agent_protocol::items::MessagePart::Text { text: text.into() })), "Host-owned user text must survive the native prompt prefix and reopen");
            }
            let native = host_fixture::test_support::native_id(root.path(), &id);
            let inputs: Value = serde_json::from_slice(
                &std::fs::read(root.path().join(format!("claude-session-{native}.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(inputs[index]["model"], api_model);
            assert_eq!(inputs[index]["effort"], json!(api_effort));
            assert_eq!(inputs[index]["settings"], settings);
            assert_eq!(
                inputs[index]["content"][0]["text"],
                if index == 5 {
                    "Ultrathink:\ninvestigate"
                } else {
                    text
                }
            );
            if index > 0 {
                assert_eq!(
                    inputs[index]["pid"] == inputs[index - 1]["pid"],
                    matches!(index, 1 | 5 | 6 | 8),
                    "reuse at case {index}"
                );
            }
        }
        local.close().await;
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("compiled native query deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_submission_preserves_inputs_settings_workspaces_and_history_across_host_restart() {
    tokio::time::timeout(Duration::from_secs(120), async {
        for automatic in [false, true] {
            for selected in [false, true] {
                for attachment in ["none", "image", "file"] {
                    let root = tempfile::tempdir().unwrap();
                    let root = dunce::canonicalize(root.path()).unwrap();
                    let workspace = root.join("project");
                    std::fs::create_dir(&workspace).unwrap();
                    git(&workspace, &["init", "--quiet"]);
                    git(&workspace, &["config", "core.autocrlf", "false"]);
                    std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
                    git(&workspace, &["add", "tracked.txt"]);
                    git(&workspace, &["-c","user.name=Fixture","-c","user.email=fixture@example.invalid","-c","commit.gpgsign=false","commit","--quiet","-m","fixture"]);
                    std::fs::write(root.join("bex-projects.json"), json!([{"id":"project","name":"Project","roots":[{"path":workspace}]}]).to_string()).unwrap();
                    std::fs::write(root.join("bex-worktrees.json"), json!({"settings":{"createOnNewSession":automatic,"worktreeDirectory":root.join("worktrees")}}).to_string()).unwrap();
                    let memory = Arc::new(Memory::default());
                    let mut fixture = host(&root, memory.clone(), fixture_program()).await;
                    let (mut store, mut endpoint) = connect(&fixture, Snapshot::default()).await;
                    store.dispatch(Intent::NewChat { cwd: if selected { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                    let key = store.snapshot().navigation.draft_key.clone();
                    store.dispatch(Intent::SelectModel { thread_id: key.clone(), model: agent_protocol::models::ModelRef { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude-fable-5-1".into() } }).await.unwrap();
                    store.dispatch(Intent::SelectModelOption { thread_id: key, id: "effort".into(), value: Some(agent_protocol::models::ModelOptionValue::String("low".into()))}).await.unwrap();
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
                        assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.drafts[&agent_core::state::DraftKey::from(&id)].attachments.is_empty());
                        assert_eq!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].model.as_ref().map(|model| model.id.as_str()), Some("claude-fable-5-1"));
                        assert_eq!(agent_protocol::models::model_option_string(&snapshot.drafts[&agent_core::state::DraftKey::from(&id)].options, "effort"), Some("low"));
                        let cwd = snapshot.navigation.cwd.clone();
                        if let Some(previous) = &previous_cwd { assert_eq!(&cwd, previous); }
                        if selected && automatic {
                            assert_eq!(Path::new(&cwd).file_name(), workspace.file_name());
                            assert_eq!(Path::new(&cwd).parent().unwrap().parent().unwrap(), root.join("worktrees"));
                        }
                        else { assert_eq!(Path::new(&cwd), if selected { workspace.clone() } else { root.join("bex-chats") }); }
                        let turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[number];
                        let items = turn.items.as_ref().unwrap();
                        assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::Reasoning { .. }) && item_text(item) == Some("Fixture reasoning")));
                        let responses: Vec<_> = items.iter().filter(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. })).collect();
                        assert_eq!(responses.len(), 1, "streaming and completed blocks must not duplicate");
                        assert!(item_text(responses[0]).unwrap().starts_with(&format!("reply {}: message {number}", number + 1)));
                        let user = items.iter().find(|item| matches!(item.body(), agent_protocol::items::ItemBody::UserMessage { .. })).unwrap();
                        assert_eq!(user.client_input_id.as_deref(), Some(format!("client-{number}").as_str()));
                        assert!(matches!(user.body(), agent_protocol::items::ItemBody::UserMessage { content, .. } if content.first() == Some(&agent_protocol::items::MessagePart::Text { text: format!("message {number}") })));
                        let session = host_fixture::test_support::native_id(&root, &id);
                        let inputs: Value = serde_json::from_slice(&std::fs::read(Path::new(&cwd).join(format!("claude-session-{session}.json"))).unwrap()).unwrap();
                        assert_eq!(inputs.as_array().unwrap().len(), number + 1);
                        assert_eq!(inputs[number]["effort"], "low");
                        assert_eq!(inputs[number]["content"].as_array().unwrap().len(), if attachment == "none" { 1 } else { 2 });
                        if attachment == "image" { assert_eq!(inputs[number]["content"][1]["source"]["media_type"], "image/png"); }
                        if attachment == "file" { assert!(inputs[number]["content"][1]["text"].as_str().unwrap().contains("note.txt")); }
                        store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
                        assert!(store.snapshot().threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_ref() == Some(&id)));
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
async fn unsupported_controls_and_session_elicitation_keep_claude_running() {
    tokio::time::timeout(Duration::from_secs(60), async {
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
                model: agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        for (index, text) in ["unknown_control", "dialog", "elicitation"]
            .into_iter()
            .enumerate()
        {
            let id = send(&store, text, text).await;
            if text == "elicitation" {
                let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
                let request = snapshot.requests().next().unwrap();
                assert_eq!(
                    request.target,
                    agent_protocol::requests::RequestTarget::Session
                );
                store
                    .dispatch(Intent::Respond(op::Respond {
                        request_id: request.id.clone(),
                        answer: Answer::Elicitation {
                            action: agent_protocol::requests::ElicitationAnswer::Accept {
                                values: json!({"name":"BEX"}),
                            },
                        },
                    }))
                    .await
                    .unwrap();
            }
            let snapshot = completed(&store, &id, index + 1, "completed").await;
            assert!(snapshot.requests().next().is_none());
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        }
        store.disconnect().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .unwrap();
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
                model: agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "permission-1").await;
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        let cwd = store.snapshot().navigation.cwd.clone();
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None)
            .await
            .unwrap();
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        let request = snapshot.requests().next().unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Approval {
                    choice_id: request.body.choices()[1].id.clone(),
                },
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        assert!(snapshot.requests().next().is_none());
        send(&store, "permission", "permission-2").await;
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        let request = snapshot.requests().next().unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Approval {
                    choice_id: request.body.choices()[0].id.clone(),
                },
            }))
            .await
            .unwrap();
        completed(&store, &id, 2, "completed").await;
        assert_eq!(
            std::fs::read_to_string(Path::new(&cwd).join("approved.txt")).unwrap(),
            "approved"
        );
        send(&store, "question", "question").await;
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        let request = snapshot.requests().next().unwrap();
        let agent_protocol::requests::RequestBody::Question { questions } = &request.body else {
            panic!("question request")
        };
        let question = &questions[0];
        let blue = question
            .choices
            .iter()
            .find(|choice| choice.label == "Blue")
            .unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Questions {
                    answers: [(
                        question.id.clone(),
                        agent_protocol::requests::QuestionAnswer::SingleChoice {
                            choice_id: blue.id.clone(),
                        },
                    )]
                    .into(),
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
                .any(|item| item_text(item) == Some("Blue"))
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
                .any(|item| item_text(item) == Some("Waiting for interruption"))
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
        assert!(
            store.snapshot().drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
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
                .label(),
            "running"
        );
        assert!(
            !store.snapshot().conversations[&id].queue_held,
            "a rejected stale stop must not hold waiting inputs"
        );
        store
            .dispatch(Intent::Interrupt(op::Interrupt {
                thread_id: id.clone(),
                turn_id,
            }))
            .await
            .unwrap();
        completed(&store, &id, 4, "interrupted").await;
        let snapshot = until(&store, |snapshot| snapshot.conversations[&id].queue_held).await;
        assert_eq!(snapshot.conversations[&id].queued_inputs.len(), 1);
        for action in [
            agent_protocol::queue::QueueAction::Cancel { id: "busy".into() },
            agent_protocol::queue::QueueAction::Resume,
        ] {
            store
                .dispatch(Intent::QueueControl(agent_protocol::queue::QueueControl {
                    session: id.clone(),
                    action,
                }))
                .await
                .unwrap();
        }
        send(&store, "after interruption", "recovered").await;
        let snapshot = completed(&store, &id, 5, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.requests().next().is_none() && snapshot.pending_submissions.is_empty());
        assert!(
            snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
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
            ("claude-fable-5-1", "Claude Fable 5.1"),
            ("claude-opus-5", "Claude Opus 5"),
            ("claude-sonnet-5", "Claude Sonnet 5"),
            ("claude-haiku-4-5", "Claude Haiku 4.5"),
        ] {
            let snapshot = store.snapshot();
            let entry = snapshot
                .models
                .iter()
                .find(|entry| {
                    entry.model.instance_id.as_str() == "claude" && entry.model.id == model
                })
                .unwrap();
            assert_eq!(entry.display_name, display);
        }
        let codex_model = store
            .snapshot()
            .models
            .iter()
            .find(|model| {
                model.model.instance_id
                    == "codex"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap()
            })
            .unwrap()
            .model
            .clone();
        let claude_model = agent_protocol::models::ModelRef {
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
            id: "claude-fable-5-1".into(),
        };
        for (original, other) in [
            (codex_model.clone(), claude_model.clone()),
            (claude_model, codex_model),
        ] {
            store
                .dispatch(Intent::NewChat { cwd: String::new() })
                .await
                .unwrap();
            let key = store.snapshot().navigation.draft_key.clone();
            store
                .dispatch(Intent::SelectModel {
                    thread_id: key,
                    model: original.clone(),
                })
                .await
                .unwrap();
            let id = send(
                &store,
                "original provider",
                &format!("{}-start", original.id),
            )
            .await;
            completed(&store, &id, 1, "completed").await;

            store
                .dispatch(Intent::SelectModel {
                    thread_id: id.clone().into(),
                    model: other.clone(),
                })
                .await
                .unwrap();
            draft(&store, "keep this input").await;
            assert!(
                store
                    .dispatch(Intent::Submit {
                        alternate: false,
                        thread_id: Some(id.clone()),
                        client_user_message_id: format!("{}-mismatch", original.id).into(),
                    })
                    .await
                    .is_err()
            );
            let snapshot = store.snapshot();
            assert_eq!(
                snapshot
                    .navigation
                    .thread_id
                    .as_ref()
                    .map(|session| session.id.as_str()),
                Some(id.id.as_str())
            );
            assert_eq!(
                snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text,
                "keep this input"
            );
            assert!(snapshot.error.as_ref().unwrap().contains("新しい会話"));
            assert!(snapshot.pending_submissions.is_empty());
            assert_eq!(snapshot.conversations[&id].turns.as_ref().unwrap().len(), 1);

            store
                .dispatch(Intent::SelectModel {
                    thread_id: id.clone().into(),
                    model: original.clone(),
                })
                .await
                .unwrap();
            send(
                &store,
                "continue with original provider",
                &format!("{}-retry", original.id),
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
async fn unconfigured_claude_keeps_codex_usable_without_model_errors() {
    tokio::time::timeout(Duration::from_secs(30), async {
        for installed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let program = if installed {
                std::fs::create_dir_all(root.path().join("claude/accounts")).unwrap();
                std::fs::write(
                    root.path().join("claude/accounts/accounts.json"),
                    json!({"accounts":[],"selectedId":null}).to_string(),
                )
                .unwrap();
                fixture_program().to_path_buf()
            } else {
                root.path().join("missing-claude")
            };
            let fixture = host(root.path(), Arc::new(Memory::default()), &program).await;
            let saved = agent_core::state::Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-sonnet-5".into(),
                }),
                options: vec![
                    agent_protocol::models::ModelOptionSelection {
                        id: "effort".into(),
                        value: agent_protocol::models::ModelOptionValue::String("high".into()),
                    },
                    agent_protocol::models::ModelOptionSelection {
                        id: "contextWindow".into(),
                        value: agent_protocol::models::ModelOptionValue::String("200k".into()),
                    },
                ],
                text: "Keep the Claude draft".into(),
                ..Default::default()
            };
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.drafts)
                .insert("saved-claude".into(), Arc::new(saved.clone()));
            let (store, endpoint) = connect(&fixture, snapshot).await;
            assert_eq!(
                *store.snapshot().drafts[&agent_core::state::DraftKey::from("saved-claude")],
                saved
            );
            assert!(!store.snapshot().models.is_empty());
            assert!(
                store.snapshot().model_errors.is_empty(),
                "installed={installed}: {:?}",
                store.snapshot().model_errors
            );
            let models = store.snapshot();
            assert!(
                models
                    .models
                    .iter()
                    .any(|model| model.model.instance_id.as_str() == "codex")
            );
            let claude_models = models
                .models
                .iter()
                .filter(|model| model.model.instance_id.as_str() == "claude")
                .collect::<Vec<_>>();
            assert_eq!(claude_models.len(), if installed { 10 } else { 0 });
            if installed {
                assert!(
                    claude_models
                        .iter()
                        .any(|model| model.is_default == Some(true)
                            && model.model.id == "claude-fable-5-1")
                );
            }

            store
                .dispatch(Intent::NewChat { cwd: String::new() })
                .await
                .unwrap();
            let id = send(&store, "Codex remains available", "codex-only").await;
            let snapshot = completed(&store, &id, 1, "completed").await;
            assert!(snapshot.error.is_none());
            assert!(
                snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
                    && snapshot.pending_submissions.is_empty()
            );
            store.close().await.unwrap();
            endpoint.close().await;
            fixture.close().await.unwrap();
        }
    })
    .await
    .expect("unconfigured Claude deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_codex_keeps_claude_inputs_workspaces_and_resumed_history_usable() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for automatic in [false, true] {
            for selected in [false, true] {
                let root = tempfile::tempdir().unwrap();
                let root = dunce::canonicalize(root.path()).unwrap();
                let workspace = root.join("project");
                std::fs::create_dir(&workspace).unwrap();
                git(&workspace, &["init", "--quiet"]);
                git(&workspace, &["config", "core.autocrlf", "false"]);
                std::fs::write(workspace.join("tracked.txt"), "fixture\n").unwrap();
                git(&workspace, &["add", "tracked.txt"]);
                git(&workspace, &["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture"]);
                std::fs::write(root.join("bex-projects.json"), json!([{"id":"project","name":"Project","roots":[{"path":workspace}]}]).to_string()).unwrap();
                std::fs::write(root.join("bex-worktrees.json"), json!({"settings":{"createOnNewSession":automatic,"worktreeDirectory":root.join("worktrees")}}).to_string()).unwrap();
                let config = AppServerConfig { program: root.join("missing-codex"), ..Default::default() };
                let memory = Arc::new(Memory::default());
                let mut saved = Snapshot::default();
                let mut thread_id: Option<agent_protocol::session::SessionRef> = None;
                let mut cwd = None;
                for index in 0..2 {
                    let fixture = HostFixture::start(&root, config.clone(), memory.clone(), "Independent Host", false, Some(fixture_program())).await.unwrap();
                    let (store, endpoint) = connect(&fixture, saved).await;
                    assert!(store.snapshot().connected);
                    assert!(store.snapshot().model_errors.contains_key("codex"));
                    assert!(store.snapshot().models.iter().all(|model| model.model.instance_id == "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()));
                    if let Some(id) = &thread_id {
                        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
                    } else {
                        store.dispatch(Intent::NewChat { cwd: if selected { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                        let key = store.snapshot().navigation.draft_key.clone();
                        store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude-fable-5-1".into() } }).await.unwrap();
                    }
                    let id = send(&store, &format!("independent {index}"), &format!("independent-{index}")).await;
                    let snapshot = until(&store, |snapshot| snapshot.conversations.get(&id)
                        .and_then(|thread| thread.turns.as_ref())
                        .is_some_and(|turns| turns.len()==index+1 && turns.last().unwrap().status!=agent_protocol::execution::TurnStatus::Running)).await;
                    assert_eq!(snapshot.conversations[&id].turns.as_ref().unwrap().last().unwrap().status,agent_protocol::execution::TurnStatus::Completed,
                        "automatic={automatic}, selected={selected}, index={index}, fixture failures={}",std::fs::read_to_string(root.join("claude-native/fixture-failures.jsonl")).unwrap_or_default());
                    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
                    assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.pending_submissions.is_empty());
                    let current = snapshot.conversations[&id].cwd.clone().unwrap();
                    if let Some(previous) = &cwd { assert_eq!(previous, &current); }
                    if selected && automatic {
                        assert_eq!(Path::new(&current).file_name(), workspace.file_name());
                        assert_eq!(Path::new(&current).parent().unwrap().parent().unwrap(), root.join("worktrees"));
                    }
                    else { assert_eq!(Path::new(&current), if selected { workspace.clone() } else { root.join("bex-chats") }); }
                    let turns = snapshot.conversations[&id].turns.as_ref().unwrap();
                    assert!(turns[index].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.starts_with(&format!("reply {}: independent {index}", index + 1)))));
                    store.dispatch(Intent::ListSessions(op::ListSessions::new(Default::default()))).await.unwrap();
                    let snapshot = store.snapshot();
                    let list = snapshot.threads.as_ref().unwrap();
                    assert!(list.data.iter().any(|thread| thread.id.as_ref() == Some(&id)));
                    assert!(list.provider_errors.as_ref().unwrap()["codex"]["message"].is_string());
                    let management = fixture.local().await.unwrap();
                    let status = management.peer.call(&rpc::ReadHostStatus {}).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                    assert!(status["providerErrors"]["codex"]["message"].is_string());
                    if selected {
                        let path = Path::new(&current).join("tracked.txt");
                        let listed = management.peer.call(&serde_json::from_value::<op::ListFiles>(json!({"path":current})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        assert!(listed["entries"].as_array().unwrap().iter().any(|entry| entry["name"] == "tracked.txt"));
                        let read = management.peer.request::<models::FileContent>(&agent_protocol::protocol::Call::ReadFile(serde_json::from_value::<op::ListFiles>(json!({"path":path})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        let contents = format!("workspace edit {index}\n");
                        let saved_file = management.peer.call(&serde_json::from_value::<rpc::WriteFile>(json!({"path":path,"revision":read["revision"],"text":contents})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap_or_else(|error| panic!("automatic={automatic}, selected={selected}, index={index}, readonly={}: {error:?}", std::fs::metadata(&path).unwrap().permissions().readonly()));
                        assert_eq!(saved_file["text"], contents);
                        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
                        let review = management.peer.call(&serde_json::from_value::<rpc::ReviewWorkspace>(json!({"cwd":current})).unwrap()).await.map(|output| serde_json::to_value(output).unwrap()).unwrap();
                        assert!(review["files"].as_array().unwrap().iter().any(|file| file["path"] == "tracked.txt"));
                    }
                    let drafts = store.snapshot().drafts.clone();
                    let dictation = management.peer.request::<rpc::Transcription>(&agent_protocol::protocol::Call::Transcribe(serde_json::from_value::<rpc::Transcribe>(json!({"preparation":null,"audio":"AAA="})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap());
                    assert!(dictation.is_err());
                    assert_eq!(*store.snapshot().drafts, *drafts);
                    assert!(management.peer.call(&op::ReadWorktreeSettings {}).await.is_ok());
                    let count_worktrees = || std::fs::read_dir(root.join("worktrees")).map(|entries| entries.count()).unwrap_or_default();
                    let before = count_worktrees();
                    assert!(management.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"instanceId":"codex","model":{"instanceId":"codex","id":"fixture-model"},"cwd":current})).unwrap()).await.is_err());
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
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "claude-approval").await;
        until(&store, |snapshot| snapshot.requests().count() == 2).await;
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
                .request::<models::Empty>(&agent_protocol::protocol::Call::ImportHistory(
                    models::Empty {}
                ))
                .await
                .is_err()
        );
        // An unavailable provider does not hide committed Host conversations.
        let list = local
            .peer
            .call(&rpc::ListSessions::new(Default::default()))
            .await
            .unwrap();
        {
            assert!(list.provider_errors.unwrap().contains_key("codex"));
            assert!(
                list.data
                    .iter()
                    .any(|thread| thread.id.as_ref() == Some(&id))
            );
        }
        until(&store, |snapshot| {
            snapshot.requests().count() == 1
                && !snapshot.activity.active[&codex_id]
                && snapshot.conversations[&codex_id].turns.as_ref().unwrap()[0]
                    .status
                    .label()
                    == "failed"
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
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        assert_eq!(snapshot.requests().count(), 1);
        let request = snapshot.requests().next().unwrap();
        assert!(
            snapshot.conversations[&id]
                .requests
                .contains_key(&request.id)
        );
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Approval {
                    choice_id: request.body.choices()[0].id.clone(),
                },
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.requests().next().is_none() && snapshot.pending_submissions.is_empty());
        assert!(
            snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
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
        assert!(
            snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
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
                model: agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        draft(&store, "keep my draft").await;
        assert!(
            store
                .dispatch(Intent::Submit {
                    alternate: false,
                    thread_id: None,
                    client_user_message_id: "auth-failure".into()
                })
                .await
                .is_err()
        );
        let snapshot = store.snapshot();
        let id = snapshot.navigation.thread_id.clone().unwrap();
        assert!(snapshot.error.as_ref().unwrap().contains("サブスク認証"));
        assert_eq!(
            snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text,
            "keep my draft"
        );
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
                    alternate: false,
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
        assert_eq!(
            store.snapshot().drafts[&agent_core::state::DraftKey::from(&id)].text,
            "keep my draft"
        );
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
                .unwrap()
                .message,
            "fixture inference failed"
        );
        std::fs::write(&config, "{}").unwrap();
        send(&store, "retry", "retry").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(
            snapshot.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
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
        store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude-haiku-4-5".into() } }).await.unwrap();
        let id = send(&store, "Remember marker BEX_CLAUDE_STORE_OK. Reply with exactly that marker. Do not use tools.", "live-1").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("BEX_CLAUDE_STORE_OK"))));
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
        let fixture = host(root.path(), memory, Path::new(&program)).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
        send(&store, "Reply with exactly the marker from my previous message. Do not use tools.", "live-2").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[1].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("BEX_CLAUDE_STORE_OK"))));
        send(&store, "Count from 1 to 10000, one number per line. Do not use tools.", "live-stop").await;
        let snapshot = until(&store, |snapshot| snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 3 && turns[2].items.as_ref().is_some_and(|items| items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| !text.is_empty()))))).await;
        let turn_id = snapshot.conversations[&id].turns.as_ref().unwrap()[2].id.clone();
        store.dispatch(Intent::Interrupt(op::Interrupt { thread_id: id.clone(), turn_id })).await.unwrap();
        let snapshot = completed(&store, &id, 3, "interrupted").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        send(&store, "Reply with exactly BEX_CLAUDE_RECOVERED. Do not use tools.", "live-recovery").await;
        let snapshot = completed(&store, &id, 4, "completed").await;
        assert!(snapshot.error.is_none());
        assert!(snapshot.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.pending_submissions.is_empty());
        assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[3].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("BEX_CLAUDE_RECOVERED"))));
        send(&store, "Count from 1 to 100, one number per line. Do not use tools.", "live-before-additional").await;
        until(&store, |snapshot| snapshot.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 5 && turns[4].items.as_ref().is_some_and(|items| items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| !text.is_empty()))))).await;
        assert!(store.snapshot().conversations[&id].capabilities.unwrap().active_steering);
        send(&store, "Reply with exactly BEX_CLAUDE_ADDITIONAL_OK. Do not use tools.", "live-additional").await;
        let snapshot = completed(&store, &id, 5, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(snapshot.pending_submissions.is_empty());
        let items = snapshot.conversations[&id].turns.as_ref().unwrap()[4].items.as_ref().unwrap();
        assert_eq!(items.iter().filter(|item| item.client_input_id.as_deref() == Some("live-additional")).count(), 1);
        assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("BEX_CLAUDE_ADDITIONAL_OK"))));
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
            model: agent_protocol::models::ModelRef {
                instance_id: "claude"
                    .parse::<agent_protocol::session::ProviderInstanceId>()
                    .unwrap(),
                id: "claude-fable-5-1".into(),
            },
        })
        .await
        .unwrap();
    let id = send(&store, "first", "reuse-1").await;
    completed(&store, &id, 1, "completed").await;
    send(&store, "second", "reuse-2").await;
    let snapshot = completed(&store, &id, 2, "completed").await;
    let native = host_fixture::test_support::native_id(root.path(), &id);
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

// Windows holds a live process's cwd open; checkout recovery itself is tested on all OSes.
#[cfg(unix)]
#[tokio::test]
async fn deleted_claude_worktree_restarts_the_retained_process_and_continues_the_conversation() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        git(&project, &["init", "--quiet", "--initial-branch=main"]);
        git(&project, &["config", "core.autocrlf", "false"]);
        git(
            &project,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        std::fs::write(
            root.join("bex-worktrees.json"),
            json!({"settings":{"createOnNewSession":true}}).to_string(),
        )
        .unwrap();
        let fixture = host(&root, Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat {
                cwd: project.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectModel {
                thread_id: store.snapshot().navigation.draft_key.clone(),
                model: agent_protocol::models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "first", "before-removal").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        let cwd = snapshot.navigation.cwd.clone();
        let native = host_fixture::test_support::native_id(&root, &id);
        let inputs_path = Path::new(&cwd).join(format!("claude-session-{native}.json"));
        let before: Value = serde_json::from_slice(&std::fs::read(&inputs_path).unwrap()).unwrap();
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(send(&store, "second", "after-removal").await, id);
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert_eq!(snapshot.navigation.cwd, cwd);
        let after: Value = serde_json::from_slice(&std::fs::read(&inputs_path).unwrap()).unwrap();
        assert_ne!(
            before[0]["pid"], after[1]["pid"],
            "deleted cwd must not reuse the retained process"
        );
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("Claude worktree recovery deadline");
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
                .request::<models::Empty>(&agent_protocol::protocol::Call::WriteTerminal(
                    write.clone()
                ))
                .await
                .is_err()
        );
        assert!(stranger.peer.call(&resize).await.is_err());
        assert!(
            stranger
                .peer
                .request::<models::Empty>(&agent_protocol::protocol::Call::KillTerminal(
                    kill.clone()
                ))
                .await
                .is_err()
        );
        owner.peer.call(&resize).await.unwrap();
        owner
            .peer
            .request::<models::Empty>(&agent_protocol::protocol::Call::WriteTerminal(write))
            .await
            .unwrap();
        let mut output = Vec::new();
        loop {
            let message = owner
                .events
                .read::<agent_protocol::protocol::Notification>()
                .await
                .unwrap()
                .expect("Host event stream ended");
            if let agent_protocol::protocol::Notification::Output { data, .. } = message {
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
            .request::<models::Empty>(&agent_protocol::protocol::Call::KillTerminal(kill))
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
        let root = dunce::canonicalize(directory.path()).unwrap();
        let native = root.join("claude-native");
        std::fs::create_dir_all(&native).unwrap();
        std::fs::write(native.join("fixture-auth.json"), json!({"loggedIn":true,"authMethod":"claude.ai","email":"native@example.invalid","subscriptionType":"pro"}).to_string()).unwrap();
        std::fs::create_dir_all(native.join("skills/account-test")).unwrap();
        std::fs::write(native.join("skills/account-test/SKILL.md"), "shared skill").unwrap();
        std::fs::write(native.join("settings.json"), r#"{"model":"shared-model"}"#).unwrap();
        let memory = Arc::new(Memory::default());
        let start = || HostFixture::start(&root, AppServerConfig { program: root.join("missing-codex"), ..Default::default() }, memory.clone(), "Claude accounts", false, Some(fixture_program()));
        std::fs::write(native.join("usage-paused"), "").unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        tokio::time::timeout(Duration::from_secs(2), store.dispatch(Intent::ListAccounts(op::ListAccounts {})))
            .await.expect("listing must not wait for usage").unwrap();
        while !native.join("usage-requested").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let key = store.snapshot().navigation.draft_key.clone();
        tokio::time::timeout(Duration::from_secs(2), store.dispatch(Intent::SelectAccountForDraft(op::SelectAccountForDraft { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(),
            id: "claude:desktop".into(), thread_id: key,
        }))).await.expect("selection and model loading must not wait for usage").unwrap();
        assert!(store.snapshot().account.accounts.as_ref().unwrap().accounts[0].usage.is_none());
        std::fs::remove_file(native.join("usage-paused")).unwrap();
        until(&store, |snapshot| snapshot.account.accounts.as_ref().is_some_and(|accounts| accounts.accounts[0].usage.is_some())).await;
        let usage = store.snapshot().account.accounts.as_ref().unwrap().accounts[0].usage.clone().unwrap();
        assert_eq!(usage.windows[0].remaining_percent, 28);
        assert_eq!(usage.windows[1].remaining_percent, 61);

        assert_eq!(store.snapshot().account.accounts.as_ref().unwrap().selected.get(&"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()).map(String::as_str), Some("claude:desktop"));
        store.dispatch(Intent::NewChat { cwd: root.to_string_lossy().into() }).await.unwrap();
        let key = store.snapshot().navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude-fable-5-1".into() } }).await.unwrap();
        let thread = send(&store, "first account", "account-first").await;
        completed(&store, &thread, 1, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap() })).await.unwrap();
        let login = store.snapshot().account.login.clone().unwrap();
        assert!(login.requires_code_submission);
        assert!(login.verification_url.starts_with("https://claude.com/"));
        assert!(store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude:wrong".into(), code: "fixture-code".into() })).await.is_err());
        store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: login.login_id.clone(), code: "fixture-code".into() })).await.unwrap();
        loop {
            store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: login.login_id.clone(), thread_id: None })).await.unwrap();
            if store.snapshot().account.login.is_none() { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        until(&store, |snapshot| snapshot.account.accounts.as_ref().is_some_and(|accounts| accounts.selected.get(&"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()) == Some(&login.login_id))).await;
        send(&store, "second account, same history", "account-second").await;
        completed(&store, &thread, 2, "completed").await;
        let profile = root.join("claude").join("accounts").join(login.login_id.strip_prefix("claude:").unwrap());
        assert!(!profile.join("projects").exists(), "credential helpers do not own conversation history");
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, snapshot).await;
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert_eq!(store.snapshot().account.accounts.as_ref().unwrap().selected.get(&"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()), Some(&login.login_id));
        send(&store, "resumed after restart", "account-third").await;
        completed(&store, &thread, 3, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap() })).await.unwrap();
        let canceled = store.snapshot().account.login.clone().unwrap();
        store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: canceled.login_id.clone() })).await.unwrap();
        assert!(!root.join("claude").join("accounts").join(canceled.login_id.strip_prefix("claude:").unwrap()).exists());
        assert!(native.join("projects").exists(), "cancel must preserve shared history");
        store.dispatch(Intent::LogoutAccount(op::LogoutAccount { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: login.login_id.clone() })).await.unwrap();
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(!store.snapshot().account.accounts.as_ref().unwrap().selected.contains_key(&"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()));
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let endpoint = Endpoint::bind(fixture.credentials.local_identity().await, Relays::Disabled).await.unwrap();
        let store = Store::connect(&endpoint, &fixture.ticket, snapshot, None).await.unwrap();
        store.dispatch(Intent::LoadModels(op::LoadModels {})).await.unwrap();
        assert!(store.snapshot().models.iter().all(|model| model.model.instance_id.as_str() == "claude"));
        assert!(store.snapshot().models.iter().any(|model| model.is_default == Some(true) && model.model.id == "claude-fable-5-1"));
        let local = fixture.local().await.unwrap();
        let error = local.peer.call(&rpc::Submission {
            thread_id: thread.clone(), client_user_message_id: "logged-out-reject".into(),
            input: vec![rpc::Input::Text { text: "must not execute".into() }],
            model: Some(models::ModelRef { instance_id: "claude".parse().unwrap(), id: "claude-fable-5-1".into() }), options: Vec::new(),
        }).await.unwrap_err();
        assert!(error.to_string().contains("Claude アカウントを選択"));
        local.close().await;
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(!store.snapshot().account.accounts.as_ref().unwrap().selected.contains_key(&"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap()), "restart must preserve logout without selecting the native account");
        store.dispatch(Intent::SelectAccount(op::SelectAccount { instance_id: "claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(), id: "claude:desktop".into() })).await.unwrap();
        until(&store, |snapshot| snapshot.models.iter().any(|model| model.model == agent_protocol::models::ModelRef {instance_id:"claude".parse::<agent_protocol::session::ProviderInstanceId>().unwrap(),id:"claude-fable-5-1".into()})).await;
        send(&store, "back to native account", "account-fourth").await;
        completed(&store, &thread, 4, "completed").await;
        let homes = std::fs::read_to_string(root.join("claude-auth-homes.jsonl")).unwrap();
        let homes: Vec<Value> = homes.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(homes.iter().map(|home| home["credentialsHome"].as_str().unwrap()).collect::<Vec<_>>(),
            [native.to_str().unwrap(), profile.to_str().unwrap(), profile.to_str().unwrap(), native.to_str().unwrap()]);
        assert_eq!(homes.iter().map(|home| home["account"].as_str().unwrap()).collect::<Vec<_>>(),
            ["native@example.invalid", "claude@example.invalid", "claude@example.invalid", "native@example.invalid"]);
        assert!(homes.iter().all(|home| home["configHome"] == native.to_string_lossy().as_ref()
            && home["skill"] == "shared skill" && home["settings"] == r#"{"model":"shared-model"}"#));
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
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
        }))
        .await
        .unwrap();
    let login = store.snapshot().account.login.clone().unwrap();
    assert!(login.requires_code_submission);
    assert!(login.verification_url.starts_with("https://claude.com/"));
    store
        .dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
            id: login.login_id.clone(),
        }))
        .await
        .unwrap();
    assert!(
        !root
            .path()
            .join("claude")
            .join("accounts")
            .join(login.login_id.strip_prefix("claude:").unwrap())
            .exists()
    );
    drop(store);
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_keeps_loading_through_background_results_and_follow_up_after_reconnect() {
    use agent_core::presentation::conversation::ConversationRowContent;
    use agent_protocol::execution::TurnStatus;

    for (prompt, idle_before_result) in [("background", false), ("background-settled", true)] {
        let root = tempfile::tempdir().unwrap();
        if prompt == "background" {
            std::fs::write(root.path().join("background-paused"), "").unwrap();
        }
        std::fs::write(
            root.path().join("claude-fixture.json"),
            json!({"idleBeforeResult":idle_before_result}).to_string(),
        )
        .unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store
            .dispatch(Intent::NewChat {
                cwd: root.path().to_str().unwrap().into(),
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectModel {
                thread_id: store.snapshot().navigation.draft_key.clone(),
                model: models::ModelRef {
                    instance_id: "claude"
                        .parse::<agent_protocol::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "claude-fable-5-1".into(),
                },
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectModelOption {
                thread_id: store.snapshot().navigation.draft_key.clone(),
                id: "effort".into(),
                value: Some(agent_protocol::models::ModelOptionValue::String(
                    "low".into(),
                )),
            })
            .await
            .unwrap();
        let id = send(&store, prompt, "background-input").await;
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None)
            .await
            .unwrap();
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        let snapshot = store.snapshot();
        let thread = &snapshot.conversations[&id];
        let turn = thread.turns.as_ref().unwrap().last().unwrap();
        assert_eq!(
            turn.status,
            TurnStatus::Running,
            "an intermediate result must stay live"
        );
        assert!(
            turn.items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item_text(item) == Some("あとで報告します"))
        );
        let projected = agent_core::presentation::conversation::project_conversation(
            &snapshot,
            thread.clone(),
            &None,
        );
        assert!(projected.turns.last().unwrap().rows.iter().any(|row| matches!(
            &row.content, ConversationRowContent::ActivityHeader { activity } if activity.is_in_progress
        )), "the shared native presentation must retain its loading indicator");
        let request = snapshot.requests().next().unwrap();
        store
            .dispatch(Intent::Respond(op::Respond {
                request_id: request.id.clone(),
                answer: Answer::Approval {
                    choice_id: request.body.choices()[0].id.clone(),
                },
            }))
            .await
            .unwrap();
        if prompt == "background" {
            let snapshot = until(&store, |snapshot| {
                snapshot.conversations[&id].turns.as_ref().unwrap()[0]
                    .items.as_ref().unwrap().iter().any(|item|
                        item.status == agent_protocol::execution::ItemStatus::Completed
                        && item.is_deferred()
                        && matches!(item.body(), agent_protocol::items::ItemBody::CommandExecution {output, exit_code:None, ..} if output == "background done"))
            }).await;
            let turn = &snapshot.conversations[&id].turns.as_ref().unwrap()[0];
            assert_eq!(turn.status, TurnStatus::Running);
            store
                .dispatch(Intent::ReadItem(op::ReadItem {
                    thread_id: id.clone(),
                    turn_id: turn.id.clone(),
                    item_id: "tool-1".into(),
                }))
                .await
                .unwrap();
            let snapshot = store.snapshot();
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            assert!(snapshot.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().any(|item|
                item.status == agent_protocol::execution::ItemStatus::Completed
                && !item.is_deferred()
                && matches!(item.body(), agent_protocol::items::ItemBody::CommandExecution {output, exit_code:None, ..} if output == "background stdout\n")
            ), "expanded background work must recover native output while its turn is still running");
            std::fs::remove_file(root.path().join("background-paused")).unwrap();
        }
        let snapshot = completed(&store, &id, 1, "completed").await;
        let thread = &snapshot.conversations[&id];
        let items = thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap();
        assert!(items.iter().any(|item| item.status == agent_protocol::execution::ItemStatus::Completed
            && matches!(item.body(), agent_protocol::items::ItemBody::CommandExecution {exit_code, output, ..}
                if if prompt == "background" {exit_code.is_none()} else {*exit_code == Some(0) && output == "approved"})
        ), "live outcomes must update the originating tool");
        assert!(!items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::Attachment {content, ..} if content["commandMode"] == "task-notification")));
        assert!(
            thread.turns.as_ref().unwrap()[0]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item_text(item) == Some("approved"))
        );
        let projected = agent_core::presentation::conversation::project_conversation(
            &snapshot,
            thread.clone(),
            &Some(projected),
        );
        assert!(!projected.turns[0].rows.iter().any(|row| matches!(
            &row.content, ConversationRowContent::ActivityHeader { activity } if activity.is_in_progress
        )));
        send(&store, "next turn", "after-background").await;
        completed(&store, &id, 2, "completed").await;
        send(&store, prompt, "stop-background").await;
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::Interrupt(op::Interrupt {
                thread_id: id.clone(),
                turn_id: snapshot.conversations[&id].turns.as_ref().unwrap()[2]
                    .id
                    .clone(),
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 3, "interrupted").await;
        assert!(snapshot.requests().next().is_none());
        assert!(
            !snapshot.conversations[&id].queue_held,
            "stopping an empty queue must allow the next input"
        );
        // A settings change replaces the retained CLI. The same native
        // conversation must resume after its background work was stopped.
        store
            .dispatch(Intent::SelectModelOption {
                thread_id: snapshot.navigation.draft_key.clone(),
                id: "effort".into(),
                value: Some(agent_protocol::models::ModelOptionValue::String(
                    "high".into(),
                )),
            })
            .await
            .unwrap();
        send(&store, "after stop", "after-interruption").await;
        let snapshot = completed(&store, &id, 4, "completed").await;
        assert!(
            snapshot.conversations[&id].turns.as_ref().unwrap()[3]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item_text(item) == Some("reply 4: after stop"))
        );
        let inputs: Value = serde_json::from_slice(
            &std::fs::read(root.path().join(format!(
                "claude-session-{}.json",
                host_fixture::test_support::native_id(root.path(), &id)
            )))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(inputs[3]["effort"], "high");
        assert_ne!(inputs[2]["pid"], inputs[3]["pid"]);
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_host_queue_preserves_edits_order_and_hold_across_restart() {
    use agent_protocol::{
        queue::{QueueAction, QueueControl},
        session::SubmissionDelivery,
    };
    let root = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let fixture = host(root.path(), memory.clone(), fixture_program()).await;
    let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
    store
        .dispatch(Intent::NewChat { cwd: String::new() })
        .await
        .unwrap();
    store
        .dispatch(Intent::SelectModel {
            thread_id: store.snapshot().navigation.draft_key.clone(),
            model: models::ModelRef {
                instance_id: "claude"
                    .parse::<agent_protocol::session::ProviderInstanceId>()
                    .unwrap(),
                id: "claude-fable-5-1".into(),
            },
        })
        .await
        .unwrap();
    let id = send(&store, "wait", "initial").await;
    until(&store, |snapshot| {
        snapshot.conversations[&id]
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .any(|item| item_text(item) == Some("Waiting for interruption"))
    })
    .await;
    let initial_turn = store.snapshot().conversations[&id]
        .active_turn_id()
        .unwrap();
    assert!(
        store.snapshot().conversations[&id]
            .capabilities
            .unwrap()
            .active_steering
    );
    let local = fixture.local().await.unwrap();
    assert!(
        local
            .peer
            .call(&rpc::Interrupt {
                thread_id: id.clone(),
                turn_id: "stale".into()
            })
            .await
            .is_err()
    );
    for (text, nonce) in [
        ("follow-up", "queued-a"),
        ("remove this", "queued-b"),
        ("next", "queued-c"),
    ] {
        send(&store, text, nonce).await;
    }
    let queued = until(&store, |snapshot| {
        snapshot.conversations[&id].queued_inputs.len() == 3
    })
    .await;
    let original = queued.conversations[&id].queued_inputs[0]
        .submission
        .clone();
    for action in [
        QueueAction::Pause,
        QueueAction::Edit {
            submission: rpc::Submission {
                input: vec![rpc::Input::Text {
                    text: "edited follow-up".into(),
                }],
                ..original.clone()
            },
        },
        QueueAction::Move {
            id: "queued-c".into(),
            before: Some("queued-a".into()),
        },
        QueueAction::Cancel {
            id: "queued-b".into(),
        },
    ] {
        store
            .dispatch(Intent::QueueControl(QueueControl {
                session: id.clone(),
                action,
            }))
            .await
            .unwrap();
    }
    let queued = store.snapshot();
    assert_eq!(
        queued.conversations[&id]
            .queued_inputs
            .iter()
            .map(|entry| entry.submission.client_user_message_id.as_str())
            .collect::<Vec<_>>(),
        ["queued-c", "queued-a"]
    );
    assert!(
        queued.conversations[&id]
            .queued_inputs
            .iter()
            .all(|entry| entry.delivery == SubmissionDelivery::Queued)
    );
    let native_id = host_fixture::test_support::native_id(root.path(), &id);
    let native_inputs = root
        .path()
        .join("claude-native/projects/fixture-native-project")
        .join(format!("{native_id}.inputs.json"));
    let inputs: Vec<Value> =
        serde_json::from_slice(&std::fs::read(&native_inputs).unwrap()).unwrap();
    assert_eq!(
        inputs.len(),
        1,
        "waiting input is not written to the running provider"
    );
    store
        .dispatch(Intent::Interrupt(op::Interrupt {
            thread_id: id.clone(),
            turn_id: initial_turn,
        }))
        .await
        .unwrap();
    completed(&store, &id, 1, "interrupted").await;
    assert!(store.snapshot().conversations[&id].queue_held);
    local.close().await;
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();

    let fixture = host(root.path(), memory, fixture_program()).await;
    let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
    store
        .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
        .await
        .unwrap();
    let restored = store.snapshot();
    assert!(restored.conversations[&id].queue_held);
    assert_eq!(restored.conversations[&id].queued_inputs.len(), 2);
    assert!(
        restored.conversations[&id].queued_inputs[1]
            .submission
            .input
            .contains(&rpc::Input::Text {
                text: "edited follow-up".into()
            })
    );
    store
        .dispatch(Intent::QueueControl(QueueControl {
            session: id.clone(),
            action: QueueAction::Resume,
        }))
        .await
        .unwrap();
    let finished = completed(&store, &id, 3, "completed").await;
    let turns = finished.conversations[&id].turns.as_ref().unwrap();
    for (turn, nonce) in turns[1..].iter().zip(["queued-c", "queued-a"]) {
        assert_eq!(turn.status.label(), "completed");
        assert_eq!(
            turn.items
                .iter()
                .flatten()
                .filter(|item| item.client_input_id.as_deref() == Some(nonce))
                .count(),
            1
        );
    }
    assert!(
        turns[2]
            .items
            .iter()
            .flatten()
            .any(|item| item_text(item) == Some("reply 3: edited follow-up"))
    );
    assert!(finished.conversations[&id].queued_inputs.is_empty());
    assert!(finished.pending_submissions.is_empty());
    let local = fixture.local().await.unwrap();
    assert_eq!(
        local.peer.call(&original).await.unwrap().turn_id.as_ref(),
        Some(&turns[2].id),
        "replay uses the immutable admission even after editing"
    );
    let inputs: Vec<Value> =
        serde_json::from_slice(&std::fs::read(&native_inputs).unwrap()).unwrap();
    assert_eq!(inputs.len(), 3);
    assert_eq!(
        inputs[1]["pid"], inputs[2]["pid"],
        "consecutive queued turns reuse the provider process"
    );
    send(&store, "permission", "live-refresh").await;
    until(&store, |snapshot| snapshot.requests().next().is_some()).await;
    store
        .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
        .await
        .unwrap();
    let refreshed = store.snapshot();
    let turns = refreshed.conversations[&id].turns.as_ref().unwrap();
    assert_eq!(
        turns.len(),
        4,
        "refresh cannot append another occurrence of the live turn"
    );
    assert_eq!(
        turns
            .last()
            .unwrap()
            .items
            .iter()
            .flatten()
            .filter(|item| matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            ))
            .count(),
        1
    );
    store
        .dispatch(Intent::Interrupt(op::Interrupt {
            thread_id: id.clone(),
            turn_id: turns.last().unwrap().id.clone(),
        }))
        .await
        .unwrap();
    completed(&store, &id, 4, "interrupted").await;
    local.close().await;
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test]
async fn permission_settings_edit_native_claude_defaults_without_own_storage() {
    use agent_protocol::permissions::*;
    let root = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let fixture = host(root.path(), memory.clone(), fixture_program()).await;
    let local = fixture.local().await.unwrap();
    let read = ReadPermissionSettings {
        instance_id: "claude"
            .parse::<agent_protocol::session::ProviderInstanceId>()
            .unwrap(),
    };
    let mut settings = local.peer.call(&read).await.unwrap();
    assert_eq!(settings.mode, None);
    let path = root.path().join("claude-native/settings.json");
    for (mode, native_mode) in [
        (PermissionMode::FullAccess, "bypassPermissions"),
        (PermissionMode::Ask, "default"),
        (PermissionMode::Auto, "auto"),
    ] {
        settings = local
            .peer
            .call(&UpdatePermissionSettings {
                instance_id: read.instance_id.clone(),
                mode,
                version: settings.version,
            })
            .await
            .unwrap();
        assert_eq!(settings.mode, Some(mode));
        let native: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(native, json!({"permissions":{"defaultMode":native_mode}}));
    }
    local.endpoint.close().await;
    fixture.close().await.unwrap();
    let restarted = host(root.path(), memory, fixture_program()).await;
    let local = restarted.local().await.unwrap();
    assert_eq!(
        local.peer.call(&read).await.unwrap().mode,
        Some(PermissionMode::Auto)
    );
    local.endpoint.close().await;
    restarted.close().await.unwrap();
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

#[tokio::test]
async fn creating_default_claude_chat_does_not_launch_the_cli_or_read_models() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing-claude-cli");
    let fixture = host(root.path(), Arc::new(Memory::default()), &missing).await;
    let local = fixture.local().await.unwrap();
    let response = local
        .peer
        .call(&op::CreateSession {
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
            cwd: Some(root.path().to_string_lossy().into()),
            model: None,
        })
        .await
        .unwrap();
    assert_eq!(
        response.response.model.unwrap(),
        models::ModelRef {
            instance_id: "claude"
                .parse::<agent_protocol::session::ProviderInstanceId>()
                .unwrap(),
            id: "claude-fable-5-1".into()
        }
    );
    assert_eq!(
        response.response.thread.provider,
        Some(agent_protocol::providers::ProviderRef {
            instance_id: "claude".parse().unwrap(),
            driver: "claudeAgent".parse().unwrap()
        })
    );
    local.close().await;
    fixture.close().await.unwrap();
}
