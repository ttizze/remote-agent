use super::*;

#[test]
fn only_new_client_submissions_advance_recency_and_the_timestamp_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let id = address("recency");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    assert_eq!(storage.header(&id).unwrap().details.last_input_at, None);
    let send = |event, now| {
        storage
            .decide(
                &id,
                event,
                Context {
                    now,
                    ..context(now as u128)
                },
                None,
            )
            .unwrap()
    };
    let first = send(submit(1), 1_000);
    assert_eq!(first.step.outcome, Outcome::Accepted);
    assert_eq!(
        storage.header(&id).unwrap().details.last_input_at,
        Some(1_000)
    );
    let decision::Action::Deliver {
        id: input,
        action,
        attempt,
        ..
    } = &first.step.actions[0]
    else {
        panic!("delivery");
    };
    send(submit(1), 2_000);
    assert_eq!(
        storage.header(&id).unwrap().details.last_input_at,
        Some(1_000)
    );
    let mut rejected = submit(2);
    let Event::Command {
        command: Command::Submit { content, .. },
        ..
    } = &mut rejected
    else {
        unreachable!()
    };
    content.parts.clear();
    assert!(matches!(
        send(rejected, 3_000).step.outcome,
        Outcome::Rejected(_)
    ));
    assert_eq!(
        storage.header(&id).unwrap().details.last_input_at,
        Some(1_000)
    );
    // Queue admission counts immediately, before the provider receives it.
    assert_eq!(send(submit(3), 4_000).step.outcome, Outcome::Accepted);
    assert_eq!(
        storage.header(&id).unwrap().details.last_input_at,
        Some(4_000)
    );
    for (event, now) in [
        (
            Event::Delivered {
                id: input.clone(),
                action: *action,
                native_turn: None,
            },
            5_000,
        ),
        (
            Event::Provider {
                attempt: attempt.id,
                native_turn: None,
                native_items: Vec::new(),
                observation: Observation::Item {
                    id: "reply".into(),
                    status: ItemStatus::Running,
                    deferred: false,
                    body: ItemBody::AssistantText {
                        text: "progress".into(),
                        phase: AssistantPhase::Commentary,
                    },
                },
            },
            6_000,
        ),
        (
            Event::Provider {
                attempt: attempt.id,
                native_turn: None,
                native_items: Vec::new(),
                observation: Observation::Completed,
            },
            7_000,
        ),
        (Event::Title("New title".into()), 8_000),
    ] {
        send(event, now);
        assert_eq!(
            storage.header(&id).unwrap().details.last_input_at,
            Some(4_000)
        );
    }
    let mut automatic = submit(4);
    let Event::Command { id: operation, .. } = &mut automatic else {
        unreachable!()
    };
    *operation = OperationId::Job(Uuid::from_u128(4));
    assert_eq!(send(automatic, 9_000).step.outcome, Outcome::Accepted);
    assert_eq!(
        storage.header(&id).unwrap().details.last_input_at,
        Some(4_000)
    );
    drop(storage);
    let storage = Storage::open(&path);
    let header = storage.header(&id).unwrap();
    assert_eq!(header.details.last_input_at, Some(4_000));
    assert_eq!(
        decision::records::summary(&id, &header).last_input_at,
        Some(4_000)
    );
}

#[test]
fn imported_recency_uses_user_turns_and_ignores_later_agent_activity() {
    for milliseconds in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let id = address("imported-recency");
        let turns = [
            ("first", 1_000, true),
            ("second", 4_000, true),
            ("wake", 9_000, false),
        ]
        .map(|(name, at, human)| {
            Arc::new(agent_protocol::models::Turn {
                id: name.into(),
                started_at_ms: milliseconds.then_some(at as u64),
                started_at: (!milliseconds).then_some(at as f64 / 1000.0),
                items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                    format!("{name}-item").into(),
                    ItemStatus::Completed,
                    if human {
                        ItemBody::UserMessage {
                            text: Some(name.into()),
                            content: Vec::new(),
                        }
                    } else {
                        ItemBody::AssistantText {
                            text: name.into(),
                            phase: AssistantPhase::Final,
                        }
                    },
                ))]),
                ..Default::default()
            })
        });
        let mut response = agent_protocol::models::ThreadResponse {
            thread: agent_protocol::models::Thread {
                id: Some(id.clone()),
                turns: Some(turns.into()),
                updated_at: Some(20.0),
                ..Default::default()
            },
            ..Default::default()
        };
        storage.import(&id, &response, Some(&id), 20_000).unwrap();
        assert_eq!(
            storage.header(&id).unwrap().details.last_input_at,
            Some(4_000)
        );
        response.thread.updated_at = Some(30.0);
        storage.import(&id, &response, Some(&id), 30_000).unwrap();
        assert_eq!(
            storage.header(&id).unwrap().details.last_input_at,
            Some(4_000)
        );
    }
}
