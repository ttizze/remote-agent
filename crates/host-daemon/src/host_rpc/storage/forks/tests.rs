use super::super::test_support::{NOW, database, thread};
use super::*;
use serde_json::json;

#[test]
fn run_forks_keep_their_item_boundary_after_the_parent_retries() {
    for status in [model::RunStatus::Failed, model::RunStatus::Cancelled] {
        let mut database = database();
        let transaction = database.transaction().unwrap();
        let source_thread = thread("parent");
        projections::save(&transaction, None, &source_thread).unwrap();
        let mut source_run: model::Run = serde_json::from_value(json!({
            "id":"run","threadId":"parent","ordinal":1,"providerInstanceId":"codex",
            "modelSelection":{"instanceId":"codex","model":"example"},
            "userMessageId":"input","status":"failed","queueHeld":false,"requestedAt":NOW
        }))
        .unwrap();
        source_run.status = status;
        projections::save(&transaction, Some("parent"), &source_run).unwrap();
        let mut original: model::TurnItem = serde_json::from_value(json!({
            "id":"input","threadId":"parent","runId":"run","ordinal":1,
            "status":"completed","startedAt":NOW,"updatedAt":NOW,
            "type":"assistant_message","messageId":"input","text":"BEFORE_FORK",
            "attachments":[],"streaming":false
        }))
        .unwrap();
        projections::save(&transaction, Some("parent"), &original).unwrap();
        let state = projections::metadata(&transaction, "parent").unwrap();
        let (child, transfer) = plan(
            &transaction,
            &state,
            &records::ForkPoint::Run {
                run_id: "run".into(),
            },
            model::ForkTarget {
                id: "child",
                transfer_id: "fork",
                title: "Fork".into(),
            },
            NOW,
        )
        .unwrap();
        assert_eq!(transfer.source_point.turn_item_id.as_deref(), Some("input"));
        projections::save(&transaction, None, &child).unwrap();
        projections::save(&transaction, Some("parent"), &transfer).unwrap();
        let before = timeline::snapshot(&transaction, "child").unwrap();
        source_run.status = model::RunStatus::Completed;
        projections::save(&transaction, Some("parent"), &source_run).unwrap();
        original.id = "after".into();
        original.ordinal = 2;
        original.body = model::TurnItemBody::AssistantMessage {
            message_id: "after".into(),
            phase: None,
            text: "AFTER_FORK".into(),
            attachments: vec![],
            streaming: false,
        };
        projections::save(&transaction, Some("parent"), &original).unwrap();
        let after = timeline::snapshot(&transaction, "child").unwrap();
        assert_eq!(
            after.projection.visible_turn_items,
            before.projection.visible_turn_items
        );
        assert!(
            timeline::snapshot(&transaction, "parent")
                .unwrap()
                .projection
                .visible_turn_items
                .iter()
                .any(|row| row.source_item_id == "after")
        );
    }
}
