pub(in crate::host_rpc::service) fn repository(
    directory: &std::path::Path,
) -> std::path::PathBuf {
    let repository = directory.join("repository");
    std::fs::create_dir(&repository).unwrap();
    crate::git::text(&repository, &["init", "--quiet", "--initial-branch=main"]).unwrap();
    crate::git::text(
        &repository,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "fixture",
        ],
    )
    .unwrap();
    repository
}

#[tokio::test]
async fn worktree_resolves_public_tasks_and_preserves_native_idle_warnings() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
    for warning in [None, Some("native idle warning")] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(directory.path());
        let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        Arc::get_mut(&mut agent).unwrap().idle_warning = warning;
        let host =
            HostRpcService::new([agent.backend()], directory.path().join("worktrees.json"));
        host.inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let checkout = host
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                String::new(),
                true,
                crate::worktrees::Observer::default(),
            )
            .await
            .unwrap();
        let response = agent
            .create(checkout.to_str().unwrap(), None, None)
            .await
            .unwrap();
        let public = agent_protocol::session::SessionRef::new(
            ProviderKind::Codex,
            "bex-task:managed".into(),
        )
        .unwrap();
        own(&host, &public, &response);
        let entries = host.worktree_list().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].threads.len(), 1);
        assert_eq!(entries[0].threads[0].id, public);
        assert!(!entries[0].threads[0].active);
        assert_eq!(entries[0].blocked_reason.is_some(), warning.is_some());
        if let Some(warning) = warning {
            assert!(
                entries[0]
                    .blocked_reason
                    .as_ref()
                    .unwrap()
                    .contains(warning)
            );
            assert!(host.remove_worktree(entries[0].path.clone()).await.is_err());
        } else {
            host.remove_worktree(entries[0].path.clone()).await.unwrap();
            assert!(!checkout.exists());
            assert!(
                host.inner
                    .conversation
                    .storage
                    .lookup(&public)
                    .unwrap()
                    .is_some()
            );
        }
    }
}

#[tokio::test]
async fn worktree_occupancy_uses_the_imported_status_when_source_history_is_incomplete() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
    use agent_protocol::{
        execution::TurnStatus,
        session::{HistoryReadKind, HistoryReadState, SessionRef},
    };
    for history in [HistoryReadKind::Incomplete, HistoryReadKind::Unavailable] {
        for status in [TurnStatus::Completed, TurnStatus::Running] {
            let directory = tempfile::tempdir().unwrap();
            let repository = repository(directory.path());
            let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
            let host =
                HostRpcService::new([agent.backend()], directory.path().join("worktrees.json"));
            host.inner
                .worktrees
                .settings(Some(agent_protocol::models::WorktreeSettings {
                    create_on_new_session: true,
                    ..Default::default()
                }))
                .await
                .unwrap();
            let checkout = host
                .inner
                .worktrees
                .prepare_selected(
                    repository.to_str().unwrap(),
                    String::new(),
                    true,
                    crate::worktrees::Observer::default(),
                )
                .await
                .unwrap();
            let mut response = agent
                .create(checkout.to_str().unwrap(), None, None)
                .await
                .unwrap();
            response.thread.history_read_state =
                Some(HistoryReadState::new(history.clone(), vec![]));
            response.thread.turns = Some(vec![Arc::new(agent_protocol::models::Turn {
                id: "turn".into(),
                status,
                ..Default::default()
            })]);
            let native = response.thread.id.clone().unwrap();
            agent
                .records
                .lock()
                .unwrap()
                .insert(native.id.clone(), response.clone());
            let public =
                SessionRef::new(ProviderKind::Claude, "bex-task:managed".into()).unwrap();
            own(&host, &public, &response);
            let entries = host.worktree_list().await.unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].threads.len(), 1);
            assert_eq!(entries[0].threads[0].id, public);
            assert_eq!(entries[0].threads[0].active, status == TurnStatus::Running);
            assert_eq!(
                entries[0].blocked_reason.is_some(),
                status == TurnStatus::Running
            );
            assert_eq!(
                host.remove_worktree(entries[0].path.clone()).await.is_err(),
                status == TurnStatus::Running
            );
            assert_eq!(checkout.exists(), status == TurnStatus::Running);
        }
    }
}

#[tokio::test]
async fn worktree_keeps_idle_uncertainty_and_provider_failures_alongside_running_work() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(directory.path());
    let mut agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    Arc::get_mut(&mut agent).unwrap().idle_warning = Some("native idle warning");
    let host = HostRpcService::new(
        [
            agent.backend(),
            Backend::unavailable(ProviderKind::Claude, "claude startup failed"),
        ],
        directory.path().join("worktrees.json"),
    );
    host.inner
        .worktrees
        .settings(Some(agent_protocol::models::WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let checkout = host
        .inner
        .worktrees
        .prepare_selected(
            repository.to_str().unwrap(),
            String::new(),
            true,
            crate::worktrees::Observer::default(),
        )
        .await
        .unwrap();
    agent
        .create(checkout.to_str().unwrap(), None, None)
        .await
        .unwrap();
    let running = agent
        .create(checkout.to_str().unwrap(), None, None)
        .await
        .unwrap()
        .thread
        .id
        .unwrap();
    agent
        .records
        .lock()
        .unwrap()
        .get_mut(&running.id)
        .unwrap()
        .thread
        .status = agent_protocol::models::SessionStatus::Running;
    let client = host.open_session();
    let worktrees: Vec<agent_protocol::models::Worktree> = call(
        &host,
        client.id(),
        Call::ListWorktrees(agent_protocol::models::Empty {}),
    )
    .await
    .unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].threads.len(), 2);
    assert!(worktrees[0].threads.iter().any(|thread| thread.active));
    assert!(worktrees[0].threads.iter().any(|thread| !thread.active));
    assert_eq!(
        worktrees[0].blocked_reason.as_deref(),
        Some(
            "このワークツリーで作業を実行中です。完了または停止してから削除してください。\n稼働状況を確認できないため削除できません: claude startup failed\n稼働状況を確認できないため削除できません: native idle warning"
        )
    );
}

#[tokio::test]
async fn worktree_block_reasons_keep_every_provider_failure_in_provider_order() {
    for listing_fails in [false, true] {
        use super::*;
        use crate::host_rpc::runtime::native_fixture::NativeAgent;
        use std::sync::atomic::Ordering;
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(directory.path());
        for order in [
            [
                ProviderKind::DeepSeek,
                ProviderKind::Claude,
                ProviderKind::Codex,
            ],
            [
                ProviderKind::Claude,
                ProviderKind::Codex,
                ProviderKind::DeepSeek,
            ],
            [
                ProviderKind::Codex,
                ProviderKind::DeepSeek,
                ProviderKind::Claude,
            ],
        ] {
            let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
            let host = HostRpcService::new(
                order.map(|provider| {
                    if listing_fails && provider == ProviderKind::Codex {
                        a.backend()
                    } else {
                        Backend::unavailable(
                            provider,
                            format!("{} startup failed", provider.key()),
                        )
                    }
                }),
                directory.path().join("worktrees.json"),
            );
            host.inner
                .worktrees
                .settings(Some(agent_protocol::models::WorktreeSettings {
                    create_on_new_session: true,
                    ..Default::default()
                }))
                .await
                .unwrap();
            host.inner
                .worktrees
                .prepare_selected(
                    repository.to_str().unwrap(),
                    String::new(),
                    true,
                    crate::worktrees::Observer::default(),
                )
                .await
                .unwrap();
            let expected = if listing_fails {
                "稼働状況を確認できないため削除できません: codex list failed\nclaude startup failed\ndeepseek startup failed"
            } else {
                "稼働状況を確認できないため削除できません: codex startup failed\nclaude startup failed\ndeepseek startup failed"
            };

            a.fail_list.store(1, Ordering::SeqCst);
            let entries = host.worktree_list().await.unwrap();
            assert!(!entries.is_empty());
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.blocked_reason.as_deref() == Some(expected))
            );
        }
    }
}

#[test]
fn blockers_keep_all_occupancy_and_uncertainty_reasons_in_a_fixed_order() {
    assert_eq!(super::WorktreeBlockers::default().reason(), None);
    for warnings in [
        ["native idle warning", "codex startup failed"],
        ["codex startup failed", "native idle warning"],
    ] {
        let blockers = super::WorktreeBlockers {
            running: true,
            terminal: true,
            warnings: warnings.into_iter().map(str::to_owned).collect(),
        };
        assert_eq!(
            blockers.reason().as_deref(),
            Some(
                "このワークツリーで作業を実行中です。完了または停止してから削除してください。\nこのワークツリーのターミナルを閉じてから削除してください。\n稼働状況を確認できないため削除できません: codex startup failed\n稼働状況を確認できないため削除できません: native idle warning"
            )
        );
    }
}

#[tokio::test]
async fn worktree_listing_blocks_only_checkouts_with_unverified_claude_activity() {
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    crate::git::text(&repository, &["init", "--quiet", "--initial-branch=main"]).unwrap();
    crate::git::text(
        &repository,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "fixture",
        ],
    )
    .unwrap();
    let native = root.path().join("native");
    let project = native.join("projects/example");
    std::fs::create_dir_all(&project).unwrap();
    let claude = crate::providers::Claude::load(
        root.path().join("unused-cli"),
        root.path().join("state"),
        Some(native),
    )
    .await
    .unwrap();
    let service = HostRpcService::new([claude.into()], root.path().join("projects.json"));
    service
        .inner
        .worktrees
        .settings(Some(agent_protocol::models::WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let first = service
        .inner
        .worktrees
        .prepare_selected(
            repository.to_str().unwrap(),
            String::new(),
            true,
            crate::worktrees::Observer::default(),
        )
        .await
        .unwrap();
    let other = service
        .inner
        .worktrees
        .prepare_selected(
            repository.to_str().unwrap(),
            String::new(),
            true,
            crate::worktrees::Observer::default(),
        )
        .await
        .unwrap();
    let id = "12345678-1234-4234-8234-123456789abc";
    let transcript = project.join(format!("{id}.jsonl"));
    let history = include_str!("../../../../tests/fixtures/claude-2.1.266.jsonl")
        .lines()
        .map(|line| {
            let mut entry: serde_json::Value = serde_json::from_str(line).unwrap();
            entry["cwd"] = serde_json::json!(first);
            // Another agent's `claude -p` run stays out of task lists, but
            // its unverifiable activity must still block removal.
            entry["entrypoint"] = serde_json::json!("sdk-cli");
            entry.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&transcript, history).unwrap();
    let entries = service.worktree_list().await.unwrap();
    let first_entry = entries
        .iter()
        .find(|entry| std::path::Path::new(&entry.path) == first)
        .unwrap();
    assert_eq!(first_entry.threads[0].id.id, id);
    assert!(!first_entry.threads[0].active);
    assert!(
        first_entry
            .blocked_reason
            .as_ref()
            .unwrap()
            .contains("native transcript does not expose external process activity")
    );
    let other_entry = entries
        .iter()
        .find(|entry| std::path::Path::new(&entry.path) == other)
        .unwrap();
    assert!(other_entry.blocked_reason.is_none());
    assert!(
        service
            .remove_worktree(first_entry.path.clone())
            .await
            .is_err()
    );
    assert!(first.is_dir());
    std::fs::remove_file(transcript).unwrap();
    assert!(
        service
            .worktree_list()
            .await
            .unwrap()
            .iter()
            .all(|entry| entry.blocked_reason.is_none())
    );
}

#[tokio::test]
async fn failed_provider_preparation_keeps_the_owned_checkout_for_retry() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
    use agent_protocol::conversation::{InputContent, InputState, records};
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(directory.path());
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    agent.create_gate.close();
    let host = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    host.inner
        .worktrees
        .settings(Some(agent_protocol::models::WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        }))
        .await
        .unwrap();
    let client = host.open_session();
    let target =
        agent_protocol::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string())
            .unwrap();
    let input = uuid::Uuid::new_v4();
    let _: records::Opened = call(
        &host,
        client.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: target.clone(),
            subscription: uuid::Uuid::new_v4(),
            cwd: Some(repository.to_string_lossy().into_owned()),
            selection: Default::default(),
            initial: Some((
                input.to_string().into(),
                InputContent {
                    parts: vec![op::Input::Text {
                        text: "keep this input".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            )),
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if host
                .inner
                .conversation
                .storage
                .state(&target)
                .unwrap()
                .inputs
                .values()
                .any(|input| matches!(input.state, InputState::Failed { .. }))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let saved = host.inner.conversation.storage.header(&target).unwrap();
    let checkout = saved.state.workspace.cwd().unwrap();
    assert_ne!(checkout, repository.to_str().unwrap());
    assert!(std::path::Path::new(checkout).is_dir());
    let entries = host.worktree_list().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].threads[0].id, target);
    assert!(!entries[0].threads[0].active);
    assert!(entries[0].blocked_reason.is_none());
}

#[tokio::test]
async fn unchanged_cleanup_follows_each_projects_override() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
    use agent_protocol::conversation::{Context, Event, Observation};
    use agent_protocol::session::SessionRef;
    use uuid::Uuid;
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(directory.path());
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let host = HostRpcService::new([agent.backend()], directory.path().join("worktrees.json"));
    host.inner.projects.register(&repository).await.unwrap();
    let project = host.project_snapshot().await.unwrap().projects[0]
        .id
        .clone();
    let mut settings = agent_protocol::models::WorktreeSettings {
        create_on_new_session: true,
        delete_unchanged: true,
        project_delete_unchanged: [(project.clone(), false)].into(),
        ..Default::default()
    };
    host.update_worktree_settings(settings.clone())
        .await
        .unwrap();
    let kept = host
        .inner
        .worktrees
        .prepare_selected(
            repository.to_str().unwrap(),
            String::new(),
            true,
            crate::worktrees::Observer::default(),
        )
        .await
        .unwrap();
    host.cleanup_unchanged_worktrees().await.unwrap();
    assert!(
        kept.is_dir(),
        "the project's override disables the global rule"
    );
    settings.delete_unchanged = false;
    settings.project_delete_unchanged = [(project, true)].into();
    host.update_worktree_settings(settings).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.worktree_cleanup_requested(),
    )
    .await
    .expect("changed cleanup rules request a sweep");
    let response = agent
        .create(kept.to_str().unwrap(), None, None)
        .await
        .unwrap();
    let source = response.thread.id.clone().unwrap();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    own(&host, &target, &response);
    let storage = &host.inner.conversation.storage;
    let context = Context {
        preparation: None,
        rewind_binding: None,
        imported_turns: &[],
        external_busy: false,
        now: 100,
        seed: Uuid::new_v4(),
        steers: true,
        steer_ready: true,
    };
    let attempt = Uuid::new_v4();
    storage
        .decide(
            &target,
            Event::ObservedStart {
                execution: attempt,
                provider: ProviderKind::Codex,
                source: source.id,
            },
            context,
            None,
        )
        .unwrap();
    for observation in [
        Observation::Background {
            tasks: vec![agent_protocol::execution::BackgroundTask {
                id: "background-command".into(),
                kind: agent_protocol::execution::BackgroundTaskKind::Command,
                description: None,
            }],
        },
        Observation::Request {
            id: "permission".into(),
            body: agent_protocol::requests::RequestBody::Permission {
                description: "background command".into(),
                details: String::new(),
                choices: vec![],
            },
        },
    ] {
        storage
            .decide(
                &target,
                Event::Provider {
                    attempt,
                    native_turn: None,
                    native_items: vec![],
                    observation,
                },
                context,
                None,
            )
            .unwrap();
    }
    let header = storage.header(&target).unwrap();
    assert_eq!(
        header.details.activity.status,
        agent_protocol::models::SessionStatus::Idle
    );
    assert!(header.details.activity.waiting);
    assert!(matches!(
        storage
            .state(&target)
            .unwrap()
            .turns
            .values()
            .last()
            .unwrap()
            .state,
        agent_protocol::conversation::TurnState::Waiting(
            agent_protocol::conversation::Wait::Background { .. }
        )
    ));
    let listed = host.worktree_list().await.unwrap();
    assert!(
        listed[0]
            .threads
            .iter()
            .any(|thread| thread.id == target && thread.active)
    );
    assert!(listed[0].blocked_reason.is_some());
    assert!(
        host.remove_worktree(kept.to_string_lossy().into_owned())
            .await
            .is_err()
    );
    host.cleanup_unchanged_worktrees().await.unwrap();
    assert!(kept.is_dir(), "a background permission keeps its checkout");
    storage
        .decide(
            &target,
            Event::Provider {
                attempt,
                native_turn: None,
                native_items: vec![],
                observation: Observation::Completed,
            },
            context,
            None,
        )
        .unwrap();
    host.cleanup_unchanged_worktrees().await.unwrap();
    assert!(!kept.exists(), "the project's override enables cleanup");
}
