use agent_protocol::session::ProviderKind;
use agent_protocol::{models, operations as rpc};
use std::{path::Path, sync::Arc, time::Duration};

use agent_protocol::requests::Answer;

use agent_core::questions::QuestionAction;

use agent_core::state::Attachment;

use agent_core::state::Intent;

use agent_core::state::Snapshot;

use agent_core::state::operations as op;

use agent_core::store::Outcome;

use agent_core::store::Store;

use agent_transport::transport::Endpoint;

use agent_transport::transport::Relays;
use codex_app_server::AppServerConfig;
use host_fixture::test_support::{HostFixture, Memory, assert_project_directory};
use serde_json::{Value, json};

mod codex_fixture;

async fn host(root: &Path, memory: Arc<Memory>, program: &Path) -> HostFixture {
    let existing = root.join("agent-codex-fixture");
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
    let store = Store::connect(&endpoint, &host.ticket, snapshot, None, "Fixture device")
        .await
        .unwrap();
    let (codex, claude) = tokio::join!(
        store.dispatch(Intent::LoadModels(op::LoadModels {
            provider: ProviderKind::Codex
        })),
        store.dispatch(Intent::LoadModels(op::LoadModels {
            provider: ProviderKind::Claude
        })),
    );
    codex.unwrap();
    claude.unwrap();
    (store, endpoint)
}

async fn draft(store: &Store, text: &str) {
    let key = store.snapshot().pane.navigation.draft_key.clone();
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
    let thread_id = store.snapshot().pane.navigation.thread_id.clone();
    let outcome = store
        .dispatch(Intent::Submit {
            thread_id: thread_id.clone(),
            client_user_message_id: client_id.into(),
        })
        .await
        .unwrap();
    assert!(if thread_id.is_some() {
        matches!(outcome, Outcome::Submitted { .. })
    } else {
        matches!(outcome, Outcome::StartedThread { .. })
    });
    store.snapshot().pane.navigation.thread_id.clone().unwrap()
}

/// Read the inputs the Claude fixture appended to its workspace trace.
// Native fixture traces use the provider's private ID, independently of the
// public task address. These cases each own one Claude conversation per cwd.
fn session_trace(cwd: &Path) -> std::path::PathBuf {
    let traces: Vec<_> = std::fs::read_dir(cwd)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("claude-session-") && name.ends_with(".jsonl")
            })
        })
        .collect();
    assert_eq!(traces.len(), 1, "one native session trace in {cwd:?}");
    traces.into_iter().next().unwrap()
}

fn session_inputs(path: impl AsRef<Path>) -> serde_json::Value {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect()
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
        let snapshot = store.snapshot();
        panic!(
            "Claude state did not settle: error={:?}, turns={:?}, requests={:?}, expanded={:?}, root={:?}, pages={:?}",
            snapshot.pane.error,
            snapshot
                .sessions.conversations
                .values()
                .flat_map(|thread| thread.turns.iter().flatten())
                .map(|turn| (&turn.id, &turn.status, &turn.error))
                .collect::<Vec<_>>(),
            snapshot
                .requests()
                .map(|request| (&request.id, &request.body))
                .collect::<Vec<_>>(),
            snapshot.lists.expanded_projects,
            snapshot.lists.threads.as_ref().map(|list| (
                list.projects.iter().map(|project| &project.id).collect::<Vec<_>>(),
                list.data.iter().map(|thread| (&thread.id, &thread.project_id)).collect::<Vec<_>>(),
            )),
            snapshot.lists.project_threads.iter().map(|(id, list)| (
                id,
                list.data.iter().map(|thread| &thread.id).collect::<Vec<_>>(),
            )).collect::<Vec<_>>(),
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
            .sessions
            .conversations
            .get(id)
            .and_then(|thread| thread.turns.as_ref())
            .is_some_and(|turns| {
                turns.len() == count && turns.last().unwrap().status.label() == status
            })
    })
    .await
}

async fn listed(
    store: &Store,
    id: &agent_protocol::session::SessionRef,
    project_root: Option<&Path>,
) -> Arc<Snapshot> {
    store
        .dispatch(Intent::ListSessions(op::ListSessions::new(
            Default::default(),
        )))
        .await
        .unwrap();
    if let Some(project_root) = project_root {
        let snapshot = until(store, |snapshot| {
            snapshot.lists.threads.as_ref().is_some_and(|list| {
                list.projects.iter().any(|project| {
                    project
                        .roots
                        .iter()
                        .any(|root| Path::new(&root.path) == project_root)
                })
            })
        })
        .await;
        let project_id = snapshot
            .lists
            .threads
            .as_ref()
            .unwrap()
            .projects
            .iter()
            .find(|project| {
                project
                    .roots
                    .iter()
                    .any(|root| Path::new(&root.path) == project_root)
            })
            .unwrap()
            .id
            .clone();
        store
            .dispatch(Intent::SetProjectExpanded {
                project_id,
                expanded: true,
            })
            .await
            .unwrap();
    }
    until(store, |snapshot| {
        snapshot.thread_list().is_some_and(|list| {
            list.threads
                .iter()
                .chain(list.projects.iter().flat_map(|project| &project.threads))
                .any(|thread| &thread.id == id)
        })
    })
    .await
}

fn fixture_program() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_agent-claude-fixture"))
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
        let snapshot = store.snapshot();
        for (model, display) in [
            ("default", "Default (recommended)"),
            ("opus[1m]", "Opus (1M context)"),
            ("claude-fable-5-1[1m]", "Fable"),
            ("sonnet", "Sonnet"),
            ("haiku", "Haiku"),
            ("custom", "Custom model"),
        ] {
            let entry = snapshot
                .catalog
                .models
                .iter()
                .find(|entry| {
                    entry.model.provider == ProviderKind::Claude && entry.model.id == model
                })
                .unwrap();
            assert_eq!(entry.display_name, display);
        }
        let catalog = root.path().join("claude-native/fixture-models.json");
        std::fs::write(
            &catalog,
            serde_json::to_vec(&json!([
                {"value":"default","displayName":"Updated default"},
                {"value":"new-model","displayName":"New model","supportedEffortLevels":["high"]}
            ]))
            .unwrap(),
        )
        .unwrap();
        let claude_models = |snapshot: &Snapshot| {
            snapshot
                .catalog
                .models
                .iter()
                .filter(|model| model.model.provider == ProviderKind::Claude)
                .cloned()
                .collect::<Vec<_>>()
        };
        // Discovery answers from its last result at once and refreshes behind
        // it, so a later read observes the change without restarting the Host.
        store
            .dispatch(Intent::LoadModels(op::LoadModels {
                provider: ProviderKind::Claude,
            }))
            .await
            .unwrap();
        assert_eq!(claude_models(&store.snapshot()).len(), 6);
        while claude_models(&store.snapshot()).len() == 6 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            store
                .dispatch(Intent::LoadModels(op::LoadModels {
                    provider: ProviderKind::Claude,
                }))
                .await
                .unwrap();
        }
        let snapshot = store.snapshot();
        let claude = claude_models(&snapshot);
        assert_eq!(claude.len(), 2, "removed models must disappear");
        assert_eq!(claude[0].display_name, "Updated default");
        assert_eq!(claude[1].display_name, "New model");
        assert_eq!(
            claude[1].model,
            agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: "new-model".into()
            }
        );
        assert_eq!(
            claude[1]
                .settings
                .as_ref()
                .unwrap()
                .default_reasoning_effort,
            "high"
        );
        assert!(snapshot.catalog.model_errors.is_empty());
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("model refresh deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_execution_delegates_model_and_effort_to_cli_without_catalog_reads() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("claude-native")).unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        // Any additional catalog read would now fail to parse.
        std::fs::write(
            root.path().join("claude-native/fixture-models.json"),
            "null",
        )
        .unwrap();
        let local = fixture.local().await.unwrap();
        for (model, effort) in [("unlisted-model", "low"), ("default", "xhigh")] {
            std::fs::write(
                root.path().join("claude-fixture.json"),
                json!({"expectedModel":model}).to_string(),
            )
            .unwrap();
            let response = local
                .peer
                .call(&op::CreateSession {
                    initial_input: None,
                    provider: agent_protocol::session::ProviderKind::Claude,
                    cwd: Some(root.path().to_string_lossy().into()),
                    model: Some(agent_protocol::models::ModelRef {
                        provider: ProviderKind::Claude,
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
                        provider: ProviderKind::Claude,
                        id: model.into(),
                    }),
                    effort: Some(effort.into()),
                    service_tier: None,
                })
                .await
                .unwrap();
            completed(&store, &id, 1, "completed").await;
            let session = &id.id;
            let inputs =
                session_inputs(root.path().join(format!("claude-session-{session}.jsonl")));
            assert_eq!(inputs[0]["model"], model);
            assert_eq!(inputs[0]["effort"], effort);
        }
        local.close().await;
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .expect("CLI settings delegation deadline");
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_submission_preserves_inputs_settings_workspaces_and_history_across_host_restart(
    #[values(false, true)] automatic: bool,
    #[values(false, true)] selected: bool,
    #[values("none", "image", "file")] attachment: &str,
) {
    tokio::time::timeout(Duration::from_secs(120), async {
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
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key.clone(), model: agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Claude, id: "default".into() } }).await.unwrap();
        store.dispatch(Intent::SelectEffort { thread_id: key, effort: "low".into() }).await.unwrap();
        let mut previous_id = None;
        let mut previous_cwd = None;
        for number in 0..2 {
            let key = store.snapshot().pane.navigation.draft_key.clone();
            if attachment != "none" {
                let path = root.join(if attachment == "image" { "photo.png" } else { "note.txt" });
                if attachment == "image" {
                    std::fs::write(&path, include_bytes!("../../../apps/mobile/iosApp/Bex/Assets.xcassets/AppIcon.appiconset/AppIcon.png")).unwrap();
                } else { std::fs::write(&path, "attachment content").unwrap(); }
                store.dispatch(Intent::UploadAttachment(op::UploadAttachment {
                    draft_key: key,
                    attachment: Attachment { path: path.to_str().unwrap().into(), name: path.file_name().unwrap().to_str().unwrap().into(), is_image: attachment == "image" },
                })).await.unwrap();
            }
            let id = send(&store, &format!("message {number}"), &format!("client-{number}")).await;
            if let Some(previous) = &previous_id { assert_eq!(&id, previous); }
            let snapshot = completed(&store, &id, number + 1, "completed").await;
            assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
            assert!(snapshot.sessions.pending_submissions.is_empty());
            assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].attachments.is_empty());
            assert_eq!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].model.as_ref().map(|model| model.id.as_str()), Some("default"));
            assert_eq!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].effort.as_deref(), Some("low"));
            let cwd = snapshot.pane.navigation.cwd.clone();
            if let Some(previous) = &previous_cwd { assert_eq!(&cwd, previous); }
            assert_project_directory(
                Path::new(&cwd),
                &if selected { workspace.clone() } else { root.join("bex-chats") },
                (selected && automatic).then_some(root.join("worktrees").as_path()),
            );
            let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[number];
            let items = turn.items.as_ref().unwrap();
            assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::Reasoning { .. }) && item_text(item) == Some("Fixture reasoning")));
            let responses: Vec<_> = items.iter().filter(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. })).collect();
            assert_eq!(responses.len(), 1, "streaming and completed blocks must not duplicate");
            assert!(item_text(responses[0]).unwrap().starts_with(&format!("reply {}: message {number}", number + 1)));
            let user = items.iter().find(|item| matches!(item.body(), agent_protocol::items::ItemBody::UserMessage { .. })).unwrap();
            assert_eq!(user.client_input_id.as_deref(), Some(format!("client-{number}").as_str()));
            assert!(matches!(user.body(), agent_protocol::items::ItemBody::UserMessage { content, .. } if content.first() == Some(&agent_protocol::items::MessagePart::Text { text: format!("message {number}") })));
            let inputs = session_inputs(session_trace(Path::new(&cwd)));
            assert_eq!(inputs.as_array().unwrap().len(), number + 1);
            assert_eq!(inputs[number]["effort"], "low");
            assert_eq!(inputs[number]["content"].as_array().unwrap().len(), if attachment == "none" { 1 } else { 2 });
            if attachment == "image" { assert_eq!(inputs[number]["content"][1]["source"]["media_type"], "image/png"); }
            if attachment == "file" { assert!(inputs[number]["content"][1]["text"].as_str().unwrap().contains("note.txt")); }
            listed(&store, &id, selected.then_some(workspace.as_path())).await;
            store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
            assert_eq!(store.snapshot().sessions.conversations[&id].turns.as_ref().unwrap().len(), number + 1);
            let saved: Snapshot = serde_json::from_slice(&serde_json::to_vec(store.snapshot().as_ref()).unwrap()).unwrap();
            store.close().await.unwrap();
            endpoint.close().await;
            fixture.close().await.unwrap();
            fixture = host(&root, memory.clone(), fixture_program()).await;
            (store, endpoint) = connect(&fixture, saved).await;
            store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
            assert_eq!(store.snapshot().sessions.conversations[&id].turns.as_ref().unwrap().len(), number + 1);
            previous_id = Some(id);
            previous_cwd = Some(cwd);
        }
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
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
                thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                model: agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "default".into(),
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
            assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
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
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "default".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "permission-1").await;
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        let cwd = store.snapshot().pane.navigation.cwd.clone();
        assert!(!Path::new(&cwd).join("approved.txt").exists());
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None, "Fixture device")
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
        // The composer answers the question; a chosen option sends it and
        // returns the typed text to the draft.
        send(&store, "question", "question").await;
        let snapshot = until(&store, |snapshot| {
            snapshot.question_prompt(id.clone()).is_some()
        })
        .await;
        let prompt = snapshot.question_prompt(id.clone()).unwrap();
        let blue = prompt
            .question
            .choices
            .iter()
            .find(|choice| choice.label == "Blue")
            .unwrap();
        for action in [
            QuestionAction::Edit {
                text: "Green please".into(),
            },
            QuestionAction::Choose {
                choice_id: blue.id.clone(),
            },
        ] {
            store
                .dispatch(Intent::AnswerQuestion {
                    request_id: prompt.request_id.clone(),
                    action,
                })
                .await
                .unwrap();
        }
        let snapshot = completed(&store, &id, 3, "completed").await;
        assert!(
            snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item_text(item) == Some("Blue"))
        );
        assert_eq!(
            snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text,
            "Green please"
        );
        assert!(snapshot.question_prompt(id.clone()).is_none());
        send(&store, "wait", "wait").await;
        let snapshot = until(&store, |snapshot| {
            snapshot.sessions.conversations[&id]
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
        let turn_id = snapshot.sessions.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .last()
            .unwrap()
            .id
            .clone();
        send(&store, "wait", "busy").await;
        assert!(store.snapshot().pane.error.is_none());
        assert!(
            store.snapshot().input.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
        until(&store, |snapshot| {
            !snapshot.sessions.pending_submissions.contains_key("busy")
        })
        .await;
        let local = fixture.local().await.unwrap();
        let previous_turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2].id;
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
            store.snapshot().sessions.conversations[&id]
                .turns
                .as_ref()
                .unwrap()
                .last()
                .unwrap()
                .status
                .label(),
            "running"
        );
        store
            .dispatch(Intent::Interrupt(op::Interrupt {
                thread_id: id.clone(),
                turn_id,
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 4, "interrupted").await;
        let turn = snapshot.sessions.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .last()
            .unwrap();
        assert!(turn.error.is_none(), "{:?}", turn.error);
        // Stopping while Claude awaits an answer withdraws the question.
        send(&store, "question", "stopped-question").await;
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::Interrupt(op::Interrupt {
                thread_id: id.clone(),
                turn_id: snapshot.sessions.conversations[&id]
                    .turns
                    .as_ref()
                    .unwrap()
                    .last()
                    .unwrap()
                    .id
                    .clone(),
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 5, "interrupted").await;
        let turn = snapshot.sessions.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .last()
            .unwrap();
        assert!(turn.error.is_none(), "{:?}", turn.error);
        assert!(snapshot.requests().next().is_none());
        send(&store, "after interruption", "recovered").await;
        let snapshot = completed(&store, &id, 6, "completed").await;
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert!(
            snapshot.requests().next().is_none()
                && snapshot.sessions.pending_submissions.is_empty()
        );
        assert!(
            snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
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
async fn provider_switch_preserves_conversation_identity_history_and_workspace() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        let codex_model = store
            .snapshot()
            .catalog
            .models
            .iter()
            .find(|model| model.model.provider == ProviderKind::Codex)
            .unwrap()
            .model
            .clone();
        let claude_model = agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: "default".into(),
        };
        for (original, other) in [
            (codex_model.clone(), claude_model.clone()),
            (claude_model, codex_model),
        ] {
            store
                .dispatch(Intent::NewChat {
                    cwd: root.path().to_string_lossy().into_owned(),
                })
                .await
                .unwrap();
            let key = store.snapshot().pane.navigation.draft_key.clone();
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
            let original_snapshot = completed(&store, &id, 1, "completed").await;
            let cwd = original_snapshot.pane.navigation.cwd.clone();

            let mut expected = vec!["original provider"];
            for (step, model, text) in [
                ("handoff", &other, "keep this input"),
                ("return", &original, "continue with original provider"),
            ] {
                store
                    .dispatch(Intent::SelectModel {
                        thread_id: id.clone().into(),
                        model: model.clone(),
                    })
                    .await
                    .unwrap();
                assert_eq!(
                    send(&store, text, &format!("{}-{step}", original.id)).await,
                    id
                );
                expected.push(text);
                completed(&store, &id, expected.len(), "completed").await;
                let snapshot = until(&store, |snapshot| {
                    snapshot.sessions.conversations[&id].active_provider == Some(model.provider)
                })
                .await;
                assert_eq!(snapshot.pane.navigation.thread_id.as_ref(), Some(&id));
                assert_eq!(snapshot.pane.navigation.cwd, cwd);
                assert!(
                    snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                        .text
                        .is_empty()
                );
                assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
                assert!(snapshot.sessions.pending_submissions.is_empty());
                let messages: Vec<_> = snapshot.sessions.conversations[&id]
                    .turns
                    .iter()
                    .flatten()
                    .flat_map(|turn| turn.items.iter().flatten())
                    .filter(|item| {
                        matches!(
                            item.body(),
                            agent_protocol::items::ItemBody::UserMessage { .. }
                        )
                    })
                    .filter_map(|item| agent_core::presentation::body::item_body(item).text)
                    .collect();
                assert_eq!(messages, expected);
            }
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
                    provider: ProviderKind::Claude,
                    id: "sonnet".into(),
                }),
                effort: Some("high".into()),
                service_tier: Some("default".into()),
                text: "Keep the Claude draft".into(),
                ..Default::default()
            };
            let mut snapshot = Snapshot::default();
            Arc::make_mut(&mut snapshot.input.drafts)
                .insert("saved-claude".into(), Arc::new(saved.clone()));
            let (store, endpoint) = connect(&fixture, snapshot).await;
            assert_eq!(
                *store.snapshot().input.drafts[&agent_core::state::DraftKey::from("saved-claude")],
                saved
            );
            assert!(!store.snapshot().catalog.models.is_empty());
            assert!(
                store.snapshot().catalog.model_errors.is_empty(),
                "installed={installed}: {:?}",
                store.snapshot().catalog.model_errors
            );
            assert!(
                store
                    .snapshot()
                    .catalog
                    .models
                    .iter()
                    .all(|model| model.model.provider == ProviderKind::Codex)
            );

            store
                .dispatch(Intent::NewChat { cwd: String::new() })
                .await
                .unwrap();
            store
                .dispatch(Intent::SelectModel {
                    thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                    model: store.snapshot().catalog.models[0].model.clone(),
                })
                .await
                .unwrap();
            let id = send(&store, "Codex remains available", "codex-only").await;
            let snapshot = completed(&store, &id, 1, "completed").await;
            assert!(snapshot.pane.error.is_none());
            assert!(
                snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                    .text
                    .is_empty()
                    && snapshot.sessions.pending_submissions.is_empty()
            );
            store.close().await.unwrap();
            endpoint.close().await;
            fixture.close().await.unwrap();
        }
    })
    .await
    .expect("unconfigured Claude deadline");
}

#[rstest::rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_codex_keeps_claude_inputs_workspaces_and_resumed_history_usable(
    #[values(false, true)] automatic: bool,
    #[values(false, true)] selected: bool,
) {
    tokio::time::timeout(Duration::from_secs(90), async {
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
            assert!(store.snapshot().connection.connected);
            assert!(store.snapshot().catalog.model_errors.contains_key("codex"));
            assert!(store.snapshot().catalog.models.iter().all(|model| model.model.provider == ProviderKind::Claude));
            if let Some(id) = &thread_id {
                store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
            } else {
                store.dispatch(Intent::NewChat { cwd: if selected { workspace.to_str().unwrap().into() } else { String::new() } }).await.unwrap();
                let key = store.snapshot().pane.navigation.draft_key.clone();
                store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Claude, id: "default".into() } }).await.unwrap();
            }
            let id = send(&store, &format!("independent {index}"), &format!("independent-{index}")).await;
            let snapshot = completed(&store, &id, index + 1, "completed").await;
            assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
            assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.sessions.pending_submissions.is_empty());
            let current = snapshot.sessions.conversations[&id].cwd.clone().unwrap();
            if let Some(previous) = &cwd { assert_eq!(previous, &current); }
            assert_project_directory(
                Path::new(&current),
                &if selected { workspace.clone() } else { root.join("bex-chats") },
                (selected && automatic).then_some(root.join("worktrees").as_path()),
            );
            let turns = snapshot.sessions.conversations[&id].turns.as_ref().unwrap();
            assert!(turns[index].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.starts_with(&format!("reply {}: independent {index}", index + 1)))));
            listed(&store, &id, selected.then_some(workspace.as_path())).await;
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
            let drafts = store.snapshot().input.drafts.clone();
            let dictation = management.peer.request::<rpc::Transcription>(&agent_protocol::protocol::Call::Transcribe(serde_json::from_value::<rpc::Transcribe>(json!({"preparation":null,"audio":"AAA="})).unwrap())).await.map(|output| serde_json::to_value(output).unwrap());
            assert!(dictation.is_err());
            assert_eq!(*store.snapshot().input.drafts, *drafts);
            assert!(management.peer.call(&op::ReadWorktreeSettings {}).await.is_ok());
            let count_worktrees = || std::fs::read_dir(root.join("worktrees")).map(|entries| entries.count()).unwrap_or_default();
            let before = count_worktrees();
            assert!(management.peer.call(&serde_json::from_value::<op::CreateSession>(json!({"provider":"codex","model":{"provider":"codex","id":"fixture-model"},"cwd":current})).unwrap()).await.is_err());
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
        let model = store
            .snapshot()
            .catalog
            .models
            .iter()
            .find(|model| model.model.provider == ProviderKind::Codex)
            .unwrap()
            .model
            .clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                model,
            })
            .await
            .unwrap();
        let codex_id = send(&store, "[approval]", "codex-approval").await;
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::NewChat { cwd: String::new() })
            .await
            .unwrap();
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "default".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "permission", "claude-approval").await;
        until(&store, |snapshot| snapshot.requests().count() == 2).await;
        let cwd = store.snapshot().pane.navigation.cwd.clone();
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
        let _ = local
            .peer
            .call(&agent_protocol::live_activity::ReadTaskActivity {})
            .await;
        let list = local
            .peer
            .call(&rpc::ListSessions::new(Default::default()))
            .await
            .unwrap();
        assert!(
            list.data
                .iter()
                .any(|thread| thread.id.as_ref() == Some(&id))
        );
        until(&store, |snapshot| {
            snapshot.requests().count() == 1
                && !snapshot.lists.activity.active[&codex_id]
                && snapshot.sessions.conversations[&codex_id]
                    .turns
                    .as_ref()
                    .unwrap()[0]
                    .status
                    .label()
                    == "failed"
        })
        .await;
        assert!(store.snapshot().connection.connected);
        assert!(!fixture.running.is_finished());
        assert!(!store.snapshot().lists.activity.active[&codex_id]);
        assert!(
            matches!(
                store.snapshot().terminals["codex-terminal"].phase,
                agent_core::state::TerminalPhase::Running
            ),
            "Host-owned terminals must survive Codex exit"
        );
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None, "Fixture device")
            .await
            .unwrap();
        let snapshot = until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        assert_eq!(snapshot.requests().count(), 1);
        let request = snapshot.requests().next().unwrap();
        assert!(
            snapshot.sessions.conversations[&id]
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
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert!(
            snapshot.requests().next().is_none()
                && snapshot.sessions.pending_submissions.is_empty()
        );
        assert!(
            snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
        assert_eq!(
            std::fs::read_to_string(Path::new(&cwd).join("approved.txt")).unwrap(),
            "approved"
        );
        store
            .dispatch(Intent::LoadModels(op::LoadModels {
                provider: ProviderKind::Codex,
            }))
            .await
            .unwrap();
        assert!(store.snapshot().catalog.model_errors.contains_key("codex"));
        send(&store, "after Codex exit", "claude-after-exit").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert!(
            snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
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
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store
            .dispatch(Intent::SelectModel {
                thread_id: key,
                model: agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "default".into(),
                },
            })
            .await
            .unwrap();
        draft(&store, "keep my draft").await;
        store.dispatch(Intent::Submit {
            thread_id: None,
            client_user_message_id: "auth-failure".into(),
        }).await.unwrap();
        let id = store.snapshot().pane.navigation.thread_id.clone().unwrap();
        let failed_preparation = |snapshot: &Snapshot| {
            snapshot.sessions.conversations[&id].preparation.as_ref().is_some_and(|preparation| matches!(preparation.status, agent_protocol::session::PreparationStatus::Failed { .. }))
        };
        let snapshot = until(&store, failed_preparation).await;
        let preparation = snapshot.sessions.conversations[&id].preparation.as_ref().unwrap();
        assert!(matches!(&preparation.status, agent_protocol::session::PreparationStatus::Failed { error } if error.message.contains("サブスク認証")));
        assert!(preparation.submission.input.iter().any(|input| matches!(input, rpc::Input::Text { text } if text == "keep my draft")));
        assert!(snapshot.sessions.pending_submissions[&agent_protocol::ids::ClientInputId::from("auth-failure")].accepted);
        assert!(snapshot.sessions.conversations[&id].turns.as_ref().is_none_or(|turns| turns.is_empty()));
        std::fs::write(&config, json!({"initializeError":true}).to_string()).unwrap();
        store.dispatch(Intent::RetryPreparation(op::RetryPreparation { thread_id: id.clone() })).await.unwrap();
        let snapshot = until(&store, |snapshot| {
            snapshot.sessions.conversations[&id].preparation.as_ref().is_some_and(|preparation| matches!(&preparation.status, agent_protocol::session::PreparationStatus::Failed { error } if error.message.contains("fixture initialization failed")))
        }).await;
        assert!(snapshot.sessions.conversations[&id].turns.as_ref().is_none_or(|turns| turns.is_empty()));
        std::fs::write(&config, json!({"resultError":true}).to_string()).unwrap();
        store.dispatch(Intent::RetryPreparation(op::RetryPreparation { thread_id: id.clone() })).await.unwrap();
        let snapshot = completed(&store, &id, 1, "failed").await;
        assert_eq!(
            snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0]
                .error
                .as_ref()
                .unwrap()
                .message,
            "fixture inference failed"
        );
        std::fs::write(&config, "{}").unwrap();
        send(&store, "retry", "retry").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.operation_error(op::OperationKey::Submission { draft_key: (&id).into() }).is_none());
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert!(
            snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)]
                .text
                .is_empty()
        );
        assert!(
            snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[1]
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
        let program = std::env::var_os("AGENT_LIVE_CLAUDE_PROGRAM").expect("set an absolute Claude Code executable path");
        let root = tempfile::tempdir().unwrap();
        let memory = Arc::new(Memory::default());
        let fixture = host(root.path(), memory.clone(), Path::new(&program)).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store.dispatch(Intent::NewChat { cwd: String::new() }).await.unwrap();
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Claude, id: "haiku".into() } }).await.unwrap();
        let id = send(&store, "Remember marker AGENT_CLAUDE_STORE_OK. Reply with exactly that marker. Do not use tools.", "live-1").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        assert!(snapshot.pane.error.is_none());
        assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.sessions.pending_submissions.is_empty());
        assert!(snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("AGENT_CLAUDE_STORE_OK"))));
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
        let fixture = host(root.path(), memory, Path::new(&program)).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        store.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))).await.unwrap();
        send(&store, "Reply with exactly the marker from my previous message. Do not use tools.", "live-2").await;
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.pane.error.is_none());
        assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.sessions.pending_submissions.is_empty());
        assert!(snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[1].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("AGENT_CLAUDE_STORE_OK"))));
        send(&store, "Count from 1 to 10000, one number per line. Do not use tools.", "live-stop").await;
        let snapshot = until(&store, |snapshot| snapshot.sessions.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 3 && turns[2].items.as_ref().is_some_and(|items| items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| !text.is_empty()))))).await;
        let turn_id = snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2].id.clone();
        store.dispatch(Intent::Interrupt(op::Interrupt { thread_id: id.clone(), turn_id })).await.unwrap();
        let snapshot = completed(&store, &id, 3, "interrupted").await;
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2];
        assert!(turn.error.is_none(), "{:?}", turn.error);
        send(&store, "Reply with exactly AGENT_CLAUDE_RECOVERED. Do not use tools.", "live-recovery").await;
        let snapshot = completed(&store, &id, 4, "completed").await;
        assert!(snapshot.pane.error.is_none());
        assert!(snapshot.input.drafts[&agent_core::state::DraftKey::from(&id)].text.is_empty() && snapshot.sessions.pending_submissions.is_empty());
        assert!(snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[3].items.as_ref().unwrap().iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("AGENT_CLAUDE_RECOVERED"))));
        send(&store, "Count from 1 to 100, one number per line. Do not use tools.", "live-before-additional").await;
        until(&store, |snapshot| snapshot.sessions.conversations[&id].turns.as_ref().is_some_and(|turns| turns.len() == 5 && turns[4].items.as_ref().is_some_and(|items| items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| !text.is_empty()))))).await;
        assert!(agent_protocol::session::input_unavailable_reason(&store.snapshot().sessions.conversations[&id]).is_none());
        send(&store, "Reply with exactly AGENT_CLAUDE_ADDITIONAL_OK. Do not use tools.", "live-additional").await;
        let snapshot = completed(&store, &id, 5, "completed").await;
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert!(snapshot.sessions.pending_submissions.is_empty());
        let items = snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[4].items.as_ref().unwrap();
        assert_eq!(items.iter().filter(|item| item.client_input_id.as_deref() == Some("live-additional")).count(), 1);
        assert!(items.iter().any(|item| matches!(item.body(), agent_protocol::items::ItemBody::AssistantText { .. }) && item_text(item).is_some_and(|text| text.contains("AGENT_CLAUDE_ADDITIONAL_OK"))));
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
            thread_id: store.snapshot().pane.navigation.draft_key.clone(),
            model: agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Claude,
                id: "default".into(),
            },
        })
        .await
        .unwrap();
    let id = send(&store, "first", "reuse-1").await;
    completed(&store, &id, 1, "completed").await;
    send(&store, "second", "reuse-2").await;
    let snapshot = completed(&store, &id, 2, "completed").await;
    let inputs = session_inputs(session_trace(Path::new(&snapshot.pane.navigation.cwd)));
    assert!(inputs[0]["process"].is_string());
    assert_eq!(
        inputs[0]["process"], inputs[1]["process"],
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
                thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                model: agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "default".into(),
                },
            })
            .await
            .unwrap();
        let id = send(&store, "first", "before-removal").await;
        let snapshot = completed(&store, &id, 1, "completed").await;
        let cwd = snapshot.pane.navigation.cwd.clone();
        let inputs_path = session_trace(Path::new(&cwd));
        let before = session_inputs(&inputs_path);
        std::fs::remove_dir_all(&cwd).unwrap();
        assert_eq!(send(&store, "second", "after-removal").await, id);
        let snapshot = completed(&store, &id, 2, "completed").await;
        assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
        assert_eq!(snapshot.pane.navigation.cwd, cwd);
        let after = session_inputs(&inputs_path);
        assert_ne!(
            before[0]["process"], after[1]["process"],
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
        let input = b"printf 'AGENT_%s\\n' 'PTY_READY'; stty size\n".as_slice();
        #[cfg(windows)]
        let input = b"echo AGENT_PTY_READY\r\n".as_slice();
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
                if text.contains("AGENT_PTY_READY") {
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
        std::fs::write(native.join("fixture-auth.json"), json!({"tokenSource":"oauth","apiKeySource":"none","email":"native@example.invalid","subscriptionType":"pro"}).to_string()).unwrap();
        std::fs::create_dir_all(native.join("skills/account-test")).unwrap();
        std::fs::write(native.join("skills/account-test/SKILL.md"), "shared skill").unwrap();
        std::fs::write(native.join("settings.json"), r#"{"model":"shared-model"}"#).unwrap();
        let memory = Arc::new(Memory::default());
        let start = || HostFixture::start(&root, AppServerConfig { program: root.join("missing-codex"), ..Default::default() }, memory.clone(), "Claude accounts", false, Some(fixture_program()));
        std::fs::write(native.join("usage-paused"), root.join("claude/accounts").to_string_lossy().as_bytes()).unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        // Usage stays blocked until after selection. The outer deadline detects
        // waiting for it without assuming a machine-specific SDK startup time.
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        while !native.join("usage-requested").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store.dispatch(Intent::SelectAccountForDraft(op::SelectAccountForDraft { provider: agent_protocol::session::ProviderKind::Claude,
            id: "claude:desktop".into(), thread_id: key,
        })).await.unwrap();
        assert!(store.snapshot().catalog.account.accounts.as_ref().unwrap().accounts[0].usage.is_none());
        std::fs::remove_file(native.join("usage-paused")).unwrap();
        until(&store, |snapshot| snapshot.catalog.account.accounts.as_ref().is_some_and(|accounts| accounts.accounts[0].usage.is_some())).await;
        let usage = store.snapshot().catalog.account.accounts.as_ref().unwrap().accounts[0].usage.clone().unwrap();
        assert!(usage.error.is_none(), "{usage:?}");
        assert_eq!(usage.windows.len(), 3);
        assert_eq!(usage.windows[0].remaining_percent, 28);
        assert_eq!(usage.windows[1].remaining_percent, 61);
        assert_eq!(usage.windows[2].label, "Fable · 週間枠");
        assert_eq!(usage.windows[2].remaining_percent, 66);
        assert_eq!(usage.windows[2].resets_at, Some(2000518400));

        assert_eq!(store.snapshot().catalog.account.accounts.as_ref().unwrap().selected.get(&agent_protocol::session::ProviderKind::Claude).map(String::as_str), Some("claude:desktop"));
        store.dispatch(Intent::NewChat { cwd: root.to_string_lossy().into() }).await.unwrap();
        let key = store.snapshot().pane.navigation.draft_key.clone();
        store.dispatch(Intent::SelectModel { thread_id: key, model: agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Claude, id: "default".into() } }).await.unwrap();
        let thread = send(&store, "first account", "account-first").await;
        completed(&store, &thread, 1, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { provider: agent_protocol::session::ProviderKind::Claude })).await.unwrap();
        let login = store.snapshot().catalog.account.login.clone().unwrap();
        assert!(login.requires_code_submission);
        assert!(login.verification_url.starts_with("https://claude.com/"));
        assert!(store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { provider: agent_protocol::session::ProviderKind::Claude, id: "claude:wrong".into(), code: "fixture-code".into() })).await.is_err());
        store.dispatch(Intent::SubmitAccountLogin(op::SubmitAccountLogin { provider: agent_protocol::session::ProviderKind::Claude, id: login.login_id.clone(), code: "fixture-code".into() })).await.unwrap();
        loop {
            store.dispatch(Intent::ReadAccountLogin(op::ReadAccountLogin { provider: agent_protocol::session::ProviderKind::Claude, id: login.login_id.clone(), thread_id: None })).await.unwrap();
            if store.snapshot().catalog.account.login.is_none() { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        until(&store, |snapshot| snapshot.catalog.account.accounts.as_ref().is_some_and(|accounts| accounts.selected.get(&agent_protocol::session::ProviderKind::Claude) == Some(&login.login_id))).await;
        send(&store, "second account, same history", "account-second").await;
        completed(&store, &thread, 2, "completed").await;
        let profile = root.join("claude").join("accounts").join(login.login_id.strip_prefix("claude:").unwrap());
        assert!(!profile.join("projects").exists(), "credential helpers do not own conversation history");
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, snapshot).await;
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert_eq!(store.snapshot().catalog.account.accounts.as_ref().unwrap().selected.get(&agent_protocol::session::ProviderKind::Claude), Some(&login.login_id));
        send(&store, "resumed after restart", "account-third").await;
        completed(&store, &thread, 3, "completed").await;

        store.dispatch(Intent::StartAccountLogin(op::StartAccountLogin { provider: agent_protocol::session::ProviderKind::Claude })).await.unwrap();
        let canceled = store.snapshot().catalog.account.login.clone().unwrap();
        store.dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin { provider: agent_protocol::session::ProviderKind::Claude, id: canceled.login_id.clone() })).await.unwrap();
        assert!(!root.join("claude").join("accounts").join(canceled.login_id.strip_prefix("claude:").unwrap()).exists());
        assert!(native.join("projects").exists(), "cancel must preserve shared history");
        store.dispatch(Intent::LogoutAccount(op::LogoutAccount { provider: agent_protocol::session::ProviderKind::Claude, id: login.login_id.clone() })).await.unwrap();
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(!store.snapshot().catalog.account.accounts.as_ref().unwrap().selected.contains_key(&agent_protocol::session::ProviderKind::Claude));
        let snapshot = (*store.snapshot()).clone();
        drop(store); endpoint.close().await; fixture.close().await.unwrap();
        let fixture = start().await.unwrap();
        let (store, endpoint) = connect(&fixture, snapshot).await;
        let logged_out = store.snapshot();
        assert!(logged_out.catalog.models.is_empty(), "logged-out Claude and unavailable Codex must not expose models");
        assert!(logged_out.catalog.model_errors.contains_key("codex"), "the catalog must retain the unavailable provider's error");
        assert!(!logged_out.connection_setup().can_start, "a catalog without usable models cannot finish setup");
        store.dispatch(Intent::ListAccounts(op::ListAccounts {})).await.unwrap();
        assert!(!store.snapshot().catalog.account.accounts.as_ref().unwrap().selected.contains_key(&agent_protocol::session::ProviderKind::Claude), "restart must preserve logout without selecting the native account");
        store.dispatch(Intent::SelectAccount(op::SelectAccount { provider: agent_protocol::session::ProviderKind::Claude, id: "claude:desktop".into() })).await.unwrap();
        until(&store, |snapshot| snapshot.catalog.models.iter().any(|model| model.model == agent_protocol::models::ModelRef {provider:ProviderKind::Claude,id:"default".into()})).await;
        send(&store, "back to native account", "account-fourth").await;
        completed(&store, &thread, 4, "completed").await;
        let homes = std::fs::read_to_string(root.join("claude-auth-homes.jsonl")).unwrap();
        let homes: Vec<Value> = homes.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).collect();
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
async fn claude_cancel_recovers_from_logout_failure_only_when_logged_out() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let root = tempfile::tempdir().unwrap();
        let fixture = host(root.path(), Arc::new(Memory::default()), fixture_program()).await;
        let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
        for (status, logged_out) in [
            (json!({"tokenSource":"none","apiKeySource":"none"}), true),
            (json!({"tokenSource":"oauth","apiKeySource":"none"}), false),
            (json!({}), false),
            (json!(null), false),
        ] {
            store
                .dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
                    provider: ProviderKind::Claude,
                }))
                .await
                .unwrap();
            let login = store.snapshot().catalog.account.login.clone().unwrap();
            let profile = root
                .path()
                .join("claude/accounts")
                .join(login.login_id.strip_prefix("claude:").unwrap());
            std::fs::write(profile.join("fail-auth-logout"), "").unwrap();
            std::fs::write(profile.join("fixture-auth.json"), status.to_string()).unwrap();
            let cancel = || {
                Intent::CancelAccountLogin(op::CancelAccountLogin {
                    provider: ProviderKind::Claude,
                    id: login.login_id.clone(),
                })
            };
            let result = store.dispatch(cancel()).await;
            if logged_out {
                result.unwrap();
            } else {
                assert!(result.is_err(), "unconfirmed cleanup must fail");
                assert!(
                    profile.exists(),
                    "retain the profile until credentials are removed"
                );
                std::fs::remove_file(profile.join("fail-auth-logout")).unwrap();
                store.dispatch(cancel()).await.unwrap();
            }
            assert!(
                !profile.exists(),
                "successful cancellation removes the profile, including after retry"
            );
            store.dispatch(cancel()).await.unwrap();
        }
        drop(store);
        endpoint.close().await;
        fixture.close().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AGENT_LIVE_CLAUDE_PROGRAM; starts and cancels an isolated login, never performs inference"]
async fn live_claude_account_login_url_and_cancellation() {
    let program =
        std::env::var_os("AGENT_LIVE_CLAUDE_PROGRAM").expect("set AGENT_LIVE_CLAUDE_PROGRAM");
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
    let store = Store::connect(
        &endpoint,
        &fixture.ticket,
        Snapshot::default(),
        None,
        "Fixture device",
    )
    .await
    .unwrap();
    store
        .dispatch(Intent::StartAccountLogin(op::StartAccountLogin {
            provider: agent_protocol::session::ProviderKind::Claude,
        }))
        .await
        .unwrap();
    let login = store.snapshot().catalog.account.login.clone().unwrap();
    assert!(login.requires_code_submission);
    assert!(login.verification_url.starts_with("https://claude.com/"));
    store
        .dispatch(Intent::CancelAccountLogin(op::CancelAccountLogin {
            provider: agent_protocol::session::ProviderKind::Claude,
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
                thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                model: models::ModelRef {
                    provider: ProviderKind::Claude,
                    id: "default".into(),
                },
            })
            .await
            .unwrap();
        store
            .dispatch(Intent::SelectEffort {
                thread_id: store.snapshot().pane.navigation.draft_key.clone(),
                effort: "low".into(),
            })
            .await
            .unwrap();
        let id = send(&store, prompt, "background-input").await;
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store.disconnect().await.unwrap();
        store
            .reconnect(&endpoint, &fixture.ticket, None, "Fixture device")
            .await
            .unwrap();
        until(&store, |snapshot| snapshot.requests().next().is_some()).await;
        store
            .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
            .await
            .unwrap();
        let snapshot = store.snapshot();
        let thread = &snapshot.sessions.conversations[&id];
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
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
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
                snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0]
                    .items.as_ref().unwrap().iter().any(|item|
                        item.status == agent_protocol::execution::ItemStatus::Completed
                        && item.is_deferred()
                        && matches!(item.body(), agent_protocol::items::ItemBody::CommandExecution {output, exit_code:None, ..} if output == "background done"))
            }).await;
            let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0];
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
            assert!(snapshot.pane.error.is_none(), "{:?}", snapshot.pane.error);
            assert!(snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap().iter().any(|item|
                item.status == agent_protocol::execution::ItemStatus::Completed
                && !item.is_deferred()
                && matches!(item.body(), agent_protocol::items::ItemBody::CommandExecution {output, exit_code:None, ..} if output == "background stdout\n")
            ), "expanded background work must recover native output while its turn is still running");
            std::fs::remove_file(root.path().join("background-paused")).unwrap();
        }
        let snapshot = completed(&store, &id, 1, "completed").await;
        let thread = &snapshot.sessions.conversations[&id];
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
            &snapshot.pane.navigation.draft_key,
            &snapshot.sessions.pending_submissions,
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
                turn_id: snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2]
                    .id
                    .clone(),
            }))
            .await
            .unwrap();
        let snapshot = completed(&store, &id, 3, "interrupted").await;
        assert!(snapshot.requests().next().is_none());
        let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[2];
        assert!(turn.error.is_none(), "{:?}", turn.error);
        // A settings change replaces the retained CLI. The same native
        // conversation must resume after its background work was stopped.
        store
            .dispatch(Intent::SelectEffort {
                thread_id: snapshot.pane.navigation.draft_key.clone(),
                effort: "high".into(),
            })
            .await
            .unwrap();
        send(&store, "after stop", "after-interruption").await;
        let snapshot = completed(&store, &id, 4, "completed").await;
        assert!(
            snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[3]
                .items
                .as_ref()
                .unwrap()
                .iter()
                .any(|item| item_text(item) == Some("reply 4: after stop"))
        );
        let inputs = session_inputs(session_trace(root.path()));
        assert_eq!(inputs[3]["effort"], "high");
        assert_ne!(inputs[2]["process"], inputs[3]["process"]);
        store.close().await.unwrap();
        endpoint.close().await;
        fixture.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_responses_show_their_time_and_fork_from_the_selected_turn() {
    use agent_core::presentation::conversation::{ConversationRowContent, project_conversation};

    let root = tempfile::tempdir().unwrap();
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
            thread_id: store.snapshot().pane.navigation.draft_key.clone(),
            model: models::ModelRef {
                provider: ProviderKind::Claude,
                id: "default".into(),
            },
        })
        .await
        .unwrap();
    let id = send(&store, "first", "fork-first").await;
    completed(&store, &id, 1, "completed").await;
    send(&store, "second", "fork-second").await;
    let snapshot = completed(&store, &id, 2, "completed").await;
    let thread = snapshot.sessions.conversations[&id].clone();
    let projected = project_conversation(
        &snapshot.pane.navigation.draft_key,
        &snapshot.sessions.pending_submissions,
        thread.clone(),
        &None,
    );
    for turn in &projected.turns {
        assert!(
            turn.rows.iter().any(|row| matches!(
                &row.content,
                ConversationRowContent::Response { fork_turn_id: Some(_), .. }
                    if row.timestamp_ms.is_some()
            )),
            "a completed Claude answer must show its time and offer a fork like Codex"
        );
    }
    let first = thread.turns.as_ref().unwrap()[0].id.clone();
    store
        .dispatch(Intent::ForkSession(op::ForkSession::new(id.clone(), first)))
        .await
        .unwrap();
    let snapshot = until(&store, |snapshot| {
        snapshot
            .sessions
            .conversations
            .iter()
            .any(|(fork, thread)| {
                fork != &id
                    && fork.provider == ProviderKind::Claude
                    && thread.turns.as_ref().is_some_and(|turns| turns.len() == 1)
            })
    })
    .await;
    let (_, forked) = snapshot
        .sessions
        .conversations
        .iter()
        .find(|(fork, _)| *fork != &id)
        .unwrap();
    let items = forked.turns.as_ref().unwrap()[0].items.as_ref().unwrap();
    assert!(items.iter().any(|item| matches!(item.body(),
        agent_protocol::items::ItemBody::UserMessage { content, .. }
            if content == &[agent_protocol::items::MessagePart::Text { text: "first".into() }])));
    assert!(
        items
            .iter()
            .any(|item| item_text(item) == Some("reply 1: first"))
    );
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_keeps_loading_until_it_answers_a_background_shell_and_interrupt_stops_it() {
    let root = tempfile::tempdir().unwrap();
    let paused = root.path().join("background-shell-paused");
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
            thread_id: store.snapshot().pane.navigation.draft_key.clone(),
            model: models::ModelRef {
                provider: ProviderKind::Claude,
                id: "default".into(),
            },
        })
        .await
        .unwrap();
    let says = |turn: usize, text: &'static str| {
        move |snapshot: &Snapshot| {
            snapshot.sessions.conversations.values().any(|thread| {
                thread
                    .turns
                    .as_ref()
                    .and_then(|turns| turns.get(turn)?.items.as_ref())
                    .is_some_and(|items| items.iter().any(|item| item_text(item) == Some(text)))
            })
        }
    };
    std::fs::write(&paused, "").unwrap();
    let id = send(&store, "background-shell", "background-shell-answered").await;
    until(&store, says(0, "あとで結果を報告します")).await;
    std::fs::remove_file(&paused).unwrap();
    let snapshot = until(&store, says(0, "結果を報告します")).await;
    assert_eq!(
        snapshot.sessions.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .len(),
        1,
        "Claude's answer to its finished shell must stay in the requesting turn"
    );
    completed(&store, &id, 1, "completed").await;

    std::fs::write(&paused, "").unwrap();
    send(&store, "background-shell", "background-shell-interrupted").await;
    let snapshot = until(&store, says(1, "あとで結果を報告します")).await;
    store
        .dispatch(Intent::Interrupt(op::Interrupt {
            thread_id: id.clone(),
            turn_id: snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[1]
                .id
                .clone(),
        }))
        .await
        .unwrap();
    let snapshot = completed(&store, &id, 2, "interrupted").await;
    let turn = &snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[1];
    assert!(turn.error.is_none(), "{:?}", turn.error);
    std::fs::remove_file(&paused).unwrap();
    send(&store, "after interrupt", "after-background-shell").await;
    let snapshot = completed(&store, &id, 3, "completed").await;
    assert!(
        !says(2, "結果を報告します")(&snapshot),
        "an interrupted shell must not answer into a later turn"
    );
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_while_only_a_background_shell_runs_opens_the_turn_history_shows() {
    let root = tempfile::tempdir().unwrap();
    let paused = root.path().join("background-shell-paused");
    let memory = Arc::new(Memory::default());
    let fixture = host(root.path(), memory.clone(), fixture_program()).await;
    let (store, endpoint) = connect(&fixture, Snapshot::default()).await;
    store
        .dispatch(Intent::NewChat {
            cwd: root.path().to_str().unwrap().into(),
        })
        .await
        .unwrap();
    store
        .dispatch(Intent::SelectModel {
            thread_id: store.snapshot().pane.navigation.draft_key.clone(),
            model: models::ModelRef {
                provider: ProviderKind::Claude,
                id: "default".into(),
            },
        })
        .await
        .unwrap();
    let texts = |snapshot: &Snapshot, id: &agent_protocol::session::SessionRef| {
        snapshot.sessions.conversations[id]
            .turns
            .iter()
            .flatten()
            .map(|turn| {
                turn.items
                    .iter()
                    .flatten()
                    .filter_map(|item| match item.body() {
                        agent_protocol::items::ItemBody::AssistantText { text, .. } => {
                            Some(text.clone())
                        }
                        agent_protocol::items::ItemBody::UserMessage { content, .. } => {
                            content.iter().find_map(|part| match part {
                                agent_protocol::items::MessagePart::Text { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    std::fs::write(&paused, "").unwrap();
    let id = send(&store, "background-shell", "background-shell-answered").await;
    until(&store, |snapshot| {
        texts(snapshot, &id)
            .first()
            .is_some_and(|turn| turn.iter().any(|text| text == "あとで結果を報告します"))
    })
    .await;
    send(&store, "next question", "while-background-shell").await;
    let snapshot = until(&store, |snapshot| {
        texts(snapshot, &id)
            .get(1)
            .is_some_and(|turn| turn.iter().any(|text| text == "reply 2: next question"))
    })
    .await;
    let turns = snapshot.sessions.conversations[&id].turns.as_ref().unwrap();
    assert_eq!(turns.len(), 2, "{:?}", texts(&snapshot, &id));
    assert_eq!(turns[0].status.label(), "completed");
    assert_eq!(turns[1].id, "while-background-shell".into());
    std::fs::remove_file(&paused).unwrap();
    let live = completed(&store, &id, 2, "completed").await;
    assert_eq!(
        texts(&live, &id),
        [
            vec![
                "background-shell".to_owned(),
                "あとで結果を報告します".into()
            ],
            vec![
                "next question".into(),
                "reply 2: next question".into(),
                "結果を報告します".into()
            ],
        ],
        "the shell's report belongs to the turn that is current when it arrives"
    );
    let saved: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(live.as_ref()).unwrap()).unwrap();
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
    let fixture = host(root.path(), memory, fixture_program()).await;
    let (store, endpoint) = connect(&fixture, saved).await;
    store
        .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
        .await
        .unwrap();
    assert_eq!(
        texts(&store.snapshot(), &id),
        texts(&live, &id),
        "saved history must show the turns the live conversation showed"
    );
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[rstest::rstest]
#[case::folded(false, 1, 2)]
#[case::after_result(true, 2, 1)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claude_accepts_running_input_and_reads_past_the_previous_result(
    #[case] after_result: bool,
    #[case] turns_per_wait: usize,
    #[case] users_in_waiting_turn: usize,
) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("claude-fixture.json"),
        json!({"inputAfterResult":after_result}).to_string(),
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
    let key = store.snapshot().pane.navigation.draft_key.clone();
    store
        .dispatch(Intent::SelectModel {
            thread_id: key,
            model: agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Claude,
                id: "default".into(),
            },
        })
        .await
        .unwrap();
    let id = send(&store, "wait", "initial").await;
    until(&store, |snapshot| {
        snapshot
            .sessions
            .conversations
            .get(&id)
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.first())
            .and_then(|turn| turn.items.as_ref())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item_text(item) == Some("Waiting for interruption"))
            })
    })
    .await;
    assert!(
        store.snapshot().sessions.conversations[&id]
            .capabilities
            .unwrap()
            .additional_input
    );
    assert!(
        agent_protocol::session::input_unavailable_reason(
            &store.snapshot().sessions.conversations[&id]
        )
        .is_none()
    );
    let local = fixture.local().await.unwrap();
    assert!(
        local
            .peer
            .call(&rpc::Interrupt {
                thread_id: id.clone(),
                turn_id: "stale".into(),
            })
            .await
            .is_err()
    );
    send(&store, "follow-up", "steered").await;
    let snapshot = completed(&store, &id, turns_per_wait, "completed").await;
    let items = snapshot.sessions.conversations[&id]
        .turns
        .as_ref()
        .unwrap()
        .last()
        .unwrap()
        .items
        .as_ref()
        .unwrap();
    assert_eq!(
        items
            .iter()
            .filter(|item| item.client_input_id.as_deref() == Some("steered"))
            .count(),
        1
    );
    assert!(
        items
            .iter()
            .any(|item| item_text(item) == Some("reply 2: follow-up"))
    );
    assert!(snapshot.sessions.pending_submissions.is_empty());
    store
        .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
        .await
        .unwrap();
    let refreshed = store.snapshot();
    let turns = refreshed.sessions.conversations[&id]
        .turns
        .as_ref()
        .unwrap();
    assert_eq!(
        turns.len(),
        turns_per_wait,
        "queued input must retain its live turn in history"
    );
    assert_eq!(
        turns
            .iter()
            .flat_map(|turn| turn.items.iter().flatten())
            .filter(|item| { item_text(item) == Some("reply 2: follow-up") })
            .count(),
        1
    );
    send(&store, "wait", "wait-again").await;
    until(&store, |snapshot| {
        snapshot
            .sessions
            .conversations
            .get(&id)
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.get(turns_per_wait))
            .and_then(|turn| turn.items.as_ref())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item_text(item) == Some("Waiting for interruption"))
            })
    })
    .await;
    let waiting_turn = store.snapshot().sessions.conversations[&id]
        .turns
        .as_ref()
        .unwrap()
        .last()
        .unwrap()
        .id
        .clone();
    let queued = local
        .peer
        .call(&rpc::Submission {
            thread_id: id.clone(),
            client_user_message_id: "queued".into(),
            model: None,
            effort: None,
            service_tier: None,
            input: vec![rpc::Input::Text {
                text: "queued follow-up".into(),
            }],
        })
        .await
        .unwrap();
    let snapshot = completed(&store, &id, 2 * turns_per_wait, "completed").await;
    let items = snapshot.sessions.conversations[&id]
        .turns
        .as_ref()
        .unwrap()
        .last()
        .unwrap()
        .items
        .as_ref()
        .unwrap();
    assert_eq!(queued.turn_id.as_ref(), Some(&waiting_turn));
    assert!(
        items
            .iter()
            .any(|item| item.client_input_id.as_deref() == Some("queued"))
    );
    assert!(
        items
            .iter()
            .any(|item| item_text(item) == Some("reply 4: queued follow-up"))
    );
    send(&store, "after completion", "last").await;
    completed(&store, &id, 2 * turns_per_wait + 1, "completed").await;
    let inputs = session_inputs(session_trace(Path::new(&snapshot.pane.navigation.cwd)));
    let inputs = inputs.as_array().unwrap();
    assert_eq!(inputs.len(), 5);
    assert!(
        inputs
            .iter()
            .all(|input| input["process"] == inputs[0]["process"])
    );
    send(&store, "wait", "before-live-refresh").await;
    until(&store, |snapshot| {
        snapshot.sessions.conversations[&id]
            .turns
            .as_ref()
            .unwrap()
            .get(2 * turns_per_wait + 1)
            .and_then(|turn| turn.items.as_ref())
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item_text(item) == Some("Waiting for interruption"))
            })
    })
    .await;
    send(&store, "permission", "queued-before-refresh").await;
    until(&store, |snapshot| snapshot.requests().next().is_some()).await;
    store
        .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
        .await
        .unwrap();
    let refreshed = store.snapshot();
    let turns = refreshed.sessions.conversations[&id]
        .turns
        .as_ref()
        .unwrap();
    assert_eq!(
        turns.len(),
        3 * turns_per_wait + 1,
        "live overlay must not append a duplicate queued turn"
    );
    let turn = turns.last().unwrap();
    assert_eq!(turn.status, agent_protocol::execution::TurnStatus::Running);
    assert_eq!(
        turn.items
            .as_ref()
            .unwrap()
            .iter()
            .filter(|item| matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            ))
            .count(),
        users_in_waiting_turn
    );
    store
        .dispatch(Intent::Interrupt(op::Interrupt {
            thread_id: id.clone(),
            turn_id: turn.id.clone(),
        }))
        .await
        .unwrap();
    completed(&store, &id, 3 * turns_per_wait + 1, "interrupted").await;
    local.close().await;
    store.close().await.unwrap();
    endpoint.close().await;
    fixture.close().await.unwrap();
}

#[tokio::test]
async fn permission_settings_edit_native_claude_defaults_without_own_storage() {
    use agent_protocol::{permissions::*, session::ProviderKind};
    let root = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let fixture = host(root.path(), memory.clone(), fixture_program()).await;
    let local = fixture.local().await.unwrap();
    let read = ReadPermissionSettings {
        provider: ProviderKind::Claude,
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
                provider: read.provider,
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
            initial_input: None,
            provider: ProviderKind::Claude,
            cwd: Some(root.path().to_string_lossy().into()),
            model: None,
        })
        .await
        .unwrap();
    assert_eq!(
        response.response.model.unwrap(),
        models::ModelRef {
            provider: ProviderKind::Claude,
            id: "default".into()
        }
    );
    assert_eq!(
        response.response.thread.id.unwrap().provider,
        ProviderKind::Claude
    );
    local.close().await;
    fixture.close().await.unwrap();
}
