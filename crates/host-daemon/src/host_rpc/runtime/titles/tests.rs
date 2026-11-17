use super::super::native_fixture::{NativeAgent, call};
use super::*;
use agent_protocol::{conversation::InputContent, protocol::Call};

const REQUEST: &str = "タスク名が入力の冒頭のままなので、内容を要約した名前に直してほしい";
const TITLE: &str = "内容からタスク名を生成";

fn content(provider: ProviderKind, text: &str) -> InputContent {
    InputContent {
        parts: vec![op::Input::Text { text: text.into() }],
        model: Some(agent_protocol::models::ModelRef {
            provider,
            id: "fixture".into(),
        }),
        fresh_context: false,
        effort: None,
        service_tier: None,
        follow_up: Default::default(),
    }
}

async fn create(
    service: &HostRpcService,
    provider: ProviderKind,
    cwd: &str,
    initial: bool,
) -> SessionRef {
    let connection = service.open_session();
    let session = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        service,
        connection.id(),
        Call::CreateConversation(records::Create {
            session: session.clone(),
            provider,
            subscription: Uuid::new_v4(),
            cwd: Some(cwd.into()),
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            selection: Default::default(),
            initial: initial.then(|| {
                (
                    Uuid::new_v4().to_string().into(),
                    content(provider, REQUEST),
                )
            }),
        }),
    )
    .await
    .unwrap();
    if !initial {
        change(
            service,
            &session,
            Command::Submit {
                id: Uuid::new_v4().to_string().into(),
                content: content(provider, REQUEST),
            },
        )
        .await;
    }
    session
}

async fn change(service: &HostRpcService, session: &SessionRef, command: Command) {
    let connection = service.open_session();
    let ack: records::Acknowledged = call(
        service,
        connection.id(),
        Call::ChangeConversation(records::Change {
            session: session.clone(),
            operation: Uuid::new_v4(),
            command,
        }),
    )
    .await
    .unwrap();
    assert!(ack.error.is_none(), "{:?}", ack.error);
}

async fn wait_for(
    service: &HostRpcService,
    session: &SessionRef,
    ready: impl Fn(&model::State) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let state = service
                .inner
                .conversation
                .storage
                .thread_state(&session.id, &[])
                .unwrap();
            if ready(&state) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn generated_titles_reach_live_streams_search_and_cold_lists_for_both_creation_paths() {
    for provider in [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::DeepSeek,
    ] {
        for initial in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let agent = NativeAgent::new(provider);
            *agent.generated_title.lock().unwrap() = Some(TITLE.into());
            let gate = agent.title_gate.acquire().await.unwrap();
            let service =
                HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
            let session = create(
                &service,
                provider,
                directory.path().to_str().unwrap(),
                initial,
            )
            .await;
            let storage = &service.inner.conversation.storage;
            let stream = storage.thread_stream(&session, None).unwrap();
            let sequence = stream
                .iter()
                .find_map(|item| match item {
                    model::projection::ThreadStreamItem::Snapshot(snapshot) => {
                        Some(snapshot.snapshot_sequence)
                    }
                    _ => None,
                })
                .unwrap();
            drop(gate);
            wait_for(&service, &session, |state| state.thread.title == TITLE).await;
            let updates = storage.thread_stream(&session, Some(sequence)).unwrap();
            assert!(updates.iter().any(|item| matches!(item, model::projection::ThreadStreamItem::Event {event,..} if matches!(&event.change, model::Change::Thread(thread) if thread.title == TITLE))));
            let prompts = agent.title_prompts.lock().unwrap();
            assert_eq!(prompts.len(), 1);
            assert!(prompts[0].contains(REQUEST));
            drop(prompts);
            let reopened = Storage::open(
                &directory
                    .path()
                    .join(crate::host_rpc::storage::CONVERSATION_DATABASE_FILE),
            );
            let (threads, _, _) = reopened.search_threads(TITLE, 10).unwrap();
            assert_eq!(threads.len(), 1);
            assert_eq!(threads[0].name.as_deref(), Some(TITLE));
            assert_eq!(threads[0].id.as_ref(), Some(&session));
        }
    }
}

#[tokio::test]
async fn slow_generation_never_blocks_followups_or_overwrites_a_manual_title() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    *agent.generated_title.lock().unwrap() = Some(TITLE.into());
    let gate = agent.title_gate.acquire().await.unwrap();
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let session = create(
        &service,
        ProviderKind::Codex,
        directory.path().to_str().unwrap(),
        true,
    )
    .await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        agent.title_started.notified(),
    )
    .await
    .unwrap();
    wait_for(&service, &session, |state| {
        state
            .runs
            .values()
            .all(|run| run.status == model::RunStatus::Completed)
    })
    .await;
    change(
        &service,
        &session,
        Command::Submit {
            id: Uuid::new_v4().to_string().into(),
            content: content(ProviderKind::Codex, "次の依頼"),
        },
    )
    .await;
    wait_for(&service, &session, |state| {
        state.runs.len() == 2
            && state
                .runs
                .values()
                .all(|run| run.status == model::RunStatus::Completed)
    })
    .await;
    change(&service, &session, Command::Rename("手動の名前".into())).await;
    drop(gate);
    wait_for(&service, &session, |state| {
        state
            .effects
            .values()
            .filter(|effect| {
                matches!(
                    effect.request,
                    model::EffectRequest::ThreadTitleGenerate { .. }
                )
            })
            .all(|effect| effect.status == model::EffectStatus::Cancelled)
    })
    .await;
    let native = service
        .inner
        .conversation
        .storage
        .active_source(&session)
        .unwrap()
        .unwrap();
    Runtime::observe(
        &service,
        AgentChange::Renamed {
            session: native,
            name: Some("native late title".into()),
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        service
            .inner
            .conversation
            .storage
            .thread_state(&session.id, &[])
            .unwrap()
            .thread
            .title,
        "手動の名前"
    );
    assert_eq!(agent.title_prompts.lock().unwrap().len(), 1);
}
