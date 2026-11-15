use super::*;
use agent_protocol::execution::{BackgroundTask, BackgroundTaskKind};

#[test]
fn pending_answer_admission_enforces_size_and_per_conversation_and_host_limits() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(
        Arc::new(Storage::open(&directory.path().join("state.sqlite"))),
        [],
        false,
    );
    let answer = Arc::new(super::super::native_fixture::AnswerResource);
    let pending = |conversation: usize| PendingRequest {
        body: agent_protocol::requests::RequestBody::Question { questions: vec![] },
        target: SessionRef::new(None, Uuid::from_u128(conversation as u128 + 1).to_string())
            .unwrap(),
        native: SessionRef::new(ProviderKind::Codex, "native".into()).unwrap(),
        attempt_id: Some(Uuid::new_v4()),
        origin: RequestOrigin {
            instance: Uuid::new_v4(),
            native_id: serde_json::json!(1),
            provider: ProviderKind::Codex,
            source: answer.clone(),
        },
    };
    assert!(
        runtime
            .retain_request("oversized".into(), 64 * 1024 + 1, pending(0))
            .is_err()
    );
    for conversation in 0..8 {
        for request in 0..32 {
            assert!(
                runtime
                    .retain_request(
                        format!("{conversation}:{request}").into(),
                        64 * 1024,
                        pending(conversation)
                    )
                    .unwrap()
            );
        }
        if conversation == 0 {
            assert!(
                runtime
                    .retain_request("conversation-overflow".into(), 1, pending(0))
                    .is_err()
            );
        }
    }
    // A duplicate never consumes another slot, including at the host limit.
    let duplicate_resource = Arc::new(super::super::native_fixture::AnswerResource);
    let mut duplicate = pending(0);
    duplicate.origin.source = duplicate_resource.clone();
    assert!(
        !runtime
            .retain_request("0:0".into(), 64 * 1024, duplicate)
            .unwrap()
    );
    assert_eq!(
        Arc::strong_count(&duplicate_resource),
        1,
        "a duplicate cannot replace the original native answer handle"
    );
    assert!(
        runtime
            .retain_request("host-overflow".into(), 1, pending(8))
            .is_err()
    );
    drop(runtime);
    assert_eq!(
        Arc::strong_count(&answer),
        1,
        "all admitted native resources are released with their owner"
    );
}

#[test]
fn a_new_foreground_turn_cannot_inherit_or_hide_the_previous_turns_background_work() {
    let source = Source {
        target: SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap(),
        session_id: Uuid::new_v4().to_string(),
        io: Default::default(),
        updates: Default::default(),
        text: Default::default(),
        background: Default::default(),
        images: Default::default(),
    };
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let task = BackgroundTask {
        id: "monitor".into(),
        kind: BackgroundTaskKind::Command,
        description: None,
    };
    assert_eq!(
        source.roster(first, vec![task.clone()]),
        vec![(first, vec![task.clone()], false)]
    );
    assert_eq!(
        source.roster(second, vec![task.clone()]),
        vec![(first, vec![task.clone()], false)]
    );
    assert_eq!(source.background_owners(), vec![(first, vec![task])]);
    assert_eq!(source.roster(second, vec![]), vec![(first, vec![], false)]);
    assert!(
        source
            .background_owners()
            .iter()
            .all(|(_, tasks)| tasks.is_empty())
    );
}
