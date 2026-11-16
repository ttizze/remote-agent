use super::*;
use agent_protocol::models::{Item, ItemBody, Thread, ThreadResponse, Turn};

/// T3 latestUserAuthoredMessageAt orders the list: only human input counts.
fn authored(service: &HostRpcService, target: &SessionRef) -> Option<String> {
    service
        .inner
        .conversation
        .storage
        .shell_snapshot()
        .unwrap()
        .threads
        .into_iter()
        .find(|shell| shell.thread.id == target.application_id())
        .unwrap()
        .latest_user_authored_message_at
        .clone()
}

fn user_message(service: &HostRpcService, target: &SessionRef, message: &str) -> model::TurnItem {
    service
        .inner
        .conversation
        .storage
        .thread_item(
            target,
            &model::user_item_id(&target.application_id(), message),
        )
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn client_input_is_human_while_automation_input_neither_counts_nor_claims_a_user() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let connection = service.open_session();
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let _: records::Opened = call(
        &service,
        connection.id(),
        Call::CreateConversation(records::Create {
            workspace: None,
            session: target.clone(),
            provider: ProviderKind::Codex,
            subscription: Uuid::new_v4(),
            cwd: Some(directory.path().to_string_lossy().into_owned()),
            selection: Default::default(),
            initial: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(authored(&service, &target), None);
    let submit = |operation: Uuid| records::Change {
        session: target.clone(),
        operation,
        command: Command::Submit {
            id: operation.to_string().into(),
            content: content(),
        },
    };
    let client = Uuid::new_v4();
    let acknowledged: records::Acknowledged = call(
        &service,
        connection.id(),
        Call::ChangeConversation(submit(client)),
    )
    .await
    .unwrap();
    acknowledged.accepted().unwrap();
    completed(&service, &target, 1).await;
    let human = user_message(&service, &target, &client.to_string());
    let model::TurnItemBody::UserMessage {
        created_by,
        creation_source,
        scheduled_task_id,
        ..
    } = &human.body
    else {
        panic!("user message")
    };
    assert_eq!(
        (*created_by, *creation_source, scheduled_task_id.as_deref()),
        (model::Actor::User, model::CreationSource::Mobile, None)
    );
    let recent = authored(&service, &target);
    assert_eq!(recent.as_deref(), Some(human.updated_at.as_str()));
    let automated = Uuid::new_v4();
    Runtime::change_from(
        &service,
        &submit(automated),
        super::Origin::Automation("daily-review"),
    )
    .await
    .unwrap()
    .accepted()
    .unwrap();
    completed(&service, &target, 2).await;
    let automation = user_message(&service, &target, &automated.to_string());
    let model::TurnItemBody::UserMessage {
        created_by,
        creation_source,
        scheduled_task_id,
        ..
    } = &automation.body
    else {
        panic!("user message")
    };
    assert_eq!(
        (*created_by, *creation_source, scheduled_task_id.as_deref()),
        (
            model::Actor::System,
            model::CreationSource::Server,
            Some("daily-review")
        )
    );
    assert_eq!(authored(&service, &target), recent);
    service.shutdown_owned_processes().await;
}

#[tokio::test]
async fn imported_recency_uses_user_turns_and_subagent_prompts_belong_to_the_parent_agent() {
    let directory = tempfile::tempdir().unwrap();
    let agent = NativeAgent::new(ProviderKind::Codex);
    let service = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let turn = |id: &str, started: u64, body: ItemBody| {
        Arc::new(Turn {
            id: id.into(),
            status: agent_protocol::execution::TurnStatus::Completed,
            started_at_ms: Some(started),
            items: Some(vec![Arc::new(Item::new(
                format!("{id}-item").into(),
                agent_protocol::execution::ItemStatus::Completed,
                body,
            ))]),
            ..Default::default()
        })
    };
    let prompt = || ItemBody::UserMessage {
        text: Some("Investigate".into()),
        content: vec![],
    };
    let answer = || ItemBody::AssistantText {
        text: "Done".into(),
        phase: Default::default(),
    };
    let parent = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
    for (native, parent_id) in [("top", None), ("child", Some(parent.clone()))] {
        let native = SessionRef::new(ProviderKind::Codex, native.into()).unwrap();
        let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let response = ThreadResponse {
            thread: Thread {
                id: Some(native.clone()),
                parent_id: parent_id.clone(),
                cwd: Some(directory.path().to_str().unwrap().into()),
                turns: Some(vec![
                    turn("asked", 1_000, prompt()),
                    turn("later", 5_000, answer()),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        Runtime::import_native_response(&service, &public, &native, &response, 0)
            .await
            .unwrap();
        let state = service
            .inner
            .conversation
            .storage
            .thread_state(&public.application_id(), &[])
            .unwrap();
        assert_eq!(
            state.thread.native_prompt_actor(),
            if parent_id.is_some() {
                model::Actor::Agent
            } else {
                model::Actor::User
            }
        );
        let expected = parent_id
            .is_none()
            .then(|| "1970-01-01T00:00:01.000Z".to_owned());
        assert_eq!(authored(&service, &public), expected);
    }
    service.shutdown_owned_processes().await;
}
