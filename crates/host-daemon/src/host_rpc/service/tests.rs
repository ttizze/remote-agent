#[tokio::test]
async fn provider_switches_broadcast_and_gate_new_work_while_retaining_history_access() {
    use super::super::runtime::native_fixture::{NativeAgent, call};
    use super::*;
    use agent_protocol::{
        protocol::Notification,
        providers::{ProviderSettings, SetProviderEnabled},
    };
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let host = HostRpcService::new([agent.backend()], directory.path().join("worktrees.json"));
    let client = host.open_session();
    let mut other = host.open_session();
    let settings: ProviderSettings = call(
        &host,
        client.id(),
        Call::SetProviderEnabled(SetProviderEnabled {
            provider: ProviderKind::Codex,
            enabled: false,
        }),
    )
    .await
    .unwrap();
    assert!(!settings.enabled(ProviderKind::Codex));
    let notified = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let message =
                agent_protocol::protocol::decode::<Notification>(&other.recv().await.unwrap())
                    .unwrap();
            if let Notification::ProviderSettings { settings } = message {
                break settings;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(notified, settings);
    let read: ProviderSettings = call(
        &host,
        other.id(),
        Call::ReadProviderSettings(agent_protocol::models::Empty {}),
    )
    .await
    .unwrap();
    assert_eq!(read, settings);
    assert_eq!(
        host.creation_agent(ProviderKind::Codex, None)
            .err()
            .unwrap()
            .code,
        "provider_disabled"
    );
    assert!(
        host.inner.providers.get(ProviderKind::Codex).is_ok(),
        "history and running work keep their provider"
    );
    let models: op::ModelPage = call(
        &host,
        other.id(),
        Call::ListModels(op::ListModels {
            provider: ProviderKind::Codex,
            limit: 100,
            cursor: None,
        }),
    )
    .await
    .unwrap();
    assert!(models.data.is_empty());
    assert!(models.provider_errors.is_none());
    let enabled: ProviderSettings = call(
        &host,
        other.id(),
        Call::SetProviderEnabled(SetProviderEnabled {
            provider: ProviderKind::Codex,
            enabled: true,
        }),
    )
    .await
    .unwrap();
    assert!(enabled.enabled(ProviderKind::Codex));
    assert!(host.creation_agent(ProviderKind::Codex, None).is_ok());
}

#[tokio::test]
async fn file_failures_are_classified_without_losing_visualization_recovery() {
    use super::super::runtime::native_fixture::call;
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let service = HostRpcService::new(
        std::iter::empty::<Backend>(),
        root.path().join("worktrees.json"),
    );
    let session = service.open_session();
    let missing = root.path().join("missing.mp4").to_str().unwrap().to_owned();
    for request in [
        Call::ResolveFile(op::ResolveFile {
            path: missing.clone(),
        }),
        Call::ReadFile(op::ListFiles {
            path: missing.clone(),
        }),
        Call::Download(op::ListFiles { path: missing }),
    ] {
        let failure = call::<serde_json::Value>(&service, session.id(), request)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "file_not_found");
    }
    let failure = call::<String>(
        &service,
        session.id(),
        Call::ReadVisualization(op::LoadVisualization {
            path: "missing.html".into(),
            cwd: root.path().to_str().unwrap().into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.code, "file_operation_failed");
    assert!(
        failure
            .message
            .contains("表示ファイルが見つかりません。ホストでHTMLを再作成してください。")
    );
}

#[tokio::test]
async fn account_providers_are_read_in_parallel_without_blocking_conversation_lists() {
    use super::super::runtime::native_fixture::{NativeAgent, call};
    use super::*;
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let b = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
    a.accounts_gate.forget_permits(1);
    b.accounts_gate.forget_permits(1);
    let host = HostRpcService::new(
        [a.backend(), b.backend()],
        directory.path().join("worktrees.json"),
    );
    let client = host.open_session();
    let mut accounts = Box::pin(call::<op::Accounts>(
        &host,
        client.id(),
        Call::ListAccounts(agent_protocol::models::Empty {}),
    ));
    tokio::select! {
        result = &mut accounts => panic!("withheld accounts returned early: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            a.accounts_started.notified().await;
            b.accounts_started.notified().await;
        }) => result.expect("both providers must start before either one finishes"),
    }
    let list = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        call::<agent_protocol::conversation::records::OpenedList>(
            &host,
            client.id(),
            Call::SubscribeList(agent_protocol::conversation::records::SubscribeList {
                refresh: false,
                subscription: uuid::Uuid::new_v4(),
                cached: None,
            }),
        ),
    )
    .await
    .expect("conversation lists must not wait for accounts")
    .unwrap();
    agent_protocol::conversation::records::validate_list(&list.initial).unwrap();
    a.accounts_gate.add_permits(1);
    b.accounts_gate.add_permits(1);
    assert!(accounts.await.unwrap().errors.is_empty());
}

#[tokio::test]
async fn account_read_failures_keep_their_provider_alongside_startup_errors() {
    use super::super::runtime::native_fixture::{NativeAgent, call};
    use super::*;
    use std::sync::atomic::Ordering;
    let directory = tempfile::tempdir().unwrap();
    let a = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
    let host = HostRpcService::new(
        [
            Backend::unavailable(ProviderKind::DeepSeek, "deepseek startup failed"),
            Backend::unavailable(ProviderKind::Claude, "claude startup failed"),
            a.backend(),
        ],
        directory.path().join("worktrees.json"),
    );
    let client = host.open_session();

    a.fail_accounts.store(1, Ordering::SeqCst);
    let accounts: op::Accounts = call(
        &host,
        client.id(),
        Call::ListAccounts(agent_protocol::models::Empty {}),
    )
    .await
    .unwrap();
    assert_eq!(
        accounts.errors,
        [
            (ProviderKind::Codex, "codex accounts failed".into()),
            (ProviderKind::Claude, "claude startup failed".into()),
            (ProviderKind::DeepSeek, "deepseek startup failed".into()),
        ]
        .into()
    );
}

#[tokio::test]
async fn failed_model_providers_remain_identifiable_when_no_catalog_is_available() {
    use super::*;
    let root = tempfile::tempdir().unwrap();
    let providers = [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::DeepSeek,
    ];
    let service = HostRpcService::new(
        providers.map(|provider| Backend::unavailable(provider, "offline")),
        root.path().join("worktrees.json"),
    );
    let session = service.open_session();
    for provider in providers {
        let response = service
            .dispatch(
                session.id(),
                &Call::ListModels(op::ListModels {
                    provider,
                    cursor: None,
                    limit: 100,
                }),
            )
            .await
            .unwrap();
        let Response::Success { result: page } =
            agent_protocol::protocol::decode::<Response<op::ModelPage>>(&response.initial)
                .unwrap()
        else {
            panic!("catalog failures must retain each provider's error");
        };
        assert!(page.data.is_empty());
        assert!(page.next_cursor.is_none());
        let errors = page.provider_errors.unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[provider.key()]["code"], "provider_unavailable");
    }
}

#[tokio::test]
async fn successful_configuration_clears_the_provider_startup_error() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::NativeAgent;
    let root = tempfile::tempdir().unwrap();
    let service = HostRpcService::new(
        [Backend::unavailable(
            ProviderKind::Claude,
            "invalid provider configuration",
        )],
        root.path().join("worktrees.json"),
    );
    assert_eq!(
        service.provider_errors()["claude"]["code"],
        "provider_unavailable"
    );
    let agent = NativeAgent::new(ProviderKind::Claude, root.path().join("claude"));
    let service = HostRpcService::new([agent.backend()], root.path().join("worktrees.json"));
    assert!(service.provider_errors().get("claude").is_none());
    let session = service.open_session();
    let response = service
        .dispatch(
            session.id(),
            &Call::CreateConversation(agent_protocol::conversation::records::Create {
                workspace: None,
                provider: agent_protocol::session::ProviderKind::Claude,
                session: agent_protocol::session::SessionRef::new(
                    None,
                    uuid::Uuid::new_v4().to_string(),
                )
                .unwrap(),
                subscription: uuid::Uuid::new_v4(),
                cwd: Some(root.path().to_string_lossy().into_owned()),
                selection: Default::default(),
                initial: None,
            }),
        )
        .await
        .unwrap();
    let Response::Success { result: response } = agent_protocol::protocol::decode::<
        Response<agent_protocol::conversation::records::Opened>,
    >(&response.initial)
    .unwrap() else {
        panic!("provider did not recover");
    };
    assert_eq!(response.session.provider, None);
    let header = service
        .inner
        .conversation
        .storage
        .header(&response.session)
        .unwrap();
    assert_eq!(header.state.provider, ProviderKind::Claude);
}

#[test]
fn unknown_and_native_methods_are_rejected_before_dispatch() {
    for method in [
        "not/public",
        "provider",
        "thread/list",
        "thread/read",
        "thread/resume",
        "turn/start",
        "turn/steer",
        "thread/queue/add",
        "account/read",
    ] {
        for params in [
            serde_json::json!({}),
            serde_json::json!({"threadId":{"provider":"claude","id":"native"}}),
        ] {
            assert!(
                agent_protocol::protocol::json_boundary::call(method, params).is_err(),
                "{method}"
            );
        }
    }
}
fn stored_header(
    provider: agent_protocol::session::ProviderKind,
    cwd: &std::path::Path,
) -> agent_protocol::conversation::records::Header {
    agent_protocol::conversation::records::Header {
        state: agent_protocol::conversation::Thread {
            name: String::new(),
            workspace: agent_protocol::conversation::Workspace::Ready(
                cwd.to_string_lossy().into_owned(),
            ),
            provider,
            queue_held: false,
        },
        details: Default::default(),
    }
}
#[tokio::test]
async fn host_names_do_not_depend_on_provider_availability_and_failed_input_is_not_in_flight() {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::call;
    use agent_protocol::conversation::{Command, InputContent, records};
    let root = tempfile::tempdir().unwrap();
    let service = HostRpcService::new(
        [Backend::unavailable(ProviderKind::Codex, "unavailable")],
        root.path().join("projects.json"),
    );
    let target = agent_protocol::session::SessionRef::new(
        ProviderKind::Codex,
        uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    service
        .inner
        .conversation
        .storage
        .create(&target, &stored_header(ProviderKind::Codex, root.path()))
        .unwrap();
    let session = service.open_session();
    let renamed: records::Acknowledged = call(
        &service,
        session.id(),
        Call::ChangeConversation(records::Change {
            session: target.clone(),
            operation: uuid::Uuid::new_v4(),
            command: Command::Rename("Renamed".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(renamed.error, None);
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .header(&target)
            .unwrap()
            .state
            .name,
        "Renamed"
    );
    let input = uuid::Uuid::new_v4();
    let request = Call::ChangeConversation(records::Change {
        session: target.clone(),
        operation: input,
        command: Command::Submit {
            id: input.to_string().into(),
            content: InputContent {
                parts: vec![op::Input::Text {
                    text: "unsent".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        },
    });
    for _ in 0..2 {
        let error = call::<records::Acknowledged>(&service, session.id(), request.clone())
            .await
            .unwrap()
            .accepted()
            .unwrap_err();
        assert_eq!(error.code, "command_rejected");
        assert_eq!(error.delivery, agent_protocol::error::Delivery::NotSent);
        assert!(
            service
                .inner
                .conversation
                .storage
                .state(&target)
                .unwrap()
                .inputs
                .is_empty()
        );
    }
}

#[tokio::test]
async fn enabling_host_owned_harness_keeps_the_existing_draft_namespace() {
    let root = tempfile::tempdir().unwrap();
    let claude = crate::providers::claude::Claude::load(
        "claude".into(),
        root.path().join("accounts"),
        Some(root.path().join("native")),
    )
    .await
    .unwrap();
    let service =
        super::HostRpcService::new([claude.into()], root.path().join("projects.json"));
    let before = service.storage_scope().unwrap();
    assert_eq!(before, service.inner.providers.storage_scope().unwrap());
    let claude = crate::providers::claude::Claude::load(
        "claude".into(),
        root.path().join("accounts"),
        Some(root.path().join("native")),
    )
    .await
    .unwrap();
    let harness = crate::providers::deepseek::DeepSeek::load(
        "dsh".into(),
        root.path().join("harness"),
        Some(root.path().join("home")),
        None,
    )
    .await
    .unwrap();
    let service = super::HostRpcService::new(
        [claude.into(), harness.into()],
        root.path().join("projects.json"),
    );
    let after = service.storage_scope().unwrap();
    assert_eq!(after, service.inner.providers.storage_scope().unwrap());
    assert_eq!(
        before, after,
        "Adding Harness must not archive existing drafts or navigation"
    );
}
#[tokio::test]
async fn conversation_rpc_failures_are_logged_once_without_request_payloads() {
    use crate::HostRpcService;
    use agent_protocol::protocol::{Call, Response};
    use tracing::instrument::WithSubscriber;
    let directory = tempfile::tempdir().unwrap();
    let service = HostRpcService::new([], directory.path().join("projects.json"));
    service.set_history_scope(directory.path().to_owned(), true);
    service.prepare_update().await.unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let file = log.reopen().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || file.try_clone().unwrap())
        .finish();
    let client = service.open_session();
    let input = uuid::Uuid::new_v4();
    let request = Call::ChangeConversation(agent_protocol::conversation::records::Change {
        session: agent_protocol::session::SessionRef::new(
            None,
            uuid::Uuid::new_v4().to_string(),
        )
        .unwrap(),
        operation: input,
        command: agent_protocol::conversation::Command::Submit {
            id: input.to_string().into(),
            content: agent_protocol::conversation::InputContent {
                parts: vec![agent_protocol::operations::Input::Text {
                    text: "private request title".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        },
    });
    let reply = service
        .dispatch(client.id(), &request)
        .with_subscriber(subscriber)
        .await
        .unwrap();
    let Response::Failure { error } = agent_protocol::protocol::decode::<
        Response<agent_protocol::models::Empty>,
    >(&reply.initial)
    .unwrap() else {
        panic!("updating must reject the write");
    };
    assert_eq!(error.code, "host_updating");
    let text = std::fs::read_to_string(log.path()).unwrap();
    assert_eq!(text.matches("host/conversation/change").count(), 1);
    assert!(text.contains("host_updating"));
    assert!(!text.contains("private request title"));
}
