use super::*;
use agent_protocol::models::{Item, Thread, Turn};
use agent_transport::peer::RpcMessage;

#[test]
fn subagent_work_and_requests_do_not_contribute_to_system_activity() {
    use agent_protocol::{models::SessionStatus, session::RequestDelivery};
    let router = SessionRouter::new();
    let root = SessionRef::new(ProviderKind::Codex, "root".into()).unwrap();
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    let grandchild = SessionRef::new(ProviderKind::Codex, "grandchild".into()).unwrap();
    let other = SessionRef::new(ProviderKind::Claude, "root".into()).unwrap();
    let seed = |id: &SessionRef| Thread {
        id: Some(id.clone()),
        status: SessionStatus::Running,
        ..Default::default()
    };
    let initial = router.task_activity(vec![seed(&root), seed(&other)], 0);
    assert_eq!(initial.display.current.total, 2);
    assert_eq!(initial.display.current.label, "実行中 2件");
    assert!(initial.waiting.is_empty());
    for (id, body) in [
        ("question", serde_json::json!({"question":{"questions":[]}})),
        (
            "approval",
            serde_json::json!({"approval":{"kind":"command","description":"run","details":"","choices":[]}}),
        ),
    ] {
        let request = serde_json::from_value(
            serde_json::json!({"id":id,"target":"session","delivery":"awaiting","body":body}),
        )
        .unwrap();
        router.session_change(&grandchild, SessionChange::Request { request });
        let waiting = router.task_activity(Vec::new(), 0);
        assert_eq!(waiting.display.current.total, 2);
        assert_eq!(waiting.display.current.label, "実行中 2件");
        assert!(waiting.waiting.is_empty());
        assert!(!waiting.statuses.iter().any(|(id, _)| id == &grandchild));
        router.session_change(
            &grandchild,
            SessionChange::RequestDelivery {
                request_id: id.into(),
                state: RequestDelivery::Sent,
            },
        );
        assert!(router.task_activity(Vec::new(), 0).waiting.is_empty());
        router.session_change(
            &grandchild,
            SessionChange::ResolveRequest {
                request_id: id.into(),
            },
        );
    }
    for target in [&root, &child, &grandchild] {
        router.session_change(
            target,
            SessionChange::Status {
                status: SessionStatus::Idle,
            },
        );
    }
    assert_eq!(router.task_activity(Vec::new(), 0).display.current.total, 1);
    // Completed actors are evicted; a later child turn is still excluded.
    router.session_change(
        &grandchild,
        SessionChange::Status {
            status: SessionStatus::Running,
        },
    );
    assert_eq!(router.task_activity(Vec::new(), 0).display.current.total, 1);
    router.session_change(
        &grandchild,
        SessionChange::Status {
            status: SessionStatus::Idle,
        },
    );
    router.session_change(
        &other,
        SessionChange::Status {
            status: SessionStatus::Idle,
        },
    );
    assert!(!router.task_activity(Vec::new(), 0).display.ongoing);
}

#[test]
fn each_finished_conversation_keeps_a_mark_until_its_result_is_read() {
    use agent_protocol::{
        execution::TurnStatus,
        live_activity::{MarkSessionRead, TaskActivityIconKind::*},
        models::SessionStatus,
    };
    let router = SessionRouter::new();
    let first = SessionRef::new(ProviderKind::Codex, "first".into()).unwrap();
    let second = SessionRef::new(ProviderKind::Claude, "second".into()).unwrap();
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    let seed = |id: &SessionRef| Thread {
        id: Some(id.clone()),
        status: SessionStatus::Running,
        ..Default::default()
    };
    router.task_activity(vec![seed(&first), seed(&second)], 0);
    let finish = |id: &SessionRef, turn: &str| {
        router.session_change(
            id,
            SessionChange::Status {
                status: SessionStatus::Idle,
            },
        );
        router.session_change(
            id,
            SessionChange::Turn {
                turn: Turn {
                    id: turn.into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        );
    };
    let display = || router.task_activity(Vec::new(), 0).display;
    let kinds = || {
        display()
            .current
            .icons
            .iter()
            .map(|icon| icon.kind)
            .collect::<Vec<_>>()
    };
    finish(&child, "child-turn");
    assert_eq!(
        display().current.label,
        "実行中 2件",
        "a descendant result belongs to its running conversation"
    );
    finish(&second, "second-turn");
    assert_eq!(display().current.label, "実行中 1件 · 完了 1件");
    assert_eq!(kinds(), [Running, Finished]);
    assert!(display().ongoing);
    finish(&first, "first-turn");
    assert!(!display().ongoing);
    assert_eq!(display().current.label, "すべてのタスクが終了");
    assert_eq!(kinds(), [Finished, Finished]);
    router
        .mark_session_read(&MarkSessionRead {
            session: second.clone(),
            turn_id: "second-turn".into(),
        })
        .unwrap();
    assert_eq!(kinds(), [Finished]);
    router.session_change(
        &first,
        SessionChange::Status {
            status: SessionStatus::Running,
        },
    );
    assert_eq!(display().current.label, "実行中 1件");
    assert_eq!(kinds(), [Running], "new work replaces the earlier result");
}

#[test]
fn only_listed_conversations_accept_activity_from_provider_events() {
    use agent_protocol::models::SessionStatus;
    let router = SessionRouter::new();
    let root = SessionRef::new(ProviderKind::Codex, "root".into()).unwrap();
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    let revision = router.task_activity_revision();
    for target in [&root, &child] {
        router.session_change(
            target,
            SessionChange::Status {
                status: SessionStatus::Running,
            },
        );
    }
    assert_eq!(router.task_activity(Vec::new(), 0).display.current.total, 0);
    let listed = router.task_activity(
        vec![Thread {
            id: Some(root.clone()),
            status: SessionStatus::Running,
            ..Default::default()
        }],
        revision,
    );
    assert_eq!(listed.display.current.total, 1);
    router.session_change(
        &root,
        SessionChange::Status {
            status: SessionStatus::Idle,
        },
    );
    let state = router.task_activity(Vec::new(), 0);
    assert_eq!(state.display.current.total, 0);
    assert!(!state.statuses.iter().any(|(id, _)| id == &child));
    router.session_change(
        &child,
        SessionChange::Status {
            status: SessionStatus::Running,
        },
    );
    assert_eq!(router.task_activity(Vec::new(), 0).display.current.total, 0);
}

proptest::proptest! {
    #[test]
    fn descendant_count_and_list_order_do_not_inflate_conversation_count(roots in 1usize..8, descendants in 0usize..24, reverse in proptest::bool::ANY) {
        let router = SessionRouter::new();
        let mut threads = Vec::new();
        for root in 0..roots {
            let id = SessionRef::new(ProviderKind::Codex, format!("root-{root}")).unwrap();
            threads.push(Thread { id: Some(id.clone()), status: agent_protocol::models::SessionStatus::Running, ..Default::default() });
            for child in 0..descendants {
                let id = SessionRef::new(ProviderKind::Codex, format!("child-{root}-{child}")).unwrap();
                router.session_change(&id, SessionChange::Status { status: agent_protocol::models::SessionStatus::Running });
                threads.push(Thread { id: Some(id), parent_id: Some(threads[0].id.clone().unwrap()), status: agent_protocol::models::SessionStatus::Running, ..Default::default() });
            }
        }
        if reverse { threads.reverse(); }
        let display = router.task_activity(threads, 0).display;
        proptest::prop_assert_eq!(display.current.total, roots as u32);
        proptest::prop_assert_eq!(display.current.icons.len(), roots);
    }
}

#[test]
fn task_state_is_authoritative_without_apns_or_conversation_subscriptions() {
    use agent_protocol::models::SessionStatus;
    use agent_protocol::session::RequestDelivery;
    let router = SessionRouter::new();
    let target = SessionRef {
        provider: ProviderKind::Codex,
        id: "task".into(),
    };
    let other = SessionRef {
        provider: ProviderKind::Claude,
        id: "task".into(),
    };
    let seed = |id: &SessionRef, status| Thread {
        id: Some(id.clone()),
        status,
        ..Default::default()
    };
    let initial = router.task_activity(
        vec![
            seed(&target, SessionStatus::Running),
            seed(&other, SessionStatus::Running),
        ],
        0,
    );
    assert_eq!(initial.display.current.label, "実行中 2件");
    let mut cached = seed(&target, SessionStatus::Idle);
    cached.list_stale = Some(true);
    assert_eq!(
        router.task_activity(vec![cached], initial.revision).display,
        initial.display,
        "a cached list cannot change the last confirmed task state"
    );
    router.session_change(
        &target,
        SessionChange::Status {
            status: SessionStatus::Running,
        },
    );
    for (id, body) in [
        ("question", serde_json::json!({"question":{"questions":[]}})),
        (
            "approval",
            serde_json::json!({"approval":{"kind":"command","description":"run","details":"","choices":[]}}),
        ),
    ] {
        let request = serde_json::from_value(serde_json::json!({
            "id":id, "target":"session", "delivery":"awaiting", "body":body
        }))
        .unwrap();
        router.session_change(&target, SessionChange::Request { request });
        let waiting = router.task_activity(Vec::new(), 0);
        assert_eq!(waiting.display.current.label, "確認待ち 1件 · 実行中 1件");
        assert_eq!(waiting.waiting.as_slice(), std::slice::from_ref(&target));
        router.session_change(
            &target,
            SessionChange::RequestDelivery {
                request_id: id.into(),
                state: RequestDelivery::Sent,
            },
        );
        assert_eq!(
            router.task_activity(Vec::new(), 0).display.current.label,
            "実行中 2件"
        );
        assert!(router.task_activity(Vec::new(), 0).waiting.is_empty());
    }
    let read_revision = router.task_activity_revision();
    router.session_change(
        &target,
        SessionChange::Status {
            status: SessionStatus::Idle,
        },
    );
    let remaining =
        router.task_activity(vec![seed(&target, SessionStatus::Running)], read_revision);
    assert_eq!(remaining.display.current.label, "実行中 1件");
    let mut cached = seed(&target, SessionStatus::Running);
    cached.list_stale = Some(true);
    assert_eq!(
        router
            .task_activity(vec![cached], remaining.revision)
            .display,
        remaining.display,
        "a cached list cannot revive an already completed task"
    );
    router.session_change(
        &other,
        SessionChange::Status {
            status: SessionStatus::Unavailable,
        },
    );
    assert_eq!(
        router.task_activity(Vec::new(), 0).display.current.label,
        "状態確認中 1件"
    );
    let completed = router.task_activity(
        vec![seed(&other, SessionStatus::Idle)],
        router.task_activity_revision(),
    );
    assert!(!completed.display.ongoing);
    assert!(completed.revision > initial.revision);
}

#[test]
fn live_activity_survives_disconnect_and_gets_terminal_result_before_execution_is_pruned() {
    use agent_protocol::{
        execution::TurnStatus,
        live_activity::{PushEnvironment, RegisterLiveActivity},
        models::SessionStatus,
    };
    let router = SessionRouter::new();
    let apns = crate::apns::Apns::testing();
    router.set_apns(Some(apns.clone()));
    let phone = router.open_authenticated_session(Some("phone".into()));
    let target = SessionRef {
        provider: ProviderKind::Codex,
        id: "task".into(),
    };
    let params = RegisterLiveActivity {
        activity_id: Some("activity".into()),
        allow_start: false,
        token: vec![1; 32],
        environment: PushEnvironment::Sandbox,
    };
    router.session_change(
        &target,
        SessionChange::Turn {
            turn: Turn {
                id: "turn".into(),
                status: TurnStatus::Running,
                ..Default::default()
            },
            completed: false,
        },
    );
    router.task_activity(
        vec![Thread {
            id: Some(target.clone()),
            status: SessionStatus::Idle,
            ..Default::default()
        }],
        0,
    );
    router.register_live_activity(phone.id(), &params).unwrap();
    assert_eq!(
        apns.test_payloads(crate::apns::now())[0]["aps"]["content-state"]["display"]["current"]["label"],
        "実行中 1件"
    );
    drop(phone);
    router.session_change(
        &target,
        SessionChange::Turn {
            turn: Turn {
                id: "turn".into(),
                status: TurnStatus::Failed,
                ..Default::default()
            },
            completed: true,
        },
    );
    assert!(router.execution_targets().is_empty());
    let final_payload = &apns.test_payloads(crate::apns::now() + 2)[0]["aps"];
    assert_eq!(final_payload["event"], "end");
    assert_eq!(
        final_payload["content-state"]["display"]["current"]["total"],
        0
    );
    let replacement = router.open_authenticated_session(Some("phone".into()));
    router.revoke_device("phone");
    assert!(apns.test_payloads(crate::apns::now() + 3).is_empty());
    assert!(
        router
            .register_live_activity(replacement.id(), &params)
            .is_err()
    );
}

#[tokio::test]
async fn detail_reads_are_ordered_with_live_changes_and_stay_on_the_requesting_connection() {
    let router = SessionRouter::new();
    let first = router.open_session();
    let second = router.open_session();
    let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let response = ThreadResponse {
        thread: Thread {
            id: Some(target.clone()),
            turns: Some(vec![Arc::new(Turn {
                id: "turn".into(),
                items_summary: true,
                items: Some(vec![]),
                ..Default::default()
            })]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut first_updates = router
        .finish_session_read(open(&router, "native"), first.id(), response.clone(), false)
        .unwrap()
        .updates
        .unwrap();
    let mut second_updates = router
        .finish_session_read(open(&router, "native"), second.id(), response, false)
        .unwrap()
        .updates
        .unwrap();
    let item = |text: &str| {
        Arc::new(Item::new(
            "answer".into(),
            Default::default(),
            agent_protocol::models::ItemBody::AssistantText {
                text: text.into(),
                phase: Default::default(),
            },
        ))
    };
    router.session_change(
        &target,
        SessionChange::Turn {
            turn: Turn {
                id: "turn".into(),
                items: Some(vec![item("current")]),
                ..Default::default()
            },
            completed: false,
        },
    );
    let _: SessionChange = protocol::decode(&first_updates.recv().await.unwrap()).unwrap();
    let _: SessionChange = protocol::decode(&second_updates.recv().await.unwrap()).unwrap();
    router
        .finish_turn_read(
            open(&router, "native"),
            first.id(),
            "turn".into(),
            vec![item("old")],
        )
        .unwrap();
    router.session_change(
        &target,
        SessionChange::Text {
            turn_id: "turn".into(),
            item_id: "answer".into(),
            field: agent_protocol::session::TextField::AssistantText,
            delta: " + delta".into(),
        },
    );
    let hydration: SessionChange = protocol::decode(&first_updates.recv().await.unwrap()).unwrap();
    let SessionChange::TurnItems { items, .. } = &hydration else {
        panic!("detail patch missing")
    };
    assert!(
        matches!(items[0].body(), agent_protocol::models::ItemBody::AssistantText {text, ..} if text == "current")
    );
    assert!(matches!(
        protocol::decode::<SessionChange>(&first_updates.recv().await.unwrap()).unwrap(),
        SessionChange::Text { .. }
    ));
    assert!(matches!(
        protocol::decode::<SessionChange>(&second_updates.recv().await.unwrap()).unwrap(),
        SessionChange::Text { .. }
    ));
    assert!(second_updates.receiver.try_recv().is_err());
    drop(first_updates);
    assert!(
        router
            .finish_turn_read(open(&router, "native"), first.id(), "turn".into(), vec![],)
            .is_err()
    );
    assert!(second_updates.receiver.try_recv().is_err());
}
#[tokio::test]
async fn opened_turn_sends_typical_activity_bodies_and_defers_oversized_output() {
    let router = SessionRouter::new();
    let client = router.open_session();
    let response = ThreadResponse {
        thread: Thread {
            id: Some(SessionRef::new(ProviderKind::Codex, "native".into()).unwrap()),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut updates = router
        .finish_session_read(open(&router, "native"), client.id(), response, false)
        .unwrap()
        .updates
        .unwrap();
    let command = |id: &str, output: usize| {
        Arc::new(Item::new(
            id.into(),
            Default::default(),
            agent_protocol::models::ItemBody::CommandExecution {
                command: "cargo test".into(),
                cwd: None,
                output: "x".repeat(output),
                exit_code: Some(0),
            },
        ))
    };
    let typical = command("typical", 4 * 1024);
    router
        .finish_turn_read(
            open(&router, "native"),
            client.id(),
            "turn".into(),
            vec![typical.clone(), command("oversized", OPENED_ACTIVITY_BYTES)],
        )
        .unwrap();
    let SessionChange::TurnItems { items, .. } =
        protocol::decode(&updates.recv().await.unwrap()).unwrap()
    else {
        panic!("turn items missing")
    };
    assert_eq!(items[0], typical);
    assert!(items[1].is_deferred());
}
use agent_protocol::session::TextField;
use serde_json::json;

fn open(router: &SessionRouter, id: &str) -> SessionLease {
    router
        .retain_execution(
            SessionRef::new(agent_protocol::session::ProviderKind::Codex, id.to_string()).unwrap(),
        )
        .unwrap()
}

fn turn(router: &SessionRouter, completed: bool) {
    router.session_change(
        &SessionRef {
            provider: ProviderKind::Codex,
            id: "native".into(),
        },
        SessionChange::Turn {
            turn: Turn {
                id: "run".into(),
                ..Default::default()
            },
            completed,
        },
    );
}

fn assert_no_execution(router: &SessionRouter) {
    let actors: Vec<_> = lock_state(&router.state)
        .executions
        .values()
        .cloned()
        .collect();
    for actor in actors {
        let actor = lock_state(&actor);
        assert!(actor.timeline.turns.is_none());
        assert!(actor.timeline.submissions.is_empty());
        assert!(actor.timeline.requests.is_empty());
        assert!(actor.request_origins.is_empty());
        assert_eq!(actor.leases, 0);
    }
}
#[test]
fn a_busy_conversation_does_not_hold_the_registry_or_another_conversation() {
    let router = SessionRouter::new();
    let a = SessionRef::new(ProviderKind::Codex, "A".into()).unwrap();
    let b = SessionRef::new(ProviderKind::Codex, "B".into()).unwrap();
    let lease = router.retain_execution(a.clone()).unwrap();
    let guard = lock_state(&lease.actor);
    let stalled = std::thread::spawn({
        let router = router.clone();
        move || {
            router.session_change(
                &a,
                SessionChange::Status {
                    status: agent_protocol::models::SessionStatus::Running,
                },
            )
        }
    });
    let (send, receive) = std::sync::mpsc::channel();
    let independent = std::thread::spawn({
        let router = router.clone();
        move || {
            router.session_change(
                &b,
                SessionChange::Turn {
                    turn: Turn {
                        id: "turn".into(),
                        ..Default::default()
                    },
                    completed: false,
                },
            );
            send.send(router.current_turn(&b, "turn").is_some())
                .unwrap();
        }
    });
    let progress = receive.recv_timeout(std::time::Duration::from_secs(2));
    drop(guard);
    stalled.join().unwrap();
    independent.join().unwrap();
    assert!(progress.unwrap());
}
#[tokio::test]
async fn malformed_delta_keeps_all_subscriptions_and_the_last_valid_item() {
    let router = SessionRouter::new();
    let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let connections = [router.open_session(), router.open_session()];
    let response = ThreadResponse {
        thread: Thread {
            id: Some(target.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut streams = Vec::new();
    for connection in &connections {
        streams.push(
            router
                .finish_session_read(
                    open(&router, "native"),
                    connection.id(),
                    response.clone(),
                    false,
                )
                .unwrap()
                .updates
                .unwrap(),
        );
    }
    turn(&router, false);
    router.session_change(
        &target,
        SessionChange::Item {
            turn_id: "run".into(),
            item: Arc::new(Item::new(
                "item".into(),
                agent_protocol::execution::ItemStatus::Running,
                agent_protocol::items::ItemBody::AssistantText {
                    text: "kept".into(),
                    phase: agent_protocol::items::AssistantPhase::Unknown,
                },
            )),
        },
    );
    for stream in &mut streams {
        stream.recv().await.unwrap();
        stream.recv().await.unwrap();
    }
    for (item, field) in [
        ("missing", TextField::AssistantText),
        ("item", TextField::CommandOutput),
    ] {
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "run".into(),
                item_id: item.into(),
                field,
                delta: "bad".into(),
            },
        );
    }
    router.session_change(
        &target,
        SessionChange::Text {
            turn_id: "run".into(),
            item_id: "item".into(),
            field: TextField::AssistantText,
            delta: " good".into(),
        },
    );
    for (connection, stream) in connections.iter().zip(&mut streams) {
        router.ensure_session(connection.id()).unwrap();
        stream.recv().await.unwrap();
    }
    let turn = router.current_turn(&target, "run").unwrap();
    assert!(
        matches!(turn.items.as_ref().unwrap()[0].body(), agent_protocol::items::ItemBody::AssistantText {text,..} if text == "kept good")
    );
}

#[test]
fn resolving_unsubscribed_elicitation_releases_its_execution_immediately() {
    let router = SessionRouter::new();
    let target = SessionRef::new(ProviderKind::Claude, "native".into()).unwrap();
    let (input, _receiver) = tokio::sync::mpsc::channel(1);
    let instance = uuid::Uuid::new_v4();
    let adapted = crate::providers::requests::claude(
        "request".into(),
        &"turn".into(),
        &crate::providers::claude::SdkRequest::Elicitation {
            server_name: String::new(),
            message: String::new(),
            mode: None,
            url: None,
            requested_schema: Some(json!({"type":"object","properties":{}})),
        },
    )
    .unwrap();
    router
        .request(
            target.clone(),
            crate::providers::claude::request_origin(
                instance,
                json!("native-request"),
                input,
                adapted.answers,
            ),
            adapted.request,
        )
        .unwrap();
    assert!(lock_state(&router.state).executions.contains_key(&target));
    router.resolve_native_request(instance, &json!("native-request"));
    assert!(!lock_state(&router.state).executions.contains_key(&target));
}
#[test]
fn turn_requests_require_live_ownership_and_retire_only_with_their_target() {
    use agent_protocol::{execution::ItemStatus, requests::Answer, session::RequestDelivery};
    let router = SessionRouter::new();
    let connection = router.open_session();
    let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    // An outstanding native read also retains the completed turn below.
    let _read = open(&router, "native");
    let instance = uuid::Uuid::new_v4();
    let request = || {
        crate::providers::requests::codex(
            "request".into(),
            "item/commandExecution/requestApproval",
            &json!({"turnId":"wanted","itemId":"tool","availableDecisions":["accept","decline"]}),
        )
        .unwrap()
        .request
    };
    let register = || {
        router.request(
            target.clone(),
            crate::providers::unavailable_origin(
                instance,
                json!("native-request"),
                Default::default(),
            ),
            request(),
        )
    };
    let pending = || {
        router
            .overlay_execution(&target, Thread::default())
            .requests
    };
    let lifecycle = |id: &str, completed| {
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: Turn {
                    id: id.into(),
                    ..Default::default()
                },
                completed,
            },
        );
    };
    let item = |turn: &str, id: &str, status| {
        router.session_change(
            &target,
            SessionChange::Item {
                turn_id: turn.into(),
                item: Arc::new(Item::new(
                    id.into(),
                    status,
                    agent_protocol::items::ItemBody::CommandExecution {
                        command: "true".into(),
                        cwd: None,
                        output: String::new(),
                        exit_code: None,
                    },
                )),
            },
        );
    };
    assert!(register().is_err());
    lifecycle("other", false);
    assert!(register().is_err());
    lifecycle("wanted", false);
    register().unwrap();
    let previous = pending();
    for (turn, id) in [("other", "tool"), ("wanted", "other"), ("wanted", "tool")] {
        item(turn, id, ItemStatus::Running);
        assert_eq!(pending()["request"].delivery, RequestDelivery::Awaiting);
    }
    router
        .claim_response(
            connection.id(),
            "request",
            &Answer::Approval {
                choice_id: request().body.choices()[0].id.clone(),
            },
        )
        .unwrap();
    assert_eq!(previous["request"].delivery, RequestDelivery::Awaiting);
    assert_eq!(pending()["request"].delivery, RequestDelivery::Sending);
    router.response_delivery("request", RequestDelivery::Unknown);
    router.response_delivery("request", RequestDelivery::Awaiting);
    assert_eq!(pending()["request"].delivery, RequestDelivery::Unknown);
    item("other", "tool", ItemStatus::Completed);
    assert!(pending().contains_key("request"));
    item("wanted", "tool", ItemStatus::Running);
    assert!(pending().is_empty());
    // Native identity indexes are released along with the normalized request.
    register().unwrap();
    item("wanted", "tool", ItemStatus::Completed);
    assert!(pending().is_empty());
    register().unwrap();
    lifecycle("wanted", true);
    assert!(pending().is_empty());
    assert!(register().is_err());
    assert_eq!(previous["request"].delivery, RequestDelivery::Awaiting);
}
#[tokio::test]
async fn history_is_not_retained_or_trimmed_and_subscriptions_do_not_pin_execution() {
    let router = SessionRouter::new();
    let connection = router.open_session();
    let mut turns: Vec<_> = (0..1001)
        .map(|id| {
            Arc::new(Turn {
                id: id.to_string().into(),
                ..Default::default()
            })
        })
        .collect();
    turns[0] = Arc::new(
        serde_json::from_value(json!({"id":"first","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(5 * 1024 * 1024),"phase":"unknown"}}}}}],"status":"unknown"}))
        .unwrap(),
    );
    let response = ThreadResponse {
        thread: Thread {
            id: Some(SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "native".into(),
            }),
            turns: Some(turns),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut response = router
        .finish_session_read(open(&router, "native"), connection.id(), response, false)
        .unwrap();
    let reply = agent_protocol::protocol::decode::<
        agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
    >(&response.initial)
    .unwrap()
    .into_value();
    assert_eq!(
        reply["result"]["response"]["thread"]["turns"]
            .as_array()
            .unwrap()
            .len(),
        1001
    );
    assert_eq!(
        reply["result"]["response"]["thread"]["turns"][0]["items"][0]["body"]["deferred"]["summary"]["assistantText"]["text"]
            .as_str()
            .unwrap()
            .len(),
        256
    );
    assert!(
        reply["result"]["response"]["thread"]["turns"][0]["items"][0]["body"]["deferred"]
            .is_object()
    );
    assert_no_execution(&router);
    let delta = SessionChange::Text {
        turn_id: "first".into(),
        item_id: "answer".into(),
        field: TextField::AssistantText,
        delta: " forwarded to native history".into(),
    };
    let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    router.session_change(&target, delta.clone());
    let frame = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        response.updates.as_mut().unwrap().recv(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(protocol::decode::<SessionChange>(&frame).unwrap(), delta);
    assert_no_execution(&router);
    turn(&router, false);
    assert_eq!(lock_state(&router.state).executions.len(), 1);
    turn(&router, true);
    let state = lock_state(&router.state);
    assert_eq!(state.subscriptions.len(), 1);
    drop(state);
    assert_no_execution(&router);
}

#[tokio::test]
async fn aggregate_item_bodies_are_deferred_to_fit_one_rpc() {
    let router = SessionRouter::new();
    let connection = router.open_session();
    let mut items: Vec<_> = (0..40).map(|id| json!({"id":id.to_string(),"status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(512 * 1024),"phase":"unknown"}}}}})).collect();
    items.push(
        json!({"id":"tool","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"z".repeat(8192),"exitCode":null}}}}}),
    );
    let response = serde_json::from_value(
        json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[{"id":"turn","items":items,"status":"unknown"}]}}),
    )
    .unwrap();
    let response = router
        .finish_session_read(open(&router, "native"), connection.id(), response, false)
        .unwrap();
    let line = response.initial;
    assert!(line.len() < 64 * 1024);
    let reply =
        protocol::decode::<protocol::Response<agent_protocol::session::OpenedSession>>(&line)
            .unwrap()
            .into_value();
    let turn = &reply["result"]["response"]["thread"]["turns"][0];
    assert_eq!(turn["items"].as_array().unwrap().len(), 41);
    assert_eq!(
        turn["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["body"]["deferred"].is_object())
            .count(),
        41
    );
    router.ensure_session(connection.id()).unwrap();
    assert_no_execution(&router);
}

#[tokio::test]
async fn oversized_rpc_returns_an_error_without_dropping_the_connection_or_subscribing() {
    let router = SessionRouter::new();
    let connection = router.open_session();
    for _ in 0..2 {
        let response = serde_json::from_value(
            json!({"thread":{"id":{"provider":"codex","id":"native"},"name":"x".repeat(MAX_QUEUED_BYTES + 1)}}),
        )
        .unwrap();
        let response = router
            .finish_session_read(open(&router, "native"), connection.id(), response, false)
            .unwrap();
        let reply = agent_protocol::protocol::decode::<
            agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
        >(&response.initial)
        .unwrap()
        .into_value();
        assert_eq!(reply["error"]["code"], "response_too_large");
        router.ensure_session(connection.id()).unwrap();
    }
    assert!(lock_state(&router.state).subscriptions.is_empty());
    assert_no_execution(&router);
}

#[tokio::test]
async fn completion_during_native_read_is_overlaid_before_live_updates() {
    let router = SessionRouter::new();
    let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let connection = router.open_session();
    let read = open(&router, "native");
    turn(&router, false);
    router.session_change(
        &target,
        SessionChange::Item {
            turn_id: "run".into(),
            item: Item::new(
                "answer".into(),
                agent_protocol::execution::ItemStatus::Running,
                agent_protocol::items::ItemBody::AssistantText {
                    text: "start".into(),
                    phase: agent_protocol::items::AssistantPhase::Unknown,
                },
            )
            .into(),
        },
    );
    router.session_change(
        &target,
        SessionChange::Text {
            turn_id: "run".into(),
            item_id: "answer".into(),
            field: TextField::AssistantText,
            delta: " final".into(),
        },
    );
    router.session_change(&target, SessionChange::Turn {
        turn: serde_json::from_value(json!({"id":"run","status":"completed","items":[{"id":"other","status":"completed","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"done","phase":"final"}}}}}]})).unwrap(),
        completed: true,
    });
    router.session_change(
        &target,
        SessionChange::Text {
            turn_id: "run".into(),
            item_id: "answer".into(),
            field: TextField::AssistantText,
            delta: " suffix".into(),
        },
    );
    let response = router
        .finish_session_read(
            read,
            connection.id(),
            serde_json::from_value(json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[{"id":"run","status":"running","items":[]}]}}))
            .unwrap(),
            false,
        )
        .unwrap();
    let reply = agent_protocol::protocol::decode::<
        agent_protocol::protocol::Response<agent_protocol::session::OpenedSession>,
    >(&response.initial)
    .unwrap()
    .into_value();
    let current = &reply["result"]["response"]["thread"]["turns"][0];
    assert_eq!(current["status"], "completed");
    assert_eq!(
        current["items"][0]["body"]["inline"]["body"]["assistantText"]["text"],
        "start final suffix"
    );
    assert_no_execution(&router);
}

#[test]
fn late_item_changes_update_the_correct_recorded_occurrence() {
    use agent_protocol::{
        execution::TurnStatus,
        items::{AssistantPhase, ItemBody},
    };
    for retained in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let transcripts = Arc::new(crate::host_rpc::store::ConversationStore::open(
            &directory.path().join("transcripts.sqlite"),
        ));
        let target = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
        let turn = |item: &str, status| Turn {
            id: "repeat".into(),
            status,
            items: Some(vec![Arc::new(Item::new(
                item.into(),
                Default::default(),
                ItemBody::AssistantText {
                    text: item.into(),
                    phase: AssistantPhase::Final,
                },
            ))]),
            ..Default::default()
        };
        transcripts
            .import(
                &target,
                transcripts.version(&target).unwrap(),
                ThreadResponse {
                    thread: Thread {
                        id: Some(target.clone()),
                        turns: Some(vec![
                            Arc::new(turn("older", TurnStatus::Completed)),
                            Arc::new(turn("latest", TurnStatus::Completed)),
                        ]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        let router = SessionRouter::new().with_store(transcripts.clone());
        let lease = retained.then(|| router.retain_execution(target.clone()).unwrap());
        if retained {
            router.session_change(
                &target,
                SessionChange::Turn {
                    turn: turn("older", TurnStatus::Completed),
                    completed: true,
                },
            );
        }
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: turn("latest", TurnStatus::Running),
                completed: false,
            },
        );
        router.session_change(
            &target,
            SessionChange::Item {
                turn_id: "repeat".into(),
                item: Arc::new(Item::new(
                    "older".into(),
                    Default::default(),
                    ItemBody::AssistantText {
                        text: "older full".into(),
                        phase: AssistantPhase::Final,
                    },
                )),
            },
        );
        router.session_change(
            &target,
            SessionChange::Text {
                turn_id: "repeat".into(),
                item_id: "older".into(),
                field: TextField::AssistantText,
                delta: " tail".into(),
            },
        );
        let history = transcripts.history(&target).unwrap().unwrap();
        let turns = history.thread.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 2);
        assert!(
            matches!(turns[0].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText {text, ..} if text == "older full tail")
        );
        assert!(
            matches!(turns[1].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText {text, ..} if text == "latest")
        );
        let live_item = router.item(&target, &"repeat".into(), &"older".into());
        assert_eq!(live_item.is_some(), retained);
        if let Some(item) = live_item {
            assert!(
                matches!(item.body(), ItemBody::AssistantText {text, ..} if text == "older full tail")
            );
        }
        router.session_change(
            &target,
            SessionChange::RemoveItem {
                turn_id: "repeat".into(),
                item_id: "older".into(),
            },
        );
        let history = transcripts.history(&target).unwrap().unwrap();
        let turns = history.thread.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns[0].items.as_ref().unwrap().is_empty());
        assert_eq!(turns[1].items.as_ref().unwrap()[0].id.as_str(), "latest");
        let live = router.current_turn(&target, "repeat").unwrap();
        assert_eq!(live.status, TurnStatus::Running);
        assert_eq!(live.items.as_ref().unwrap().len(), 1);
        assert_eq!(live.items.as_ref().unwrap()[0].id.as_str(), "latest");
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: turn("latest", TurnStatus::Completed),
                completed: true,
            },
        );
        // Repeated removal after completion must not append or replace a turn.
        router.session_change(
            &target,
            SessionChange::RemoveItem {
                turn_id: "repeat".into(),
                item_id: "older".into(),
            },
        );
        let history = transcripts.history(&target).unwrap().unwrap();
        let turns = history.thread.turns.as_ref().unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns[0].items.as_ref().unwrap().is_empty());
        assert_eq!(turns[1].items.as_ref().unwrap()[0].id.as_str(), "latest");
        drop(lease);
    }
}

#[test]
fn completion_does_not_discard_unconfirmed_input_and_accepted_receipts_replay() {
    let router = SessionRouter::new();
    let target = SessionRef {
        provider: ProviderKind::Codex,
        id: "native".into(),
    };
    router.begin_submission(&target, "send").unwrap();
    assert!(router.begin_submission(&target, "send").is_err());
    turn(&router, false);
    assert!(router.begin_submission(&target, "send").is_err());
    turn(&router, true);
    assert!(router.begin_submission(&target, "send").is_err());
    router.finish_submission(
        &target,
        "send",
        agent_protocol::session::SubmissionDelivery::Accepted {
            turn_id: Some("run".into()),
        },
    );
    assert_eq!(
        router
            .submission_receipt(&target, "send")
            .unwrap()
            .turn_id
            .as_deref(),
        Some("run")
    );
    let thread = router.overlay_execution(
        &target,
        serde_json::from_value(json!({"id":{"provider":"codex","id":"native"},"turns":[]}))
            .unwrap(),
    );
    assert_eq!(
        thread.submissions["send"],
        agent_protocol::session::SubmissionDelivery::Accepted {
            turn_id: Some("run".into())
        }
    );
}

#[test]
fn provider_exit_clears_activity_but_preserves_uncertain_delivery() {
    use agent_protocol::{models::SessionStatus, session::SubmissionDelivery};
    let router = SessionRouter::new();
    let target = SessionRef::new(
        agent_protocol::session::ProviderKind::Codex,
        "native".to_string(),
    )
    .unwrap();
    router.begin_submission(&target, "input").unwrap();
    router.session_change(
        &target,
        SessionChange::Status {
            status: SessionStatus::Running,
        },
    );
    assert_eq!(
        lock_state(&router.actor(&target)).timeline.status,
        SessionStatus::Running
    );
    router.fail_provider(ProviderKind::Codex, "provider stopped", &[]);
    let actor = router.actor(&target);
    let state = lock_state(&actor);
    let live = &state.timeline;
    assert_eq!(live.status, SessionStatus::Unknown);
    assert_eq!(live.submissions["input"], SubmissionDelivery::Unknown);
}

#[tokio::test]
async fn byte_budget_disconnects_only_the_slow_connection() {
    let router = SessionRouter::new();
    let slow = router.open_session();
    let mut healthy = router.open_session();
    router
        .send_frame(slow.id(), vec![0; MAX_QUEUED_BYTES - frame_cost(0)])
        .unwrap();
    assert!(router.send_frame(slow.id(), b"overflow".to_vec()).is_err());
    router
        .send_frame(healthy.id(), b"current".to_vec())
        .unwrap();
    assert_eq!(healthy.recv().await.as_deref(), Some(b"current".as_slice()));
}

#[tokio::test]
async fn burst_of_small_updates_keeps_the_connection_and_delivers_every_update() {
    let router = SessionRouter::new();
    let initial = router.task_activity(
        vec![Thread {
            id: Some(SessionRef::new(ProviderKind::Codex, "native".into()).unwrap()),
            ..Default::default()
        }],
        0,
    );
    let mut connection = router.open_session();
    let response: ThreadResponse = serde_json::from_value(
        json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
    )
    .unwrap();
    let mut subscription = router
        .finish_session_read(open(&router, "native"), connection.id(), response, false)
        .unwrap()
        .updates
        .unwrap();
    // A provider can deliver a buffered burst before the network writer is scheduled.
    // Both notification streams remain well within the connection's memory budget.
    for index in 0..512 {
        router.session_change(
            &SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "native".to_string(),
            )
            .unwrap(),
            SessionChange::Status {
                status: if index % 2 == 0 {
                    agent_protocol::models::SessionStatus::Running
                } else {
                    agent_protocol::models::SessionStatus::Idle
                },
            },
        );
    }
    router
        .ensure_session(connection.id())
        .expect("a small burst must not disconnect the client");
    for index in 0..512 {
        let change =
            protocol::decode::<SessionChange>(&subscription.recv().await.unwrap()).unwrap();
        let SessionChange::Status { status } = change else {
            panic!("expected status")
        };
        assert_eq!(
            status == agent_protocol::models::SessionStatus::Running,
            index % 2 == 0
        );
        let activity = protocol::decode::<Notification>(&connection.recv().await.unwrap()).unwrap();
        let Notification::Activity { active, .. } = activity else {
            panic!("expected activity")
        };
        assert_eq!(active, index % 2 == 0);
        let activity = protocol::decode::<Notification>(&connection.recv().await.unwrap()).unwrap();
        let Notification::TaskActivity { state } = activity else {
            panic!("expected task state")
        };
        assert_eq!(state.display.ongoing, active);
        assert_eq!(state.revision, initial.revision + index + 1);
    }
    assert_eq!(
        connection
            .queued_bytes
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn slow_session_subscribers_do_not_block_other_devices_or_destroy_current_state() {
    let router = SessionRouter::new();
    let slow = router.open_session();
    let mut healthy = router.open_session();
    let target = SessionRef::new(
        agent_protocol::session::ProviderKind::Codex,
        "native".to_string(),
    )
    .unwrap();
    let _execution = open(&router, "native");
    let response: ThreadResponse = serde_json::from_value(
        json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
    )
    .unwrap();
    let mut streams = Vec::new();
    for connection in [slow.id(), healthy.id()] {
        streams.push(
            router
                .finish_session_read(open(&router, "native"), connection, response.clone(), false)
                .unwrap()
                .updates
                .unwrap(),
        );
    }
    let change = SessionChange::Turn {
        completed: false,
        turn: serde_json::from_value(json!({"id":"turn","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"x".repeat(1024 * 1024),"phase":"unknown"}}}}}],"status":"unknown"}))
        .unwrap(),
    };
    for _ in 0..20 {
        router.session_change(&target, change.clone());
        assert!(streams[1].recv().await.is_some());
        assert!(healthy.recv().await.is_some());
    }
    assert!(router.ensure_session(slow.id()).is_err());
    router.ensure_session(healthy.id()).unwrap();
    while streams[0].recv().await.is_some() {}
    assert!(router.current_turn(&target, "turn").is_some());
}

#[test]
fn subscriptions_share_the_connection_budget_and_drop_releases_unread_bytes() {
    let router = SessionRouter::new();
    let connection = router.open_session();
    let response: ThreadResponse = serde_json::from_value(
        json!({"thread":{"id":{"provider":"codex","id":"native"},"turns":[]}}),
    )
    .unwrap();
    let first = router
        .finish_session_read(
            open(&router, "native"),
            connection.id(),
            response.clone(),
            false,
        )
        .unwrap()
        .updates
        .unwrap();
    let second = router
        .finish_session_read(open(&router, "native"), connection.id(), response, false)
        .unwrap()
        .updates
        .unwrap();
    let budget = lock_state(&router.state).sessions[&connection.id()]
        .bytes
        .clone();
    let actor = router.actor(&SessionRef::new(ProviderKind::Codex, "native".into()).unwrap());
    let (a, b) = {
        let actor = lock_state(&actor);
        (
            actor.subscriptions[&first.id].1.clone(),
            actor.subscriptions[&second.id].1.clone(),
        )
    };
    a.try_send(connection.id(), Vec::with_capacity(MAX_QUEUED_BYTES / 2))
        .unwrap();
    assert!(
        b.try_send(connection.id(), vec![0; MAX_QUEUED_BYTES / 2 + 1])
            .is_err()
    );
    drop(first);
    assert!(matches!(
        a.try_send(connection.id(), vec![]),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
    assert_eq!(budget.load(std::sync::atomic::Ordering::Relaxed), 0);
    b.try_send(connection.id(), vec![0; MAX_QUEUED_BYTES / 2 + 1])
        .unwrap();
    drop(second);
    assert_eq!(budget.load(std::sync::atomic::Ordering::Relaxed), 0);
    router.ensure_session(connection.id()).unwrap();
}

#[test]
fn cancellation_and_disconnect_release_an_unfinished_open() {
    let router = SessionRouter::new();
    let connection = router.open_session();
    let read = router
        .retain_execution(
            SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "native".to_string(),
            )
            .unwrap(),
        )
        .unwrap();
    drop(connection);
    drop(read);
    // Fill and evict idle entries: leaked leases would eventually prevent admission.
    for id in 0..200 {
        let read = router
            .retain_execution(
                SessionRef::new(agent_protocol::session::ProviderKind::Codex, id.to_string())
                    .unwrap(),
            )
            .unwrap();
        drop(read);
    }
}

#[test]
fn identical_native_request_ids_keep_their_source_instance() {
    use agent_protocol::requests::Answer;
    let router = SessionRouter::new();
    let connection = router.open_session();
    let native = serde_json::json!("claude-permission:shared-native-id");
    let (input, _receiver) = tokio::sync::mpsc::channel(1);
    let mut requests = Vec::new();
    for provider in [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::Codex,
    ] {
        let instance = uuid::Uuid::new_v4();
        let target = SessionRef::new(provider, "shared-native-session".into()).unwrap();
        router.session_change(
            &target,
            SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "turn".into(),
                    ..Default::default()
                },
                completed: false,
            },
        );
        let adapted = if provider == ProviderKind::Codex {
            crate::providers::requests::codex(
                uuid::Uuid::new_v4().to_string().into(),
                "item/commandExecution/requestApproval",
                &serde_json::json!({"turnId":"turn","availableDecisions":["accept","decline"]}),
            )
            .unwrap()
        } else {
            crate::providers::requests::claude(
                uuid::Uuid::new_v4().to_string().into(),
                &"turn".into(),
                &crate::providers::claude::SdkRequest::Tool {
                    tool_name: "Bash".into(),
                    tool_use_id: Some("tool".into()),
                    input: serde_json::json!({"command":"pwd"}),
                },
            )
            .unwrap()
        };
        let request_id = adapted.request.id.clone();
        let choice_id = adapted.request.body.choices()[0].id.clone();
        let origin = if provider == ProviderKind::Codex {
            crate::providers::unavailable_origin(instance, native.clone(), Default::default())
        } else {
            crate::providers::claude::request_origin(
                instance,
                native.clone(),
                input.clone(),
                adapted.answers,
            )
        };
        router
            .request(target.clone(), origin, adapted.request)
            .unwrap();
        let (origin, _) = router
            .claim_response(
                connection.id(),
                &request_id,
                &Answer::Approval { choice_id },
            )
            .unwrap();
        assert_eq!(origin.instance, instance);
        assert_eq!(origin.native_id, native);
        requests.push((instance, target, request_id));
    }
    let malformed =
        serde_json::json!({"id":1,"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    assert!(
        crate::providers::codex::event_change(
            requests[0].0,
            &RpcMessage::parse(&malformed).unwrap()
        )
        .is_err()
    );
    let resolved =
        serde_json::json!({"method":"serverRequest/resolved","params":{"requestId":native}})
            .to_string();
    let change = crate::providers::codex::event_change(
        requests[0].0,
        &RpcMessage::parse(&resolved).unwrap(),
    )
    .unwrap()
    .unwrap();
    router.apply(change, &[]).unwrap();
    assert!(router.request_session(&requests[0].2).is_none());
    for (_, target, id) in &requests[1..] {
        assert_eq!(router.request_session(id).as_ref(), Some(target));
    }
}
