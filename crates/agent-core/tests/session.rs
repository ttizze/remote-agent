use agent_core::{
    models::{Item, Thread, Turn},
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use serde_json::json;
use std::sync::Arc;

fn conversation() -> Thread {
    serde_json::from_value(json!({"id":"native", "turns":[{"id":"run", "status":"inProgress", "items":[{"id":"answer", "type":"agentMessage", "text":"partial"}]}]})).unwrap()
}

#[test]
fn provider_identity_keeps_the_complete_native_id() {
    let claude = SessionRef::from_thread_id("claude:01234567-89ab-cdef-0123-456789abcdef").unwrap();
    assert_eq!(claude.provider, ProviderKind::Claude);
    assert_eq!(claude.id, "01234567-89ab-cdef-0123-456789abcdef");
    assert_eq!(
        claude.thread_id(),
        "claude:01234567-89ab-cdef-0123-456789abcdef"
    );
    assert_ne!(claude, SessionRef::from_thread_id(&claude.id).unwrap());
    for id in ["", "claude:", " native", "native "] {
        assert!(SessionRef::from_thread_id(id).is_err());
    }
}

#[test]
fn a_final_item_replaces_streamed_text_without_mutating_the_input() {
    let original = conversation();
    let streamed = SessionChange::Text {
        turn_id: "run".into(),
        item_id: "answer".into(),
        field: TextField::Message,
        delta: " draft".into(),
    }
    .apply(&original)
    .unwrap();
    let completed = SessionChange::Item {
        turn_id: "run".into(),
        item: Item {
            id: "answer".into(),
            kind: Some("agentMessage".into()),
            text: Some("final answer".into()),
            ..Default::default()
        },
    }
    .apply(&streamed)
    .unwrap();
    let items = completed.turns.as_ref().unwrap()[0].items.as_ref().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text.as_deref(), Some("final answer"));
    assert_eq!(original, conversation());
}

#[test]
fn completion_preserves_tool_relationships_and_missing_timing_fields() {
    let mut original = conversation();
    let turn = Arc::make_mut(&mut original.turns.as_mut().unwrap()[0]);
    turn.started_at = Some(Some(123.into()));
    turn.error = Some(json!({"message":"retrying", "willRetry":true}));
    turn.items.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({
        "id":"tool", "type":"mcpToolCall", "result":{"content":[{"type":"image", "data":"fixture"}]}, "parentToolUseId":"parent"
    })).unwrap()));
    let final_turn = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            status: Some("completed".into()),
            items: Some(vec![]),
            started_at: Some(None),
            ..Default::default()
        },
        completed: true,
    };
    let completed = final_turn.apply(&original).unwrap();
    let turn = &completed.turns.as_ref().unwrap()[0];
    assert_eq!(turn.started_at, Some(Some(123.into())));
    assert_eq!(turn.error, None);
    assert_eq!(
        turn.items.as_ref().unwrap()[1],
        original.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1]
    );
    let late = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            ..Default::default()
        },
        completed: false,
    };
    assert_eq!(late.apply(&completed).unwrap(), completed);
}

#[test]
fn updates_target_the_latest_occurrence_without_changing_older_turns() {
    let mut original = conversation();
    let first = original.turns.as_ref().unwrap()[0].clone();
    original.turns.as_mut().unwrap().push(first.clone());
    let updated = SessionChange::Text {
        turn_id: "run".into(),
        item_id: "answer".into(),
        field: TextField::Message,
        delta: " new".into(),
    }
    .apply(&original)
    .unwrap();
    assert!(Arc::ptr_eq(&updated.turns.as_ref().unwrap()[0], &first));
    assert_eq!(
        updated.turns.as_ref().unwrap()[1].items.as_ref().unwrap()[0]
            .text
            .as_deref(),
        Some("partial new")
    );
}

#[test]
fn storage_changes_keep_drafts_separate_and_restore_the_original_area() {
    use agent_core::state::{Draft, Event, Snapshot, reduce};
    let mut original = Snapshot {
        storage_scope: "host-key:area-a".into(),
        ..Default::default()
    };
    Arc::make_mut(&mut original.conversations).insert("native".into(), Arc::new(conversation()));
    Arc::make_mut(&mut original.drafts).insert(
        "native".into(),
        Arc::new(Draft {
            text: "area A draft".into(),
            ..Default::default()
        }),
    );
    let (mut next, _) = reduce(&original, Event::StorageScope("host-key:area-b".into()));
    assert!(next.conversations.is_empty());
    assert!(next.drafts.is_empty());
    assert_eq!(
        next.archived_scopes["host-key:area-a"].drafts["native"].text,
        "area A draft"
    );
    Arc::make_mut(&mut next.drafts).insert(
        "native".into(),
        Arc::new(Draft {
            text: "area B draft".into(),
            ..Default::default()
        }),
    );
    let saved: Snapshot = serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
    let (restored, _) = reduce(&saved, Event::StorageScope("host-key:area-a".into()));
    assert_eq!(restored.drafts["native"].text, "area A draft");
    assert_eq!(
        restored.archived_scopes["host-key:area-b"].drafts["native"].text,
        "area B draft"
    );
    assert!(restored.conversations.contains_key("native"));
    assert_eq!(original.drafts["native"].text, "area A draft");
}

#[test]
fn late_completion_does_not_make_a_newer_execution_idle() {
    let old = conversation();
    let running = SessionChange::Turn {
        turn: Turn {
            id: "next".into(),
            ..Default::default()
        },
        completed: false,
    }
    .apply(&old)
    .unwrap();
    let late = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            ..Default::default()
        },
        completed: true,
    }
    .apply(&running)
    .unwrap();
    assert_eq!(late.status.as_ref().unwrap().kind, "active");
    assert_eq!(late.turns.as_ref().unwrap().last().unwrap().id, "next");
}

#[test]
fn native_images_remain_complete_without_the_removed_snapshot_budget() {
    let path = format!("/native/{}/image.png", "a".repeat(300));
    let item: Item = serde_json::from_value(json!({"id":"image","type":"imageGeneration","savedPath":path,"result":"a".repeat(5 * 1024 * 1024)})).unwrap();
    let mut thread = Thread {
        turns: Some(vec![Arc::new(Turn {
            id: "turn".into(),
            items: Some(vec![Arc::new(item.clone())]),
            ..Default::default()
        })]),
        ..Default::default()
    };
    thread.defer_item_details();
    assert_eq!(
        thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].as_ref(),
        &item
    );
    assert!(
        thread.turns.as_ref().unwrap()[0]
            .deferred_item_ids
            .is_none()
    );
}
