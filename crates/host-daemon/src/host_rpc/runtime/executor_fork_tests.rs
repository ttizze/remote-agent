//! The actual native I/O lease must protect a query-retiring fork from a
//! concurrently admitted prompt; canonical provenance is rechecked after wait.
use super::super::native_fixture::{NativeAgent, call};
use super::*;
use agent_protocol::{conversation::Command, protocol::Call};

fn content(text: &str) -> agent_protocol::conversation::InputContent {
    agent_protocol::conversation::InputContent {
        parts: vec![op::Input::Text { text: text.into() }],
        model: Some(agent_protocol::models::ModelRef {
            provider: ProviderKind::Codex,
            id: "fixture".into(),
        }),
        effort: None,
        service_tier: None,
        follow_up: Default::default(),
    }
}

async fn completed(
    service: &HostRpcService,
    session: &SessionRef,
    count: usize,
) -> model::projection::ThreadSnapshot {
    let storage = &service.inner.conversation.storage;
    let mut changed = storage.changes();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let frames = storage.thread_stream(session, None).unwrap();
            let model::projection::ThreadStreamItem::Snapshot(snapshot) =
                frames.into_iter().next().unwrap()
            else {
                panic!("snapshot")
            };
            if snapshot.projection.runs.len() == count
                && snapshot
                    .projection
                    .runs
                    .iter()
                    .all(|run| run.status == model::RunStatus::Completed)
            {
                return snapshot;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .expect("the real canonical run completes")
}

#[rstest::rstest]
#[case::unchanged(false)]
#[case::changed(true)]
#[tokio::test]
async fn native_fork_serializes_and_revalidates_the_source(#[case] changed: bool) {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let source = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: Some(agent_protocol::workspace::WorkspaceStrategy::Root),
            session: source.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_str().unwrap().into()),
            selection: Default::default(),
            initial: Some((
                Uuid::new_v4().to_string().into(),
                content("original-prefix"),
            )),
        }),
    )
    .await
    .unwrap();
    let first = completed(&service, &source, 1).await;
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: SessionRef = call(
        &service,
        connection.id(),
        Call::ForkConversation(records::Fork {
            source: source.clone(),
            target: target.clone(),
            through: records::ForkPoint::Run {
                run_id: first.projection.runs[0].id.clone(),
            },
        }),
    )
    .await
    .unwrap();
    let runtime = &service.inner.conversation;
    let origin = runtime
        .storage
        .fork_origin(&target)
        .unwrap()
        .expect("admission selected a real native fork");
    let model::projection::ThreadStreamItem::Snapshot(child) = runtime
        .storage
        .thread_stream(&target, None)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
    else {
        panic!("snapshot")
    };
    let point = child.projection.context_transfers[0].source_point.clone();
    let native = SessionRef::new(
        ProviderKind::Codex,
        point
            .provider_thread_ref
            .as_ref()
            .unwrap()
            .native_id
            .clone()
            .unwrap(),
    )
    .unwrap();
    let resource = runtime.register_native(&source, &native);
    let held = resource.io.lock().await;
    let creations = agent.created_in.lock().unwrap().len();
    let mut copying = std::pin::pin!(fork_native(
        &service,
        agent.as_ref(),
        ProviderKind::Codex,
        &origin,
        Some(&point),
        None
    ));
    // Poll the real operation once: no timeout or scheduler delay can hide a
    // missing source lock, since all native fixture gates are open.
    assert!(
        futures_util::poll!(copying.as_mut()).is_pending(),
        "the source I/O holder must block native copy"
    );
    assert_eq!(agent.created_in.lock().unwrap().len(), creations);
    if changed {
        let ack: records::Acknowledged = call(
            &service,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: source.clone(),
                operation: Uuid::new_v4(),
                command: Command::Submit {
                    id: Uuid::new_v4().to_string().into(),
                    content: content("new-parent-input"),
                },
            }),
        )
        .await
        .unwrap();
        assert!(ack.error.is_none(), "{:?}", ack.error);
    }
    drop(held);
    let result = copying.await.unwrap();
    assert_eq!(
        result.is_none(),
        changed,
        "a newly active source invalidates the old native plan after waiting"
    );
    assert_eq!(
        agent.created_in.lock().unwrap().len(),
        creations + usize::from(!changed)
    );
    if changed {
        completed(&service, &source, 2).await;
    }
}
