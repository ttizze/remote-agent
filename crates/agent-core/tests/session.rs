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
fn session_error_survives_binary_transport_and_updates_the_turn() {
    for error in [
        json!({"message":"provider failed", "details":{"code":429,"retryAfter":null}}),
        json!("provider failed"),
    ] {
        for will_retry in [false, true] {
            let change = SessionChange::Error {
                turn_id: "run".into(),
                error: error.clone(),
                will_retry,
            };
            let bytes = agent_core::protocol::encode(&change).unwrap();
            let decoded: SessionChange = agent_core::protocol::decode(&bytes).unwrap();
            assert_eq!(decoded, change);
            let updated = decoded.apply(&conversation()).unwrap();
            let actual = updated.turns.as_ref().unwrap()[0].error.as_ref().unwrap();
            assert_eq!(actual["message"], "provider failed");
            assert_eq!(actual["willRetry"], will_retry);
            assert_eq!(
                serde_json::to_value(&decoded).unwrap()["error"]["error"],
                error
            );
        }
    }
}

#[test]
fn unavailable_history_preserves_live_turn_requests_and_subsequent_text() {
    use agent_core::{
        session::OpenedSession,
        state::{
            Event, Snapshot,
            operations::{Operation, ReadThread},
            reduce,
        },
    };
    let cached: Thread = serde_json::from_value(json!({"id":"native","turns":[
        {"id":"A","status":"completed","items":[{"id":"past","type":"agentMessage","text":"cached history"}]},
        {"id":"stale","status":"inProgress"}
    ]})).unwrap();
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.conversations).insert("native".into(), Arc::new(cached));
    let subscription = uuid::Uuid::new_v4();
    let response = serde_json::from_value(json!({"thread":{
        "id":"native", "status":{"type":"active"}, "historyReadState":{"type":"unavailable"},
        "turns":[{"id":"B","status":"inProgress","items":[{"id":"latest","type":"agentMessage","text":"live"}]}],
        "requests":{"approval":{"id":"approval","method":"item/commandExecution/requestApproval","params":{"threadId":"native","turnId":"B"}}}
    }})).unwrap();
    ReadThread::new("native".into()).apply(
        &mut snapshot,
        OpenedSession {
            session: SessionRef::from_thread_id("native").unwrap(),
            subscription_id: subscription,
            response,
        },
    );
    let change = SessionChange::Text {
        turn_id: "B".into(),
        item_id: "latest".into(),
        field: TextField::Message,
        delta: " updated".into(),
    };
    let (snapshot, _) = reduce(
        &snapshot,
        Event::SessionUpdate(Box::new(agent_core::session::SessionUpdate {
            subscription_id: subscription,
            change,
        })),
    );
    let thread = &snapshot.conversations["native"];
    let turns = thread.turns.as_ref().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(
        turns[0].items.as_ref().unwrap()[0].text.as_deref(),
        Some("cached history")
    );
    assert_eq!(
        turns[1].items.as_ref().unwrap()[0].text.as_deref(),
        Some("live updated")
    );
    assert_eq!(
        thread.status.as_ref().unwrap().kind,
        agent_core::models::ThreadStatusKind::Active
    );
    assert!(thread.requests.contains_key("approval"));
    assert!(snapshot.requests.contains_key("approval"));
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
        }
        .into(),
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
    turn.started_at = Some(123.);
    turn.error = Some(json!({"message":"retrying", "willRetry":true}));
    turn.items.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({
        "id":"tool", "type":"mcpToolCall", "result":{"content":[{"type":"image", "data":"fixture"}]}, "parentToolUseId":"parent"
    })).unwrap()));
    let final_turn = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            status: Some("completed".into()),
            items: Some(vec![]),
            started_at: None,
            ..Default::default()
        },
        completed: true,
    };
    let completed = final_turn.apply(&original).unwrap();
    let turn = &completed.turns.as_ref().unwrap()[0];
    assert_eq!(turn.started_at, Some(123.));
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
    let saved: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&next.local_state()).unwrap()).unwrap();
    let (restored, _) = reduce(&saved, Event::StorageScope("host-key:area-a".into()));
    assert_eq!(restored.drafts["native"].text, "area A draft");
    assert_eq!(
        restored.archived_scopes["host-key:area-b"].drafts["native"].text,
        "area B draft"
    );
    assert!(restored.conversations.is_empty());
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
    assert_eq!(
        late.status.as_ref().unwrap().kind,
        agent_core::models::ThreadStatusKind::Active
    );
    assert_eq!(late.turns.as_ref().unwrap().last().unwrap().id, "next");
}

#[test]
fn large_images_are_deferred_without_truncating_base64_or_mutating_native_data() {
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
    thread.defer_item_details(agent_core::models::MAX_INLINE_ITEM_BYTES);
    let turn = &thread.turns.as_ref().unwrap()[0];
    assert_eq!(
        turn.deferred_item_ids.as_deref(),
        Some(["image".into()].as_slice())
    );
    assert_eq!(turn.items.as_ref().unwrap()[0].result, None);
    assert_eq!(turn.items.as_ref().unwrap()[0].saved_path, item.saved_path);
    assert_eq!(
        item.result.as_ref().unwrap().as_str().unwrap().len(),
        5 * 1024 * 1024
    );
}

#[test]
fn native_session_ids_round_trip_without_provider_collisions() {
    for id in ["same-native-id", "claude:native", "codex:native"] {
        let codex = SessionRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        };
        let claude = SessionRef {
            provider: ProviderKind::Claude,
            id: id.into(),
        };
        assert_ne!(codex.thread_id(), claude.thread_id());
        for session in [codex, claude] {
            assert_eq!(
                SessionRef::from_thread_id(&session.thread_id()).unwrap(),
                session
            );
        }
    }
}

#[test]
fn local_storage_keeps_user_work_without_host_caches() {
    use agent_core::state::{Draft, FileDraft, Navigation, PendingSubmission, Snapshot};
    let draft = Arc::new(Draft {
        text: "unsent message".into(),
        attachments: vec![agent_core::state::Attachment {
            path: "/uploaded/image.png".into(),
            name: "image.png".into(),
            is_image: true,
        }],
        model: Some("chosen-model".into()),
        ..Default::default()
    });
    let mut original = Snapshot {
        storage_scope: "host:area".into(),
        conversations: Arc::new([("native".into(), Arc::new(conversation()))].into()),
        drafts: Arc::new([("native".into(), draft.clone())].into()),
        pending_submissions: Arc::new(
            [(
                "send-id".into(),
                Arc::new(PendingSubmission {
                    draft_key: "native".into(),
                    draft,
                    turn_id: Some("turn".into()),
                    after_item_id: None,
                    accepted: false,
                    delivery_unknown: true,
                }),
            )]
            .into(),
        ),
        file_drafts: Arc::new(
            [(
                "/file.txt".into(),
                FileDraft {
                    revision: "revision".into(),
                    text: "unsaved edit".into(),
                },
            )]
            .into(),
        ),
        navigation: Arc::new(Navigation {
            thread_id: Some("native".into()),
            cwd: "/project".into(),
            draft_key: "native".into(),
        }),
        epoch: 99,
        ..Default::default()
    };
    Arc::make_mut(&mut original.activity)
        .unread
        .insert("native".into());
    Arc::make_mut(&mut original.activity)
        .active
        .insert("native".into(), true);
    let bytes = serde_json::to_vec(&original.local_state()).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut keys: Vec<_> = saved
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "activity",
            "archived_scopes",
            "drafts",
            "file_drafts",
            "navigation",
            "pending_submissions",
            "storage_scope"
        ]
    );
    assert_eq!(saved["activity"], json!({"unread":["native"]}));
    let restored: Snapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored.drafts, original.drafts);
    assert_eq!(restored.pending_submissions, original.pending_submissions);
    assert_eq!(restored.file_drafts, original.file_drafts);
    assert_eq!(restored.navigation, original.navigation);
    assert_eq!(restored.activity.unread, original.activity.unread);
    assert!(restored.activity.active.is_empty());
    assert!(restored.conversations.is_empty());
    assert!(restored.threads.is_none());
    assert!(restored.models.is_empty());
    assert_eq!(restored.epoch, 0);
    assert!(!restored.connected);
    assert!(!original.conversations.is_empty());
}
