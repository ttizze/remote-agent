use serde_json::json;

fn timeline_with_text(text: String) -> agent_protocol::session::Timeline {
    let item: agent_protocol::models::Item = serde_json::from_value(json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":text,"phase":"unknown"}}}}})).unwrap();
    agent_protocol::session::Timeline {
        turns: Some(vec![std::sync::Arc::new(agent_protocol::models::Turn {
            id: "turn".into(),
            items: Some(vec![std::sync::Arc::new(item)]),
            ..Default::default()
        })]),
        ..Default::default()
    }
}
proptest::proptest! {
    #[test]
    fn owned_deltas_preserve_unicode_order_and_reuse_unique_items(prefix in proptest::prelude::any::<String>(), deltas in proptest::collection::vec(proptest::prelude::any::<String>(), 0..20)) {
        use agent_protocol::{session::{SessionChange, TextField}, items::ItemBody};
        let mut timeline = timeline_with_text(prefix.clone());
        let item_ptr = std::sync::Arc::as_ptr(&timeline.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0]);
        let mut expected = prefix;
        for delta in deltas {
            expected.push_str(&delta);
            let (next, result) = SessionChange::Text { turn_id: "turn".into(), item_id: "item".into(), field: TextField::AssistantText, delta }.apply_timeline(timeline);
            proptest::prop_assert!(result.is_ok());
            timeline = next;
            let item = &timeline.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0];
            proptest::prop_assert_eq!(std::sync::Arc::as_ptr(item), item_ptr);
            let ItemBody::AssistantText { text, .. } = item.body() else { panic!("body changed"); };
            proptest::prop_assert_eq!(text, &expected);
        }
    }
}
#[test]
fn invalid_owned_delta_preserves_state_and_shared_readers() {
    use agent_protocol::session::{SessionChange, TextField};
    let original = timeline_with_text("kept".into());
    for (turn, item, field) in [
        ("missing", "item", TextField::AssistantText),
        ("turn", "missing", TextField::AssistantText),
        ("turn", "item", TextField::CommandOutput),
    ] {
        let (next, result) = SessionChange::Text {
            turn_id: turn.into(),
            item_id: item.into(),
            field,
            delta: "invalid".into(),
        }
        .apply_timeline(original.clone());
        assert!(result.is_err());
        assert_eq!(next, original);
    }
    let shared = original.clone();
    let (changed, result) = SessionChange::Text {
        turn_id: "turn".into(),
        item_id: "item".into(),
        field: TextField::AssistantText,
        delta: " + live".into(),
    }
    .apply_timeline(original);
    assert!(result.is_ok());
    assert_eq!(shared, timeline_with_text("kept".into()));
    assert_eq!(changed, timeline_with_text("kept + live".into()));
}

#[test]
fn thread_updates_publish_status_requests_and_delivery_without_changing_metadata() {
    use agent_protocol::{
        models::{SessionStatus, Thread},
        requests::Request,
        session::{SessionChange, SubmissionDelivery},
    };
    let thread = Thread {
        name: Some("original metadata".into()),
        cwd: Some("/workspace".into()),
        ..Default::default()
    };
    let active = SessionChange::Status {
        status: SessionStatus::Running,
    }
    .apply(&thread)
    .unwrap();
    assert_eq!(active.status, SessionStatus::Running);
    assert_eq!(active.name, thread.name);
    assert_eq!(active.cwd, thread.cwd);
    let submitted = SessionChange::Submission {
        id: "input".into(),
        delivery: SubmissionDelivery::Sending,
    }
    .apply(&active)
    .unwrap();
    assert_eq!(submitted.submissions["input"], SubmissionDelivery::Sending);
    assert_eq!(submitted.status, SessionStatus::Running);
    let request: Request = serde_json::from_value(json!({"id":"request","target":"session","delivery":"awaiting","body":{"approval":{"kind":"command","description":"","details":"","choices":[{"id":"allow","label":"Allow","description":""}]}}})).unwrap();
    let requested = SessionChange::Request {
        request: request.clone(),
    }
    .apply(&submitted)
    .unwrap();
    assert_eq!(requested.requests[&request.id].as_ref(), &request);
    assert_eq!(requested.submissions["input"], SubmissionDelivery::Sending);
    assert!(thread.requests.is_empty());
    assert!(thread.submissions.is_empty());
}
#[test]
fn an_owned_item_update_replaces_its_existing_position() {
    use agent_protocol::{models::Item, session::SessionChange};
    let timeline = timeline_with_text("partial".into());
    let item: Item = serde_json::from_value(json!({"id":"item","status":"running","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"complete","phase":"unknown"}}}}})).unwrap();
    let (next, result) = SessionChange::Item {
        turn_id: "turn".into(),
        item: item.into(),
    }
    .apply_timeline(timeline);
    assert!(result.is_ok());
    assert_eq!(next, timeline_with_text("complete".into()));
}
