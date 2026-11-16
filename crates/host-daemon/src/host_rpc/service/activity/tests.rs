use super::*;

#[test]
fn startup_reports_invalid_storage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(crate::CONVERSATION_DATABASE_FILE),
        b"invalid",
    )
    .unwrap();
    let service = HostRpcService::new([], root.path().join("projects.json"));
    assert!(service.start().unwrap_err().contains("not a database"));
}
use agent_protocol::{
    conversation::records,
    live_activity::{MarkSessionRead, TaskActivityState},
    session::SessionRef,
};
struct ListReader {
    updates: crate::host_rpc::routing::HostSubscription,
    shell: agent_protocol::orchestration::shell::ShellSnapshot,
}
async fn open_list(service: &HostRpcService, connection: u64) -> ListReader {
    let reply = service
        .dispatch(
            connection,
            &Call::SubscribeList(records::SubscribeList {
                refresh: false,
                subscription: uuid::Uuid::new_v4(),
                after_sequence: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result } =
        agent_protocol::protocol::decode::<Response<records::OpenedList>>(&reply.initial).unwrap()
    else {
        panic!("list subscription failed")
    };
    let mut shell = None;
    for item in result.initial {
        shell = agent_core::orchestration::shell::receive(shell.as_ref(), item, true).unwrap();
    }
    ListReader {
        updates: reply.updates.unwrap(),
        shell: shell.unwrap(),
    }
}
async fn next_activity(reader: &mut ListReader) -> TaskActivityState {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let message =
            agent_protocol::protocol::decode(&reader.updates.recv().await.unwrap()).unwrap();
        reader.shell =
            agent_core::orchestration::shell::receive(Some(&reader.shell), message, false)
                .unwrap()
                .unwrap();
        agent_protocol::orchestration::shell::activity(
            reader.shell.threads.iter().map(AsRef::as_ref),
        )
        .unwrap()
    })
    .await
    .expect("committed list update arrives")
}

#[cfg(unix)]
#[tokio::test]
async fn activity_uses_only_normal_conversations_even_with_saved_subagent_completions() {
    use crate::{
        host_rpc::runtime::{Runtime, native_fixture::NativeAgent, t3_contract::running},
        providers::AgentChange,
    };
    use agent_protocol::{
        models::{SessionStatus, Turn, TurnStatus},
        session::SessionChange,
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let (root, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    Runtime::observe(
        &service,
        AgentChange::TaskParent {
            session: child.clone(),
            parent: native.clone(),
        },
        Some(execution),
    )
    .await
    .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: child.clone(),
            change: SessionChange::Turn {
                turn: Turn {
                    id: "child-turn".into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let activity = service.inner.conversation.storage.activity().unwrap();
    assert_eq!(
        activity.statuses,
        vec![(root.clone(), SessionStatus::Running)]
    );
    assert!(activity.unread.is_empty());
    Runtime::observe(
        &service,
        AgentChange::Session {
            session: native,
            change: SessionChange::Turn {
                turn: Turn {
                    id: "observed-turn".into(),
                    status: TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    let activity = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let activity = service.inner.conversation.storage.activity().unwrap();
            if activity.statuses == vec![(root.clone(), SessionStatus::Idle)] {
                break activity;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(activity.statuses, vec![(root.clone(), SessionStatus::Idle)]);
    assert_eq!(activity.unread.len(), 1);
    assert_eq!(activity.unread[0].0, root);
    assert!(
        service
            .inner
            .conversation
            .storage
            .thread_header(&child)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn reading_on_either_device_broadcasts_to_both_and_reconnect_reads_the_shared_state() {
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, call};
    use agent_protocol::conversation::{Command, InputContent, records};
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let phone = service.open_session();
    let desktop = service.open_session();
    let mut phone_list = open_list(&service, phone.id()).await;
    let mut desktop_list = open_list(&service, desktop.id()).await;
    let id = session_uuid();
    let _: records::Opened = call(
        &service,
        phone.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            provider: agent_protocol::session::ProviderKind::Codex,
            session: id.clone(),
            subscription: uuid::Uuid::new_v4(),
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            selection: Default::default(),
            initial: None,
        }),
    )
    .await
    .unwrap();
    for reader in [phone.id(), desktop.id()] {
        let input = uuid::Uuid::new_v4();
        let _: records::Acknowledged = call(
            &service,
            reader,
            Call::ChangeConversation(records::Change {
                session: id.clone(),
                operation: input,
                command: Command::Submit {
                    id: input.to_string().into(),
                    content: InputContent {
                        fresh_context: false,
                        parts: vec![op::Input::Text {
                            text: "complete".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            }),
        )
        .await
        .unwrap();
        let mut completed = None;
        for connection in [&mut phone_list, &mut desktop_list] {
            loop {
                let update = next_activity(connection).await;
                if let Some((_, turn)) = update.unread.iter().find(|(session, _)| session == &id) {
                    completed = Some(turn.clone());
                    break;
                }
            }
        }
        let _: agent_protocol::models::Empty = call(
            &service,
            reader,
            Call::MarkSessionRead(MarkSessionRead {
                session: id.clone(),
                turn_id: completed.unwrap(),
            }),
        )
        .await
        .unwrap();
        for connection in [&mut phone_list, &mut desktop_list] {
            loop {
                if next_activity(connection).await.unread.is_empty() {
                    break;
                }
            }
        }
    }
    let restarted = HostRpcService::new([], directory.path().join("projects.json"));
    let reconnected = restarted.open_session();
    let list = open_list(&restarted, reconnected.id()).await;
    let state = restarted.inner.conversation.storage.activity().unwrap();
    let cursor = restarted
        .inner
        .conversation
        .storage
        .shell_snapshot()
        .unwrap()
        .snapshot_sequence;
    assert_eq!(list.shell.snapshot_sequence, cursor);
    assert!(state.unread.is_empty());
}
fn session_uuid() -> SessionRef {
    SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap()
}

#[tokio::test]
async fn live_activity_registration_accepts_both_environments_and_rejects_malformed_tokens() {
    use super::*;
    use agent_protocol::live_activity::{
        LiveActivityRegistration, PushEnvironment, RegisterLiveActivity,
    };
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new(
        [crate::providers::codex::Codex::new(Err("provider is offline".into())).into()],
        directory.path().join("projects.json"),
    );
    let connection = service.open_session();
    let mut params = RegisterLiveActivity {
        activity_id: Some("activity".into()),
        allow_start: false,
        token: vec![1; 32],
        environment: PushEnvironment::Sandbox,
    };
    // One Host serves store and directly installed development builds of the same app.
    for (configured, environment) in [
        (false, PushEnvironment::Sandbox),
        (true, PushEnvironment::Sandbox),
        (true, PushEnvironment::Production),
    ] {
        if configured {
            service
                .inner
                .router
                .set_apns(Some(crate::apns::Apns::testing()));
        }
        params.environment = environment;
        let response = service
            .dispatch(connection.id(), &Call::RegisterLiveActivity(params.clone()))
            .await
            .unwrap();
        let Response::Success { result } = agent_protocol::protocol::decode::<
            Response<LiveActivityRegistration>,
        >(&response.initial)
        .unwrap() else {
            panic!("unconfigured APNs must keep local updates available");
        };
        assert_eq!(result.enabled, configured);
    }
    params.token.clear();
    let response = service
        .dispatch(connection.id(), &Call::RegisterLiveActivity(params))
        .await
        .unwrap();
    assert!(matches!(
        agent_protocol::protocol::decode::<Response<LiveActivityRegistration>>(&response.initial)
            .unwrap(),
        Response::Failure { .. }
    ));
}
#[tokio::test]
async fn live_activity_tracks_committed_work_after_disconnect_and_revocation_prevents_reregistration()
 {
    use crate::host_rpc::runtime::native_fixture::call;
    use agent_protocol::live_activity::{
        LiveActivityRegistration, PushEnvironment, RegisterLiveActivity,
    };
    let directory = tempfile::tempdir().unwrap();
    use crate::host_rpc::runtime::{Runtime, native_fixture::NativeAgent, t3_contract::running};
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let apns = crate::apns::Apns::testing();
    service.inner.router.set_apns(Some(apns.clone()));
    let (_id, native, execution) =
        running(&service, &agent, directory.path().to_str().unwrap()).await;
    service.start().unwrap();
    apns.activate(
        service
            .inner
            .conversation
            .storage
            .activity()
            .unwrap()
            .display,
    );
    let phone = service.open_authenticated_session("phone".into());
    let params = RegisterLiveActivity {
        activity_id: Some("activity".into()),
        allow_start: false,
        token: vec![1; 32],
        environment: PushEnvironment::Sandbox,
    };
    let registration: LiveActivityRegistration = call(
        &service,
        phone.id(),
        Call::RegisterLiveActivity(params.clone()),
    )
    .await
    .unwrap();
    assert!(registration.enabled);
    let payload = apns.test_payloads(crate::apns::now()).pop().unwrap();
    assert_eq!(
        payload["aps"]["content-state"]["display"]["current"]["label"],
        "実行中 1件"
    );
    service.close_session(phone.id());
    assert_eq!(apns.test_payloads(crate::apns::now()).len(), 1);
    Runtime::observe(
        &service,
        crate::providers::AgentChange::Session {
            session: native,
            change: agent_protocol::session::SessionChange::Turn {
                turn: agent_protocol::models::Turn {
                    id: "observed-turn".into(),
                    status: agent_protocol::models::TurnStatus::Completed,
                    ..Default::default()
                },
                completed: true,
            },
        },
        Some(execution),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if apns
                .test_payloads(crate::apns::now())
                .iter()
                .any(|payload| payload["aps"]["event"] == "end")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("committed completion reaches APNs without a connected phone");
    let reconnected = service.open_authenticated_session("phone".into());
    service.revoke_device("phone");
    assert!(apns.test_payloads(crate::apns::now()).is_empty());
    assert!(
        service
            .register_live_activity(reconnected.id(), &params)
            .is_err()
    );
    assert!(apns.test_payloads(crate::apns::now()).is_empty());
}
