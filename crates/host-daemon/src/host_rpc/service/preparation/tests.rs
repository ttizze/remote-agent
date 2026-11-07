use super::*;
use crate::host_rpc::{
    routing::HostSubscription,
    service::conversations::tests::{NativeAgent, call},
};
use agent_protocol::{
    models::{Empty, ListQuery, SessionStatus, ThreadList},
    session::{OpenSession, OpenedSession},
};
use std::{path::Path, sync::atomic::Ordering, time::Duration};

fn request(cwd: &Path) -> Call {
    Call::CreateSession(op::CreateSession {
        provider: ProviderKind::Codex,
        cwd: Some(
            dunce::canonicalize(cwd)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        ),
        model: Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Codex,
            id: "selected".into(),
        }),
        initial_input: Some(op::InitialInput {
            client_user_message_id: "first-input".into(),
            input: vec![
                op::Input::Text {
                    text: "Build the requested feature".into(),
                },
                op::Input::LocalImage {
                    path: "/attachment.png".into(),
                },
            ],
            effort: Some("high".into()),
            service_tier: Some("fast".into()),
        }),
    })
}

async fn admit(
    host: &HostRpcService,
    client: SessionId,
    request: &Call,
) -> (OpenedSession, HostSubscription) {
    let reply = tokio::time::timeout(Duration::from_secs(5), host.dispatch(client, request))
        .await
        .expect("admission must not await preparation")
        .unwrap();
    let Response::Success { result } = agent_protocol::protocol::decode(&reply.initial).unwrap()
    else {
        panic!("admission failed")
    };
    (result, reply.updates.unwrap())
}

async fn until(
    updates: &mut HostSubscription,
    predicate: impl Fn(&SessionChange) -> bool,
) -> SessionChange {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let change: SessionChange =
                agent_protocol::protocol::decode(&updates.recv().await.unwrap()).unwrap();
            if predicate(&change) {
                return change;
            }
        }
    })
    .await
    .expect("preparation transition must arrive")
}

#[tokio::test]
async fn preparation_publishes_worktree_setup_only_when_it_creates_a_checkout() {
    for (selected, automatic) in [(true, true), (true, false), (false, true)] {
        let directory = tempfile::tempdir().unwrap();
        let repository = crate::host_rpc::service::worktrees::tests::repository(directory.path());
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
        let blocked = agent.create_gate.acquire().await.unwrap();
        let host = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        host.inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: automatic,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut input = request(&repository);
        if !selected {
            let Call::CreateSession(params) = &mut input else {
                unreachable!()
            };
            params.cwd = None;
        }
        let client = host.open_session();
        let (opened, mut updates) = admit(&host, client.id(), &input).await;
        assert_eq!(
            opened.response.thread.preparation.unwrap().stage,
            PreparationStage::Workspace
        );
        let mut stages = Vec::new();
        loop {
            let SessionChange::Preparation {
                preparation: Some(preparation),
            } = until(&mut updates, |change| {
                matches!(
                    change,
                    SessionChange::Preparation {
                        preparation: Some(_)
                    }
                )
            })
            .await
            else {
                unreachable!()
            };
            assert_eq!(preparation.status, PreparationStatus::Preparing);
            stages.push(preparation.stage);
            if preparation.stage == PreparationStage::Provider {
                break;
            }
        }
        assert_eq!(
            stages.contains(&PreparationStage::Worktree),
            selected && automatic
        );
        let other = host.open_session();
        let reopened: OpenedSession = call(
            &host,
            other.id(),
            Call::OpenSession(OpenSession {
                session: opened.session.clone(),
                limit: 5,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            reopened.response.thread.preparation.unwrap().stage,
            PreparationStage::Provider
        );
        if selected {
            assert_eq!(
                dunce::canonicalize(reopened.response.thread.cwd.as_deref().unwrap()).unwrap()
                    != dunce::canonicalize(&repository).unwrap(),
                automatic,
                "automatic={automatic}: the selected workspace is {repository:?}, the task uses {:?}",
                reopened.response.thread.cwd,
            );
        }
        drop(blocked);
        until(&mut updates, |change| {
            matches!(change, SessionChange::Preparation { preparation: None })
        })
        .await;
    }
}

#[tokio::test]
async fn admission_lists_and_reopens_the_same_task_before_workspace_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let host = HostRpcService::new(
        [agent.backend()],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    host.inner
        .projects
        .register(directory.path())
        .await
        .unwrap();
    let project = host.inner.projects.load().await.unwrap().projects[0]
        .id
        .clone();
    let blocked = host.inner.worktree_access.write().await;
    let client = host.open_session();
    let other = host.open_session();
    let input = request(directory.path());
    let (opened, _) = admit(&host, client.id(), &input).await;
    let root = opened.session.clone();
    assert_eq!(opened.response.thread.status, SessionStatus::Preparing);
    assert_eq!(
        opened.response.thread.project_id.as_deref(),
        Some(project.as_str())
    );
    assert_eq!(agent.next.load(Ordering::SeqCst), 0);
    let list: ThreadList = call(
        &host,
        other.id(),
        Call::ListSessions(op::ListSessions {
            query: ListQuery::default(),
        }),
    )
    .await
    .unwrap();
    assert!(
        list.data
            .iter()
            .any(|thread| thread.id.as_ref() == Some(&root)
                && thread.project_id.as_deref() == Some(project.as_str())),
        "admitted task {root:?} must be listed in project {project}: {list:?}"
    );
    let reply = host
        .dispatch(
            other.id(),
            &Call::OpenSession(OpenSession {
                session: root.clone(),
                limit: 5,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: reopened } =
        agent_protocol::protocol::decode::<Response<OpenedSession>>(&reply.initial).unwrap()
    else {
        panic!("reopen failed")
    };
    assert_eq!(
        reopened.response.thread.preparation,
        opened.response.thread.preparation
    );
    let mut updates = reply.updates.unwrap();
    host.close_session(client.id());
    drop(blocked);
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    {
        let sent = agent.inputs.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_ne!(sent[0].thread_id, root);
        assert_eq!(sent[0].effort.as_deref(), Some("high"));
        assert_eq!(sent[0].service_tier.as_deref(), Some("fast"));
        assert_eq!(sent[0].input.len(), 2);
    }
    let history: OpenedSession = call(
        &host,
        other.id(),
        Call::OpenSession(OpenSession {
            session: root.clone(),
            limit: 5,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(history.session, root);
    assert!(history.response.thread.preparation.is_none());
    let turns = history.response.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id.as_str(), "turn-0");
    let detail: op::ItemResponse = call(
        &host,
        other.id(),
        Call::ReadItem(op::ReadItem {
            thread_id: root,
            turn_id: "turn-0".into(),
            item_id: "user-0".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        detail.item.client_input_id.as_ref().unwrap().as_str(),
        "first-input"
    );
}

#[tokio::test]
async fn repeated_admission_does_not_wait_for_or_duplicate_provider_creation() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let blocked = agent.create_gate.acquire().await.unwrap();
    let host = HostRpcService::new(
        [agent.backend()],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let input = request(directory.path());
    let (first, mut updates) = admit(&host, client.id(), &input).await;
    tokio::time::timeout(Duration::from_secs(5), agent.create_started.notified())
        .await
        .unwrap();
    let (replay, _) = admit(&host, client.id(), &input).await;
    assert_eq!(first.session, replay.session);
    drop(blocked);
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    assert_eq!(agent.next.load(Ordering::SeqCst), 1);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn admitted_public_identity_keeps_native_history_through_provider_switching() {
    let directory = tempfile::tempdir().unwrap();
    let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    let host = HostRpcService::new(
        [codex.backend(), claude.backend()],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let (opened, mut updates) = admit(&host, client.id(), &request(directory.path())).await;
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    let _: op::SubmissionReceipt = call(
        &host,
        client.id(),
        Call::Submit(op::Submission {
            thread_id: opened.session.clone(),
            client_user_message_id: "switch".into(),
            input: vec![op::Input::Text {
                text: "Continue with this AI".into(),
            }],
            model: Some(agent_protocol::models::ModelRef {
                provider: ProviderKind::Claude,
                id: "model".into(),
            }),
            effort: None,
            service_tier: None,
        }),
    )
    .await
    .unwrap();
    let history: OpenedSession = call(
        &host,
        client.id(),
        Call::OpenSession(OpenSession {
            session: opened.session.clone(),
            limit: 5,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(history.session, opened.session);
    assert_eq!(
        history.response.thread.active_provider,
        Some(ProviderKind::Claude)
    );
    let turns = history.response.thread.turns.unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].provider, Some(ProviderKind::Codex));
    assert_eq!(turns[1].provider, Some(ProviderKind::Claude));
    assert_eq!(codex.inputs.lock().unwrap().len(), 1);
    assert_eq!(claude.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn retry_restores_a_removed_workspace_before_creating_the_first_provider_session() {
    let directory = tempfile::tempdir().unwrap();
    let repository = crate::host_rpc::service::worktrees::tests::repository(directory.path());
    let state_path = directory.path().join("worktrees.json");
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    agent.create_gate.close();
    let host = HostRpcService::new([agent.backend()], ProjectStore::new(state_path.clone()));
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
        .prepare(repository.to_str(), || async { Ok(()) })
        .await
        .unwrap()
        .unwrap();
    host.inner
        .worktrees
        .settings(Some(agent_protocol::models::WorktreeSettings {
            create_on_new_session: false,
            ..Default::default()
        }))
        .await
        .unwrap();
    let client = host.open_session();
    let (opened, mut updates) = admit(&host, client.id(), &request(&checkout)).await;
    until(&mut updates, |change| matches!(change, SessionChange::Preparation { preparation: Some(preparation) } if matches!(preparation.status, PreparationStatus::Failed { .. }))).await;
    let row = host.inner.database.get(&opened.session).unwrap().unwrap();
    assert!(row.bindings.is_empty());
    assert_eq!(
        row.startup.unwrap().preparation.stage,
        PreparationStage::Provider
    );
    host.remove_worktree(checkout.to_str().unwrap().into())
        .await
        .unwrap();
    assert!(!checkout.exists());
    drop(updates);
    drop(client);
    drop(host);

    let agent = agent.restarted();
    let blocked = agent.create_gate.acquire().await.unwrap();
    let host = HostRpcService::new([agent.backend()], ProjectStore::new(state_path));
    let client = host.open_session();
    let mut updates = host
        .dispatch(
            client.id(),
            &Call::OpenSession(OpenSession {
                session: opened.session.clone(),
                limit: 5,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap()
        .updates
        .unwrap();
    let _: Empty = call(
        &host,
        client.id(),
        Call::RetryPreparation(op::RetryPreparation {
            thread_id: opened.session,
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(30), agent.create_started.notified())
        .await
        .unwrap();
    assert!(
        checkout.is_dir(),
        "restore the saved workspace before provider creation"
    );
    drop(blocked);
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    assert_eq!(agent.next.load(Ordering::SeqCst), 1);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refused_initial_input_retries_in_the_same_native_binding() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    agent.fail_submit.store(1, Ordering::SeqCst);
    let host = HostRpcService::new(
        [agent.backend()],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let (opened, mut updates) = admit(&host, client.id(), &request(directory.path())).await;
    until(&mut updates, |change| matches!(change, SessionChange::Preparation { preparation: Some(preparation) } if matches!(preparation.status, PreparationStatus::Failed { .. }))).await;
    assert!(agent.inputs.lock().unwrap().is_empty());
    let _: Empty = call(
        &host,
        client.id(),
        Call::RetryPreparation(op::RetryPreparation {
            thread_id: opened.session.clone(),
        }),
    )
    .await
    .unwrap();
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    assert_eq!(agent.next.load(Ordering::SeqCst), 1);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_eq!(
        agent.inputs.lock().unwrap()[0]
            .client_user_message_id
            .as_str(),
        "first-input"
    );
}

#[tokio::test]
async fn stopping_during_provider_creation_keeps_the_binding_without_sending() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
    let blocked = agent.create_gate.acquire().await.unwrap();
    let host = HostRpcService::new(
        [agent.backend()],
        ProjectStore::new(directory.path().join("worktrees.json")),
    );
    let client = host.open_session();
    let (opened, mut updates) = admit(&host, client.id(), &request(directory.path())).await;
    tokio::time::timeout(Duration::from_secs(5), agent.create_started.notified())
        .await
        .unwrap();
    let _: Empty = call(
        &host,
        client.id(),
        Call::Interrupt(op::Interrupt {
            thread_id: opened.session.clone(),
            turn_id: "first-input".into(),
        }),
    )
    .await
    .unwrap();
    drop(blocked);
    let finished = host
        .inner
        .router
        .submission_lock(&opened.session)
        .lock_owned()
        .await;
    let row = host.inner.database.get(&opened.session).unwrap().unwrap();
    assert_eq!(
        row.startup.unwrap().preparation.status,
        PreparationStatus::Interrupted
    );
    assert_eq!(row.bindings.len(), 1);
    assert!(agent.inputs.lock().unwrap().is_empty());
    drop(finished);
    let _: Empty = call(
        &host,
        client.id(),
        Call::RetryPreparation(op::RetryPreparation {
            thread_id: opened.session,
        }),
    )
    .await
    .unwrap();
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    assert_eq!(agent.next.load(Ordering::SeqCst), 1);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn uncertain_initial_delivery_survives_restart_without_resubmission() {
    for echoed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
        agent.fail_submit.store(2, Ordering::SeqCst);
        let host = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let (opened, mut updates) = admit(&host, client.id(), &request(directory.path())).await;
        until(&mut updates, |change| matches!(change, SessionChange::Preparation { preparation: Some(preparation) } if preparation.status == PreparationStatus::Unknown)).await;
        let idle = host
            .inner
            .router
            .submission_lock(&opened.session)
            .lock_owned()
            .await;
        drop(idle);
        if echoed {
            let native = host
                .inner
                .database
                .get(&opened.session)
                .unwrap()
                .unwrap()
                .active;
            let mut user = agent_protocol::models::Item::new(
                "persisted-input".into(),
                agent_protocol::models::ItemStatus::Completed,
                agent_protocol::models::ItemBody::UserMessage {
                    text: Some("Build the requested feature".into()),
                    content: Vec::new(),
                },
            );
            user.client_input_id = Some("first-input".into());
            agent
                .records
                .lock()
                .unwrap()
                .get_mut(&native.id)
                .unwrap()
                .thread
                .turns = Some(vec![Arc::new(agent_protocol::models::Turn {
                id: "persisted-turn".into(),
                status: agent_protocol::models::TurnStatus::Completed,
                items: Some(vec![Arc::new(user)]),
                ..Default::default()
            })]);
        }
        host.close_session(client.id());
        drop(updates);
        drop(host);
        let agent = agent.restarted();
        let restarted = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let connection = restarted.open_session();
        let original = opened
            .response
            .thread
            .preparation
            .as_ref()
            .unwrap()
            .submission
            .clone();
        let replay =
            call::<op::SubmissionReceipt>(&restarted, connection.id(), Call::Submit(original))
                .await;
        if echoed {
            assert!(replay.unwrap().turn_id.is_some());
        } else {
            assert_eq!(replay.unwrap_err().code, "submission_delivery_unknown");
        }
        let history: OpenedSession = call(
            &restarted,
            connection.id(),
            Call::OpenSession(OpenSession {
                session: opened.session.clone(),
                limit: 5,
                include_activity: true,
                known_version: None,
            }),
        )
        .await
        .unwrap();
        if echoed {
            assert!(history.response.thread.preparation.is_none());
            assert_eq!(history.response.thread.turns.unwrap().len(), 1);
        } else {
            assert_eq!(
                history.response.thread.preparation.unwrap().status,
                PreparationStatus::Unknown
            );
        }
        let failure = call::<Empty>(
            &restarted,
            connection.id(),
            Call::RetryPreparation(op::RetryPreparation {
                thread_id: opened.session,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            failure.code,
            if echoed {
                "preparation_finished"
            } else {
                "submission_delivery_unknown"
            }
        );
        assert!(agent.inputs.lock().unwrap().is_empty());
    }
}
