use super::*;

#[test]
fn joined_receipts_deliver_the_result_to_every_waiter() {
    let receipt = Receipt::default();
    let mut waiters = Vec::new();
    for _ in 0..256 {
        let (sender, receive) = oneshot::channel();
        receipt.join(Receipt::new(sender));
        waiters.push(receive);
    }
    for waiter in &mut waiters {
        assert!(matches!(
            waiter.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }
    receipt.join(receipt.clone());
    // A caller cancelling its wait must not affect the other callers.
    waiters.pop();
    receipt.send(Ok(Outcome::Applied));
    for mut waiter in waiters {
        assert_eq!(waiter.try_recv().unwrap().unwrap(), Outcome::Applied);
    }
}

#[tokio::test]
async fn rejected_downloads_return_errors_without_overwriting_application_notices() {
    let mut snapshot = Snapshot {
        connection: crate::state::Connection {
            ..Default::default()
        },
        panes: crate::state::PaneStates::with_primary(Arc::new(crate::state::PaneState {
            error: Some("unrelated failure".into()),
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut download = Scheduled::new(
        Effect::execute(op::DownloadFile {
            source: "/missing.png".into(),
            destination: "/preview.png".into(),
        }),
        &mut snapshot,
        "",
        None,
    );
    let (sender, receipt) = oneshot::channel();
    download.complete = Some(Receipt::new(sender));
    let (updates, _) = watch::channel(Arc::new(snapshot.clone()));
    assert!(finish(&updates, rejected(download, busy_error())).is_empty());
    assert_eq!(
        receipt.await.unwrap().unwrap_err().to_string(),
        busy_error().to_string()
    );
    assert_eq!(updates.borrow().as_ref(), &snapshot);
    let store = Store::offline(Snapshot::default(), None);
    assert!(
        store
            .dispatch(Intent::DownloadFile(op::DownloadFile {
                source: "/missing.png".into(),
                destination: "/preview.png".into(),
            }))
            .await
            .is_err()
    );
    assert!(store.snapshot().panes.primary().error.is_none());
    let error = store
        .dispatch(Intent::RemoveWorktree(op::RemoveWorktree {
            path: "/worktree".into(),
        }))
        .await
        .unwrap_err();
    assert_eq!(
        store.snapshot().panes.primary().error.as_deref(),
        Some(error.to_string().as_str())
    );
    store.close().await.unwrap();
}

#[test]
fn shared_operation_failures_remain_visible_after_navigation() {
    for effect in [
        Effect::execute(op::RemoveWorktree {
            path: "/worktree".into(),
        }),
        Effect::execute(op::WriteTerminal {
            handle: "terminal".into(),
            data: Vec::new(),
        }),
    ] {
        let mut snapshot = Snapshot::default();
        let scheduled = Scheduled::new(effect, &mut snapshot, "", None);
        let (snapshot, _) = crate::state::reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/another".into(),
            }),
        );
        let (updates, _) = watch::channel(Arc::new(snapshot));
        finish(&updates, rejected(scheduled, busy_error()));
        assert_eq!(
            updates.borrow().panes.primary().error.as_deref(),
            Some(busy_error().to_string().as_str()),
        );
    }
}

#[tokio::test]
async fn a_terminal_failure_does_not_discard_the_composer_catalog_load() {
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(crate::state::Navigation {
        cwd: "/fixture".into(),
        draft_key: "new:/fixture".into(),
        ..Default::default()
    });
    let scheduled = Scheduled::new(
        Effect::execute(op::StartTerminal {
            command: None,
            handle: "terminal".into(),
            cwd: "/fixture".into(),
            size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
        }),
        &mut snapshot,
        "",
        None,
    );
    let ((client, _events), mut reader, writer) = host_fixture::connect(&snapshot).await;
    reader.script_composer_catalog();
    let (updates, _) = watch::channel(Arc::new(snapshot));
    let followups = finish(&updates, rejected(scheduled, busy_error()));
    assert!(updates.borrow().catalog.composer_catalogs["/fixture"].loading);
    let loading = tokio::spawn(async move {
        for followup in followups {
            finish(&updates, perform(Some(&client), None, followup).await);
        }
        updates.borrow().clone()
    });
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_request())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(request["method"], "host/composer/catalog");
    assert_eq!(request["params"]["cwd"], "/fixture");
    writer
        .reply(
            &request,
            serde_json::json!({"result":{"cwd":"/fixture","loading":false,"candidates":[],"errors":{}}}),
        )
        .await
        .unwrap();
    let loaded = tokio::time::timeout(std::time::Duration::from_secs(2), loading)
        .await
        .unwrap()
        .unwrap();
    assert!(!loaded.catalog.composer_catalogs["/fixture"].loading);
}

#[test]
fn workspace_reads_keep_independent_statuses_and_retire_only_their_owner() {
    let mut snapshot = Snapshot::default();
    snapshot.open_pane("side".into(), "/fixture".into());
    Scheduled::new(
        Effect::execute(op::ReadPermissionSettings {
            provider: crate::session::ProviderKind::Codex,
        }),
        &mut snapshot,
        "side",
        None,
    );
    for pane in ["", "side"] {
        for effect in [
            Effect::execute(op::ListFiles {
                path: "/fixture".into(),
            }),
            Effect::execute(op::ReadFile {
                path: "/fixture/file".into(),
                discard_draft: false,
            }),
            Effect::execute(op::ReviewWorkspace {
                base_ref: None,
                cwd: "/fixture".into(),
            }),
        ] {
            Scheduled::new(effect, &mut snapshot, pane, None);
        }
        for key in [
            op::OperationKey::Directory { pane: pane.into() },
            op::OperationKey::File { pane: pane.into() },
            op::OperationKey::WorkspaceReview { pane: pane.into() },
        ] {
            assert!(snapshot.operation_running(key));
        }
    }
    snapshot.invalidate_pane("side");
    for (pane, running) in [("", true), ("side", false)] {
        for key in [
            op::OperationKey::Directory { pane: pane.into() },
            op::OperationKey::File { pane: pane.into() },
            op::OperationKey::WorkspaceReview { pane: pane.into() },
        ] {
            assert_eq!(snapshot.operation_running(key), running);
        }
    }
    assert!(snapshot.operation_running(op::OperationKey::Permissions {
        provider: crate::session::ProviderKind::Codex,
    }));
}

fn queued(scheduling: op::Scheduling) -> Scheduled {
    let mut effect = Effect::execute(crate::state::list_sync::SubscribeCatalog { refresh: false });
    effect.scheduling = scheduling;
    Scheduled {
        effect,
        scope: Scope {
            pane: String::new(),
            connection: 0,
            navigation: Some(0),
            operation: None,
            report_error: true,
        },
        complete: None,
    }
}

fn item(id: &str) -> op::ReadItem {
    op::ReadItem {
        thread_id: crate::session::SessionRef {
            provider: Some(crate::session::ProviderKind::Codex),
            id: "chat".into(),
        },
        turn_id: "turn".into(),
        item_id: id.into(),
    }
}

fn enqueue(queue: &mut Scheduler, scheduling: op::Scheduling) {
    assert!(matches!(
        queue.admit(queued(scheduling), &Snapshot::default(), 0),
        Admission::Queued
    ));
}

#[test]
fn scheduler_serializes_each_terminal_and_releases_completed_work() {
    let mut queue = Scheduler::default();
    let terminal = |handle: &str| op::Scheduling::Terminal {
        handle: handle.into(),
        starts: false,
    };
    for handle in ["a", "a", "b"] {
        enqueue(&mut queue, terminal(handle));
    }
    let first = queue.next(0).unwrap();
    assert_eq!(first.effect.scheduling.terminal(), Some("a"));
    assert_eq!(
        queue.next(1).unwrap().effect.scheduling.terminal(),
        Some("b")
    );
    assert!(queue.next(2).is_none());
    assert!(queue.complete("", &first.effect.scheduling, &[]).is_empty());
    assert_eq!(
        queue.next(1).unwrap().effect.scheduling.terminal(),
        Some("a")
    );
}

#[test]
fn scheduler_reserves_control_capacity_and_bounds_terminal_concurrency() {
    let mut queue = Scheduler::default();
    assert!(matches!(
        queue.admit(
            queued(op::Scheduling::Concurrent),
            &Snapshot::default(),
            MAX_RPC_JOBS
        ),
        Admission::Rejected(_)
    ));
    assert!(matches!(
        queue.admit(
            queued(op::Scheduling::Control),
            &Snapshot::default(),
            MAX_RPC_JOBS
        ),
        Admission::Run(_)
    ));
    assert!(matches!(
        queue.admit(
            queued(op::Scheduling::Control),
            &Snapshot::default(),
            MAX_RPC_JOBS + CONTROL_RESERVE
        ),
        Admission::Rejected(_)
    ));
    let mut running = Vec::new();
    for index in 0..=MAX_TERMINAL_JOBS {
        enqueue(
            &mut queue,
            op::Scheduling::Terminal {
                handle: index.to_string(),
                starts: false,
            },
        );
    }
    for index in 0..MAX_TERMINAL_JOBS {
        running.push(queue.next(index).unwrap());
    }
    assert!(queue.next(MAX_TERMINAL_JOBS).is_none());
    queue.complete("", &running.pop().unwrap().effect.scheduling, &[]);
    assert!(queue.next(MAX_TERMINAL_JOBS - 1).is_some());
}

#[test]
fn scheduler_coalesces_latest_reads_until_completion() {
    let mut queue = Scheduler::default();
    let latest = || {
        queued(op::Scheduling::LatestHostState(op::OperationKey::Models {
            provider: agent_protocol::session::ProviderKind::Codex,
        }))
    };
    let Admission::Run(first) = queue.admit(latest(), &Snapshot::default(), 0) else {
        panic!("initial read must run")
    };
    assert!(matches!(
        queue.admit(latest(), &Snapshot::default(), 1),
        Admission::Queued
    ));
    let mut newest = latest();
    newest.scope.navigation = Some(9);
    assert!(matches!(
        queue.admit(newest, &Snapshot::default(), 1),
        Admission::Queued
    ));
    assert!(queue.next(1).is_none());
    let mut followups = queue.complete("", &first.effect.scheduling, &[]);
    assert_eq!(followups.len(), 1);
    assert_eq!(followups[0].scope.navigation, Some(9));
    assert!(matches!(
        queue.admit(followups.pop().unwrap(), &Snapshot::default(), 0),
        Admission::Run(_)
    ));
}

#[test]
fn scheduler_releases_item_slots_and_prioritizes_items_over_terminals() {
    let mut queue = Scheduler::default();
    enqueue(
        &mut queue,
        op::Scheduling::Terminal {
            handle: "terminal".into(),
            starts: false,
        },
    );
    for index in 0..MAX_ITEM_TRANSFERS {
        enqueue(&mut queue, op::Scheduling::Item(item(&index.to_string())));
    }
    assert!(queue.next(MAX_RPC_JOBS).is_none());
    let first = queue.next(0).unwrap();
    assert_eq!(first.effect.scheduling.item(), Some(&item("0")));
    for index in 1..MAX_ITEM_TRANSFERS {
        assert!(
            queue
                .next(index)
                .unwrap()
                .effect
                .scheduling
                .item()
                .is_some()
        );
    }
    queue.complete("", &first.effect.scheduling, &[]);
    enqueue(&mut queue, op::Scheduling::Item(item("new")));
    let replacement = queue.next(0).unwrap();
    assert_eq!(replacement.effect.scheduling.item(), Some(&item("new")));
    queue.complete("", &replacement.effect.scheduling, &[]);
    enqueue(&mut queue, op::Scheduling::Item(item("0")));
    assert_eq!(
        queue.next(0).unwrap().effect.scheduling.item(),
        Some(&item("0"))
    );
    assert_eq!(
        queue.next(0).unwrap().effect.scheduling.terminal(),
        Some("terminal")
    );
}

#[test]
fn scheduler_continuations_keep_joined_receipts_and_release_unrelated_items() {
    let mut queue = Scheduler::default();
    let (first, mut first_receipt) = oneshot::channel();
    let mut scheduled = queued(op::Scheduling::Item(item("one")));
    scheduled.complete = Some(Receipt::new(first));
    assert!(matches!(
        queue.admit(scheduled, &Snapshot::default(), 0),
        Admission::Queued
    ));
    let reading = queue.next(0).unwrap();
    let mut continuation = queued(op::Scheduling::Item(item("one")));
    continuation.effect.receipt = ReceiptPolicy::Continue;
    continuation.complete = reading.complete;
    queue.complete(
        "",
        &reading.effect.scheduling,
        std::slice::from_ref(&continuation),
    );
    let (second, mut second_receipt) = oneshot::channel();
    let mut duplicate = queued(op::Scheduling::Item(item("one")));
    duplicate.complete = Some(Receipt::new(second));
    assert!(matches!(
        queue.admit(duplicate, &Snapshot::default(), 0),
        Admission::Queued
    ));
    assert!(queue.next(0).is_none());
    assert!(matches!(
        queue.admit(continuation, &Snapshot::default(), 0),
        Admission::Queued
    ));
    let continued = queue.next(0).unwrap();
    continued.complete.unwrap().send(Ok(Outcome::Applied));
    assert!(first_receipt.try_recv().unwrap().is_ok());
    assert!(second_receipt.try_recv().unwrap().is_ok());
    let mut unrelated = queued(op::Scheduling::Item(item("other")));
    unrelated.effect.receipt = ReceiptPolicy::Continue;
    queue.complete("", &continued.effect.scheduling, &[unrelated]);
    enqueue(&mut queue, op::Scheduling::Item(item("one")));
    assert_eq!(
        queue.next(0).unwrap().effect.scheduling.item(),
        Some(&item("one"))
    );
}

#[test]
fn scheduler_ignores_superseded_and_unobserved_reads() {
    let mut queue = Scheduler::default();
    let mut scheduled = queued(op::Scheduling::LatestHostState(op::OperationKey::Models {
        provider: agent_protocol::session::ProviderKind::Codex,
    }));
    scheduled.scope.operation = Some((
        op::OperationKey::Models {
            provider: agent_protocol::session::ProviderKind::Codex,
        },
        1,
    ));
    assert!(matches!(
        queue.admit(scheduled, &Snapshot::default(), 0),
        Admission::Unobserved(_)
    ));
    assert!(matches!(
        queue.admit(
            queued(op::Scheduling::LatestAgents(item("one").thread_id)),
            &Snapshot::default(),
            0
        ),
        Admission::Unobserved(_)
    ));
}

use crate::test_host as host_fixture;

#[tokio::test]
async fn saved_unknown_input_is_resolved_on_the_host_before_restoring_the_composer() {
    use agent_protocol::{
        conversation::records,
        session::{
            PreparationStage, PreparationStatus, ProviderKind, SessionPreparation, SessionRef,
        },
    };
    use serde_json::json;
    for (restore, accepted, other_host) in [
        (true, true, false),
        (false, true, false),
        (true, false, false),
        (false, false, false),
        (true, true, true),
    ] {
        let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session.clone(),
            Arc::new(crate::models::Thread {
                id: Some(session.clone()),
                active_provider: Some(ProviderKind::Codex),
                preparations: vec![SessionPreparation {
                    submission: agent_protocol::operations::Submission {
                        thread_id: session.clone(),
                        client_user_message_id: input.clone(),
                        input: vec![agent_protocol::operations::Input::Text {
                            text: "recover me".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                    stage: PreparationStage::Provider,
                    status: PreparationStatus::Unknown,
                }],
                ..Default::default()
            }),
        );
        let thread = snapshot.sessions.conversations[&session].as_ref().clone();
        crate::test_conversations::open(
            &mut snapshot,
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
        );
        let mut effect = op::resolve_saved_input(&mut snapshot, &input, restore).unwrap();
        effect.operation.capture(&snapshot, "");
        let ((client, _events), mut reader, writer) =
            host_fixture::connect(&Snapshot::default()).await;
        let mut subscriptions = Vec::new();
        let mut context = Execution {
            client: &client,
            session: None,
            subscriptions: &mut subscriptions,
        };
        let reply = async {
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/conversation/change");
            let command: agent_protocol::conversation::Command =
                serde_json::from_value(request["params"]["command"].clone()).unwrap();
            assert_eq!(
                command,
                agent_protocol::conversation::Command::ResolveUnknown {
                    id: input.clone(),
                    retry: false
                }
            );
            let receipt = records::Acknowledged {
                error: (!accepted).then(|| "already resolved".into()),
                sequence: 2,
                accepted_at: String::new(),
            };
            writer
                .reply(&request, json!({"result":receipt}))
                .await
                .unwrap();
        };
        let key = crate::state::DraftKey::from(&session);
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(crate::state::Draft {
                text: "new typing".into(),
                ..Default::default()
            }),
        );
        let (result, ()) = tokio::join!(effect.operation.run(&mut context), reply);
        if other_host {
            snapshot.connection.storage_scope = "another-host".into();
        }
        match result {
            Ok(completion) => {
                let result = completion.application.apply(&mut snapshot, "", true);
                assert_eq!(result.is_ok(), accepted);
                if let Ok(effects) = result {
                    assert!(effects.is_empty());
                }
            }
            Err(_) => assert!(!accepted),
        }
        assert_eq!(
            snapshot.input.drafts[&key].text,
            if restore && accepted && !other_host {
                "recover me\nnew typing"
            } else {
                "new typing"
            }
        );
        assert_eq!(
            snapshot.sessions.conversations[&session].preparations.len(),
            1,
            "only canonical rows settle the saved input"
        );
    }
}

#[tokio::test]
async fn new_task_admits_the_initial_input_in_one_rpc() {
    use crate::state::operations::Operation;
    use serde_json::json;
    let ((client, _events), mut reader, writer) = host_fixture::connect(&Snapshot::default()).await;
    let mut subscriptions = Vec::new();
    let mut context = Execution {
        client: &client,
        session: None,
        subscriptions: &mut subscriptions,
    };
    let operation = op::StartSubmission {
        thread_id: crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap(),
        provider: agent_protocol::session::ProviderKind::Codex,
        cwd: Some("/project".into()),
        client_user_message_id: "00000000-0000-0000-0000-00000000002a".into(),
        draft: Arc::new(crate::state::Draft {
            text: "initial message".into(),
            attachments: vec![crate::state::Attachment {
                path: "/image.png".into(),
                name: "image.png".into(),
                is_image: true,
            }],
            model: Some(agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "chosen-model".into(),
            }),
            effort: Some("high".into()),
            service_tier: Some("fast".into()),
            ..Default::default()
        }),
    };
    let base = crate::state::conversations::Capture {
        generation: 1,
        projection: None,
    };
    let reply = async {
        let request = reader.read_request().await.unwrap().unwrap();
        assert_eq!(request["method"], "host/conversation/create");
        let params = &request["params"];
        assert_eq!(params["initial"][0], "00000000-0000-0000-0000-00000000002a");
        assert_eq!(params["initial"][1]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(params["selection"]["effort"], "high");
        assert_eq!(params["selection"]["service_tier"], "fast");
        let mut response = crate::models::ThreadResponse {
            thread: crate::models::Thread {
                id: Some(operation.thread_id.clone()),
                active_provider: Some(operation.provider),
                cwd: Some("/project".into()),
                status: crate::models::SessionStatus::Preparing,
                preparations: vec![agent_protocol::session::SessionPreparation {
                    submission: agent_protocol::operations::Submission {
                        thread_id: operation.thread_id.clone(),
                        client_user_message_id: "00000000-0000-0000-0000-00000000002a".into(),
                        input: serde_json::from_value(params["initial"][1]["parts"].clone())
                            .unwrap(),
                        model: operation.draft.model.clone(),
                        effort: Some("high".into()),
                        service_tier: Some("fast".into()),
                        follow_up: Default::default(),
                    },
                    status: agent_protocol::session::PreparationStatus::Preparing,
                    stage: agent_protocol::session::PreparationStage::Workspace,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        response.model = operation.draft.model.clone();
        let projection = crate::test_conversations::projection(response, 1);
        let opened = agent_protocol::conversation::records::Opened {
            session: operation.thread_id.clone(),
            subscription_id: uuid::Uuid::nil(),
            initial: vec![
                agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(projection),
                agent_protocol::orchestration::projection::ThreadStreamItem::Synchronized,
            ],
            provider: operation.provider,
            capabilities: Default::default(),
            project_root: None,
        };
        writer
            .reply(&request, json!({"result": opened}))
            .await
            .unwrap();
    };
    let (opened, ()) = tokio::join!(operation.run(base.clone(), &mut context), reply);
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.sessions.subscription_generations)
        .insert(operation.thread_id.clone(), base.generation);
    Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
        "00000000-0000-0000-0000-00000000002a".into(),
        Arc::new(crate::state::PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: None,
            draft_key: operation.thread_id.clone().into(),
            draft: operation.draft.clone(),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
            queued: false,
        }),
    );
    let effects = operation.apply(&mut snapshot, "", opened.unwrap());
    assert!(
        snapshot.sessions.pending_submissions.is_empty(),
        "the canonical input replaces the local outbox once its row arrives"
    );
    let thread = snapshot.sessions.conversations.values().next().unwrap();
    assert_eq!(
        thread
            .preparations
            .first()
            .unwrap()
            .submission
            .service_tier
            .as_deref(),
        Some("fast")
    );
    let rendered = crate::presentation::conversation::project_conversation(
        crate::localization::UiLanguage::Japanese,
        &snapshot.panes.primary().navigation.draft_key,
        &snapshot.sessions.pending_submissions,
        thread.clone(),
        &None,
    );
    assert_eq!(rendered.queued.len(), 1);
    assert_eq!(
        rendered.queued[0].data.body.as_deref(),
        Some("initial message")
    );
    assert!(
        effects.is_empty(),
        "acceptance must not schedule another send"
    );
    client.close().await;
}

#[rstest::rstest]
#[case("ready")]
#[case("failed")]
#[case("rejected")]
#[case("aborted")]
#[case("disconnected")]
#[tokio::test]
async fn only_a_failed_connection_attempt_reports_that_the_host_is_unreachable(
    #[case] outcome: &str,
) {
    use crate::{
        presentation::error::ErrorScope,
        transport::{Endpoint, Identity, Relays, Trust},
    };
    use serde_json::json;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let client = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let host = Endpoint::bind(Identity::generate(), Relays::Loopback)
            .await
            .unwrap();
        let trust = Trust {
            allowed: [client.node_id()].into(),
            ..Default::default()
        };
        let mut initial = Snapshot::default();
        if outcome == "rejected" {
            // A cached scope attaches before the Host answers the introduction.
            initial.connection.storage_scope = format!("{}:cached", host.node_id());
        }
        let draft_key = initial.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut initial.input.drafts).insert(
            draft_key.clone(),
            Arc::new(crate::state::Draft {
                text: "unsent draft".into(),
                ..Default::default()
            }),
        );
        let (peer, _old_reader, old_writer) = host_fixture::connect(&initial).await;
        let store = Arc::new(Store::new(peer, initial, None));
        let mut snapshots = store.subscribe();
        drop(old_writer);
        snapshots
            .wait_for(|snapshot| !snapshot.connection.connected)
            .await
            .unwrap();
        let notices = || store.snapshot().error_notices("", ErrorScope::Application);
        assert!(
            notices().is_empty(),
            "losing an established connection, for example while suspended, is not a failure"
        );
        let recovering = tokio::spawn({
            let store = store.clone();
            let endpoint = client.clone();
            let ticket = host.local_ticket();
            async move { store.resume(&endpoint, &ticket, "Fixture device").await }
        });
        let incoming = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let (session, mut reader, writer) = host_fixture::accept(incoming).await;
        let scope = reader.read_request().await.unwrap().unwrap();
        assert_eq!(scope["method"], "host/device/identify");
        assert!(notices().is_empty());
        match outcome {
            "ready" => {
                writer
                    .reply(
                        &scope,
                        json!({"result":{"storageScope":"restored-storage","ticket":""}}),
                    )
                    .await
                    .unwrap();
                recovering.await.unwrap().unwrap();
                assert!(store.snapshot().connection.connected);
                assert!(notices().is_empty());
            }
            "failed" => {
                writer
                    .reply(
                        &scope,
                        json!({"error":{"code":"unavailable","message":"Host unavailable"}}),
                    )
                    .await
                    .unwrap();
                assert!(recovering.await.unwrap().is_err());
                let notices = notices();
                assert_eq!(notices.len(), 1, "{notices:?}");
                assert_eq!(notices[0].title, "ホストに接続できません");
                assert!(
                    notices[0].text.to_string().contains("Host unavailable"),
                    "{notices:?}"
                );
            }
            "rejected" => {
                recovering.await.unwrap().unwrap();
                writer
                    .reply(
                        &scope,
                        json!({"error":{"code":"unavailable","message":"Host unavailable"}}),
                    )
                    .await
                    .unwrap();
                snapshots
                    .wait_for(|snapshot| !snapshot.connection.connected)
                    .await
                    .unwrap();
                let notices = notices();
                assert_eq!(notices.len(), 1, "{notices:?}");
                assert_eq!(notices[0].title, "ホストに接続できません");
                assert!(
                    notices[0].text.to_string().contains("Host unavailable"),
                    "{notices:?}"
                );
            }
            "aborted" => {
                recovering.abort();
                assert!(recovering.await.unwrap_err().is_cancelled());
                assert!(notices().is_empty());
            }
            "disconnected" => {
                store.disconnect().await.unwrap();
                assert!(recovering.await.unwrap().is_err());
                assert!(notices().is_empty(), "a cancelled attempt is not a failure");
            }
            _ => unreachable!(),
        }
        assert_eq!(
            store.snapshot().input.drafts[&draft_key].text,
            "unsent draft"
        );
        store.close().await.unwrap();
        session.close();
        client.close().await;
        host.close().await;
    })
    .await
    .expect("connection recovery did not settle");
}

#[tokio::test]
async fn restoring_offline_does_not_report_a_connection_failure() {
    use crate::presentation::error::ErrorScope;
    let initial = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            ..Default::default()
        },
        panes: crate::state::PaneStates::with_primary(Arc::new(crate::state::PaneState {
            error: Some("previous runtime error".into()),
            ..Default::default()
        })),
        ..Default::default()
    };
    let store = Store::offline(initial, None);
    assert!(!store.snapshot().connection.connected);
    assert!(
        store
            .snapshot()
            .error_notices("", ErrorScope::Application)
            .is_empty()
    );
    store.close().await.unwrap();
    assert!(
        store
            .snapshot()
            .error_notices("", ErrorScope::Application)
            .is_empty()
    );
}

#[rstest::rstest]
#[case("disconnect")]
#[case("close")]
#[case("transport loss")]
#[tokio::test]
async fn losing_a_connection_publishes_it_without_reporting_a_failure(#[case] action: &str) {
    use crate::presentation::error::ErrorScope;
    let initial = Snapshot::default();
    let (peer, _reader, writer) = host_fixture::connect(&initial).await;
    let store = Store::new(peer, initial, None);
    let mut snapshots = store.subscribe();
    match action {
        "disconnect" => store.disconnect().await.unwrap(),
        "close" => store.close().await.unwrap(),
        "transport loss" => drop(writer),
        _ => unreachable!(),
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        snapshots.wait_for(|snapshot| !snapshot.connection.connected),
    )
    .await
    .expect("the disconnection must be published")
    .unwrap();
    assert!(
        store
            .snapshot()
            .error_notices("", ErrorScope::Application)
            .is_empty()
    );
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn older_dispatched_list_cannot_replace_newer_published_work() {
    use serde_json::json;
    let initial = Snapshot::default();
    let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
    let store = Store::new(peer, initial, None);
    let publications = store.publications.lock().unwrap().clone().unwrap();
    // Dispatch publishes before its command is consumed. Commands can arrive
    // out of order after newer work has already been published.
    let older = apply(
        &publications,
        Event::Intent(Intent::ListSessions(Default::default())),
    );
    let newer = apply(
        &publications,
        Event::Intent(Intent::ListSessions(Default::default())),
    );
    let mut receipts = Vec::new();
    for effects in [newer, older] {
        let (complete, receipt) = oneshot::channel();
        store
            .commands
            .send(Command::Dispatch(Dispatch { effects, complete }))
            .await
            .unwrap();
        receipts.push(receipt);
    }
    let replies = async {
        loop {
            let request = reader.read_request().await.unwrap().unwrap();
            let result = match request["method"].as_str().unwrap() {
                "host/list/subscribe" => {
                    json!({"data":[{"id":{"provider":"claude","id":"latest"}}],
                    "projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5})
                }
                "host/model/list" => json!({"data":[]}),
                "host/account/list" => json!({"accounts":[],"selected":{}}),
                "host/name" => json!("Fixture"),
                method => panic!("unexpected request: {method}"),
            };
            writer
                .reply(&request, json!({"result":result}))
                .await
                .unwrap();
        }
    };
    let settled = async {
        for receipt in receipts {
            receipt.await.unwrap().unwrap();
        }
        let mut updates = store.subscribe();
        updates
            .wait_for(|snapshot| {
                snapshot.lists.threads.as_ref().is_some_and(|page| {
                    page.data
                        .iter()
                        .any(|thread| thread.id.as_ref().is_some_and(|id| id.id == "latest"))
                }) && snapshot
                    .operations
                    .get(&op::OperationKey::SessionList)
                    .is_none()
            })
            .await
            .unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! { _ = settled => {}, _ = replies => unreachable!() }
    })
    .await
    .expect("The newest list must finish even when an older dispatch arrives afterward");
    store.close().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_command_queue_keeps_the_draft_and_all_submission_state() {
    let mut initial = reduce(
        &Snapshot::default(),
        "",
        Event::Intent(Intent::NewChat {
            cwd: "/project".into(),
        }),
    )
    .0;
    Arc::make_mut(&mut initial.input.drafts).insert(
        initial.panes.primary().navigation.draft_key.clone(),
        Arc::new(crate::state::Draft {
            model: Some(agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Claude,
                id: "model".into(),
            }),
            ..Default::default()
        }),
    );
    let store = Store::offline(initial, None);
    let draft_key = store
        .snapshot()
        .panes
        .primary()
        .navigation
        .draft_key
        .clone();
    store
        .dispatch(Intent::SetDraftText {
            thread_id: draft_key,
            text: "must not be lost".into(),
        })
        .await
        .unwrap();
    for index in 0..MAX_COMMANDS {
        drop(store.dispatch(Intent::ReadFile(op::ReadFile {
            path: format!("/queued/{index}"),
            discard_draft: false,
        })));
    }
    let before = store.snapshot();
    let result = store
        .dispatch(Intent::Submit {
            thread_id: None,
            client_user_message_id: "input".into(),
            alternate: false,
        })
        .await;
    assert!(matches!(result, Err(PeerError::InvalidMessage(_))));
    assert!(Arc::ptr_eq(&before, &store.snapshot()));
    assert!(matches!(
        store.disconnect().await,
        Err(PeerError::InvalidMessage(_))
    ));
    store.close().await.unwrap();
}
#[test]
fn scheduling_a_submission_dates_only_that_input_once() {
    let pending = |submitted_at_ms| {
        Arc::new(crate::state::PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms,
            draft_key: "draft".into(),
            draft: Arc::default(),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: false,
            queued: false,
        })
    };
    let mut current = Snapshot {
        sessions: crate::state::Sessions {
            pending_submissions: Arc::new(
                [
                    ("unknown-time".into(), pending(None)),
                    ("known-time".into(), pending(Some(12_000))),
                    ("new".into(), pending(None)),
                ]
                .into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    let send = |id: &str| {
        Effect::execute(op::StartSubmission {
            thread_id: crate::session::SessionRef::new(
                crate::session::ProviderKind::Codex,
                uuid::Uuid::new_v4().to_string(),
            )
            .unwrap(),
            provider: crate::session::ProviderKind::Codex,
            cwd: None,
            client_user_message_id: id.into(),
            draft: Arc::default(),
        })
    };
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    Scheduled::new(send("new"), &mut current, "", None);
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let submitted_at = current.sessions.pending_submissions["new"]
        .submitted_at_ms
        .unwrap();
    assert!((before..=after).contains(&submitted_at));
    assert_eq!(
        current.sessions.pending_submissions["unknown-time"].submitted_at_ms,
        None
    );
    Scheduled::new(send("known-time"), &mut current, "", None);
    assert_eq!(
        current.sessions.pending_submissions["known-time"].submitted_at_ms,
        Some(12_000)
    );
}

#[test]
// Reading store.snapshot() after a receipt cannot prove subscribers were
// notified. Keep publication coverage independent of operation behavior.
fn every_snapshot_field_notifies_subscribers_independently() {
    type Change = fn(&mut Snapshot);
    let changes: &[(&str, Change)] = &[
        ("project_import", |snapshot| {
            snapshot.project_import = Arc::default()
        }),
        ("operations", |snapshot| {
            snapshot.operations = Default::default()
        }),
        ("operation_sequence", |snapshot| {
            snapshot.operation_sequence += 1
        }),
        ("model_preferences", |snapshot| {
            snapshot.catalog.model_preferences.provider =
                Some(crate::session::ProviderKind::Claude);
        }),
        ("ui_language", |snapshot| {
            snapshot.catalog.ui_language = crate::localization::UiLanguage::English;
        }),
        ("permission_settings", |snapshot| {
            snapshot.catalog.permission_settings = Arc::new(
                [(
                    agent_protocol::session::ProviderKind::Codex,
                    Arc::new(crate::state::operations::PermissionSettingsState { result: None }),
                )]
                .into(),
            );
        }),
        ("account", |snapshot| {
            snapshot.catalog.account = Arc::default()
        }),
        ("terminals", |snapshot| snapshot.terminals = Arc::default()),
        ("conversations", |snapshot| {
            snapshot.sessions.conversations = Arc::default()
        }),
        ("task_activity", |snapshot| {
            snapshot.lists.task_activity =
                Some(Arc::new(agent_protocol::live_activity::TaskActivityState {
                    attention: Vec::new(),
                    waiting: Vec::new(),
                    background: Vec::new(),
                    unread: Vec::new(),
                    statuses: Vec::new(),
                    display: agent_protocol::live_activity::TaskActivitySummary {
                        running: 1,
                        ..Default::default()
                    }
                    .display(),
                }));
        }),
        ("expanded_projects", |snapshot| {
            snapshot.lists.expanded_projects = Arc::new([("p".into(), 5)].into());
        }),
        ("project_threads", |snapshot| {
            snapshot.lists.project_threads = Arc::new(
                [(
                    "p".into(),
                    Arc::new(crate::models::ThreadList {
                        data: vec![],
                        projects: vec![],
                        has_more: false,
                        has_more_projects: false,
                        limit: 5,
                        provider_errors: None,
                    }),
                )]
                .into(),
            );
        }),
        ("observed_agents", |snapshot| {
            Arc::make_mut(snapshot.panes.primary_mut()).observed_agents =
                Some(crate::session::SessionRef {
                    provider: Some(crate::session::ProviderKind::Codex),
                    id: "parent".into(),
                });
        }),
        ("models", |snapshot| {
            snapshot.catalog.models = Arc::default()
        }),
        ("drafts", |snapshot| snapshot.input.drafts = Arc::default()),
        ("pending_submissions", |snapshot| {
            snapshot.sessions.pending_submissions = Arc::default()
        }),
        ("file_drafts", |snapshot| {
            snapshot.files.file_drafts = Arc::default()
        }),
        ("workspace", |snapshot| {
            snapshot.files.workspace = Arc::default()
        }),
        ("navigation", |snapshot| {
            Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::default()
        }),
        ("activity", |snapshot| {
            snapshot.lists.activity = Arc::default()
        }),
        ("management", |snapshot| {
            snapshot.connection.management = Arc::default()
        }),
        ("list_query", |snapshot| {
            snapshot.lists.list_query = Arc::default()
        }),
        ("threads", |snapshot| {
            snapshot.lists.threads = Some(Arc::new(crate::models::ThreadList {
                data: Vec::new(),
                projects: Vec::new(),
                has_more: false,
                has_more_projects: false,
                limit: 5,

                provider_errors: None,
            }))
        }),
        ("epoch", |snapshot| snapshot.connection.epoch += 1),
        ("connected", |snapshot| snapshot.connection.connected = true),
        ("connection_failure", |snapshot| {
            snapshot.connection.connection_failure = Some("fixture failure".into())
        }),
        ("error", |snapshot| {
            Arc::make_mut(snapshot.panes.primary_mut()).error = Some("fixture failure".into())
        }),
    ];
    for (name, change) in changes {
        let previous = Arc::new(Snapshot::default());
        let (writer, reader) = watch::channel(previous.clone());
        writer.send_if_modified(|current| publish_locked(current, previous.as_ref().clone()));
        assert!(
            !reader.has_changed().unwrap(),
            "unchanged snapshot published"
        );
        let mut next = previous.as_ref().clone();
        change(&mut next);
        writer.send_if_modified(|current| publish_locked(current, next));
        assert!(reader.has_changed().unwrap(), "{name} update was dropped");
    }
}
