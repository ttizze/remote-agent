use super::super::test_support::{NOW, database, thread};
use super::*;
use serde_json::json;

fn item(id: &str, thread: &str, run: Option<&str>, ordinal: u64) -> model::TurnItem {
    serde_json::from_value(json!({
        "id":id,"threadId":thread,"runId":run,"nodeId":null,"providerThreadId":null,"providerTurnId":null,
        "nativeItemRef":null,"parentItemId":null,"ordinal":ordinal,"status":"completed","title":null,
        "startedAt":NOW,"completedAt":NOW,"updatedAt":NOW,
        "type":"assistant_message","messageId":id,"text":id,"attachments":[],"streaming":false
    })).unwrap()
}

fn run(id: &str, ordinal: u64) -> model::Run {
    serde_json::from_value(json!({
        "id":id,"threadId":"parent","ordinal":ordinal,"providerInstanceId":"codex",
        "modelSelection":{"instanceId":"codex","model":"example"},"providerThreadId":null,
        "userMessageId":id,"rootNodeId":null,"activeAttemptId":null,"status":"completed",
        "queuePosition":null,"queueHeld":false,"requestedAt":NOW,"startedAt":NOW,"completedAt":NOW,
        "checkpointId":null,"contextHandoffId":null
    }))
    .unwrap()
}

fn save_thread(db: &mut Connection, id: &str) {
    let tx = db.transaction().unwrap();
    projections::save(&tx, None, &thread(id)).unwrap();
    tx.commit().unwrap();
}

#[test]
fn old_unknown_input_stays_actionable_outside_the_bounded_visible_page() {
    let mut db = database();
    save_thread(&mut db, "thread");
    let tx = db.transaction().unwrap();
    let mut user = item("old-input", "thread", None, 1);
    user.body = model::TurnItemBody::UserMessage {
        message_id: "old".into(),
        created_by: model::Actor::User,
        creation_source: model::CreationSource::Mobile,
        input_intent: "steer".into(),
        text: "extra".into(),
        attachments: vec![],
        context: None,
    };
    save_item(&tx, &user).unwrap();
    let mut error = item("old-error", "thread", None, 2);
    error.parent_item_id = Some(user.id.clone());
    error.status = model::TurnItemStatus::Failed;
    error.body = model::TurnItemBody::Error {
        failure: model::ProviderFailure {
            class: "delivery_uncertain".into(),
            message: "lost".into(),
            code: None,
            retryable: Some(false),
            reset_at: None,
        },
        retry: None,
    };
    save_item(&tx, &error).unwrap();
    for index in 0..160 {
        save_item(
            &tx,
            &item(&format!("new-{index}"), "thread", None, index + 3),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let loaded = snapshot(&db, "thread").unwrap();
    assert!(
        !loaded
            .projection
            .visible_turn_items
            .iter()
            .any(|row| row.source_item_id == "old-input")
    );
    assert!(
        loaded
            .projection
            .turn_items
            .iter()
            .any(|item| item.id == "old-input")
    );
    assert!(
        loaded
            .projection
            .turn_items
            .iter()
            .any(|item| item.id == "old-error")
    );
}

fn save_item(tx: &Transaction<'_>, item: &model::TurnItem) -> Result<(), String> {
    event_store::append(
        tx,
        &model::Event {
            id: Uuid::new_v4().to_string(),
            thread_id: item.thread_id.clone(),
            run_id: item.run_id.clone(),
            node_id: item.node_id.clone(),
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: NOW.into(),
            change: model::Change::TurnItem(item.clone()),
        },
        None,
        item.run_id.as_ref().map(|id| {
            projections::read::<model::Run>(tx, id)
                .unwrap()
                .unwrap()
                .ordinal
        }),
    )
    .map(|_| ())
}

#[test]
fn windows_page_all_native_history_without_reading_unselected_bodies() {
    let mut db = database();
    save_thread(&mut db, "thread");
    let tx = db.transaction().unwrap();
    for ordinal in 1..=160 {
        save_item(
            &tx,
            &item(&format!("item:{ordinal}"), "thread", None, ordinal),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let newest = snapshot(&db, "thread").unwrap();
    assert_eq!(newest.projection.visible_turn_items.len(), 75);
    assert_eq!(newest.projection.visible_turn_items[0].item.ordinal, 86);
    assert_eq!(newest.latest_local_turn_ordinal, Some(160));
    assert!(newest.has_more_history);
    let older = history(&db, "thread", newest.history_cursor.as_ref().unwrap()).unwrap();
    assert_eq!(older.items.len(), 75);
    assert_eq!(older.items[0].item.ordinal, 11);
    let oldest = history(&db, "thread", older.next_cursor.as_ref().unwrap()).unwrap();
    assert_eq!(oldest.items.len(), 10);
    assert!(!oldest.has_more_history);
    assert_eq!(oldest.next_cursor, None);
    db.execute("UPDATE orchestration_v2_projection_turn_items SET payload_json=json_remove(payload_json,'$.text') WHERE turn_item_id='item:1'",[]).unwrap();
    assert!(snapshot(&db, "thread").is_ok());
    assert!(history(&db, "thread", older.next_cursor.as_ref().unwrap()).is_err());
}

#[test]
fn forks_inherit_only_the_prefix_without_copying_items_or_renumbering_local_runs() {
    let mut db = database();
    save_thread(&mut db, "parent");
    let tx = db.transaction().unwrap();
    for ordinal in 1..=2 {
        let run = run(&format!("run:{ordinal}"), ordinal);
        projections::save(&tx, Some("parent"), &run).unwrap();
        save_item(
            &tx,
            &item(
                &format!("parent:{ordinal}"),
                "parent",
                Some(&run.id),
                ordinal * 1_000_000 + 1,
            ),
        )
        .unwrap();
    }
    let mut child = thread("child");
    child.forked_from = Some(model::ThreadForkSource::Run {
        thread_id: "parent".into(),
        run_id: "run:1".into(),
    });
    projections::save(&tx, None, &child).unwrap();
    let mut child_run = run("child-run", 1);
    child_run.thread_id = "child".into();
    projections::save(&tx, Some("child"), &child_run).unwrap();
    save_item(&tx, &item("child:1", "child", Some("child-run"), 1_000_001)).unwrap();
    tx.commit().unwrap();
    let snapshot = snapshot(&db, "child").unwrap();
    let rows = &snapshot.projection.visible_turn_items;
    assert_eq!(
        rows.iter()
            .map(|row| (row.visibility, row.source_item_id.as_str(), row.position))
            .collect::<Vec<_>>(),
        [
            (Visibility::Inherited, "parent:1", 0),
            (Visibility::Synthetic, "turn-item:fork:child", 1),
            (Visibility::Local, "child:1", 2)
        ]
    );
    assert_eq!(snapshot.projection.turn_items.len(), 1);
    assert_eq!(snapshot.projection.runs[0].ordinal, 1);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM orchestration_v2_projection_turn_items WHERE thread_id='child'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn a_later_parent_rewind_preserves_already_forked_history_and_side_chats_show_only_local_items() {
    let mut db = database();
    save_thread(&mut db, "parent");
    let tx = db.transaction().unwrap();
    let mut source_run = run("run", 1);
    source_run.status = model::RunStatus::RolledBack;
    source_run.completed_at = Some("2026-11-14T00:00:01.000Z".into());
    projections::save(&tx, Some("parent"), &source_run).unwrap();
    save_item(&tx, &item("source", "parent", Some("run"), 1)).unwrap();
    let mut child = thread("child");
    child.forked_from = Some(model::ThreadForkSource::Run {
        thread_id: "parent".into(),
        run_id: "run".into(),
    });
    projections::save(&tx, None, &child).unwrap();
    let mut side = child.clone();
    side.id = "side".into();
    side.lineage.side_chat = Some(true);
    projections::save(&tx, None, &side).unwrap();
    tx.commit().unwrap();
    assert!(
        snapshot(&db, "parent")
            .unwrap()
            .projection
            .visible_turn_items
            .is_empty()
    );
    assert_eq!(
        snapshot(&db, "child")
            .unwrap()
            .projection
            .visible_turn_items
            .len(),
        2
    );
    assert!(
        snapshot(&db, "side")
            .unwrap()
            .projection
            .visible_turn_items
            .is_empty()
    );
}

#[test]
fn an_oversized_body_cannot_deadlock_paging_and_invalid_cursors_are_rejected() {
    let mut db = database();
    save_thread(&mut db, "thread");
    let tx = db.transaction().unwrap();
    let mut oversized = item("huge", "thread", None, 2);
    oversized.body = model::TurnItemBody::SystemNotice {
        message: "x".repeat(1_100_000),
    };
    save_item(&tx, &item("old", "thread", None, 1)).unwrap();
    save_item(&tx, &oversized).unwrap();
    tx.commit().unwrap();
    let newest = snapshot(&db, "thread").unwrap();
    assert_eq!(newest.projection.visible_turn_items.len(), 1);
    assert_eq!(
        newest.projection.visible_turn_items[0].source_item_id,
        "huge"
    );
    assert_eq!(
        history(&db, "thread", newest.history_cursor.as_ref().unwrap())
            .unwrap()
            .items
            .len(),
        1
    );
    for cursor in ["", "invalid", "e30"] {
        assert!(history(&db, "thread", cursor).is_err());
    }
}

#[test]
fn missing_or_cyclic_fork_sources_keep_local_history_available() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    for (id, source) in [("missing", "absent"), ("a", "b"), ("b", "a")] {
        let mut saved = thread(id);
        saved.forked_from = Some(model::ThreadForkSource::Run {
            thread_id: source.into(),
            run_id: "source-run".into(),
        });
        projections::save(&tx, None, &saved).unwrap();
        save_item(&tx, &item(&format!("local:{id}"), id, None, 1)).unwrap();
    }
    tx.commit().unwrap();
    for id in ["missing", "a", "b"] {
        let loaded = snapshot(&db, id).unwrap();
        assert!(
            loaded
                .projection
                .visible_turn_items
                .iter()
                .any(|row| row.source_item_id == format!("local:{id}"))
        );
    }
    assert_eq!(
        snapshot(&db, "missing")
            .unwrap()
            .projection
            .visible_turn_items
            .len(),
        1
    );
}

#[test]
fn rollback_cutoffs_compare_instants_including_different_timestamp_precisions() {
    let mut source = run("run", 1);
    source.status = model::RunStatus::RolledBack;
    source.completed_at = Some("2026-11-14T00:00:00.100Z".into());
    assert_eq!(
        inherited_run_ids([&source].into_iter(), 1, "2026-11-14T00:00:00.1Z").unwrap(),
        ["run"]
    );
    assert!(
        inherited_run_ids([&source].into_iter(), 1, "2026-11-14T00:00:00.101Z")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_history_page_carries_the_stop_request_paired_with_its_result() {
    let mut db = database();
    save_thread(&mut db, "parent");
    let tx = db.transaction().unwrap();
    projections::save(&tx, Some("parent"), &run("run", 1)).unwrap();
    let mut request = item("request", "parent", Some("run"), 1);
    request.body = model::TurnItemBody::RunInterruptRequest {
        message: "stop".into(),
    };
    save_item(&tx, &request).unwrap();
    for ordinal in 0..100 {
        save_item(
            &tx,
            &item(
                &format!("middle:{ordinal}"),
                "parent",
                Some("run"),
                ordinal + 2,
            ),
        )
        .unwrap();
    }
    let mut result = item("result", "parent", Some("run"), 102);
    result.body = model::TurnItemBody::RunInterruptResult {
        message: "stopped".into(),
    };
    save_item(&tx, &result).unwrap();
    save_item(&tx, &item("anchor", "parent", Some("run"), 103)).unwrap();
    tx.commit().unwrap();
    let cursor = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&HistoryCursor {
            v: 1,
            seq: 0,
            st: "parent".into(),
            si: "anchor".into(),
            p: 102,
        })
        .unwrap(),
    );
    let page = history(&db, "parent", &cursor).unwrap();
    assert_eq!(page.items.len(), 76);
    assert_eq!(page.items[0].source_item_id, "request");
    assert_eq!(page.items.last().unwrap().source_item_id, "result");
    assert!(page.has_more_history);
}

#[test]
fn context_excerpts_read_only_selected_bodies_and_do_not_count_nonhistorical_items() {
    let mut db = database();
    save_thread(&mut db, "thread");
    let tx = db.transaction().unwrap();
    for ordinal in 0..4 {
        let mut value = item(&format!("item:{ordinal}"), "thread", None, ordinal + 1);
        if ordinal == 1 {
            value.body = model::TurnItemBody::Reasoning {
                text: "private reasoning".into(),
                streaming: false,
            };
        }
        save_item(&tx, &value).unwrap();
    }
    tx.commit().unwrap();
    db.execute("UPDATE orchestration_v2_projection_turn_items SET payload_json=json_remove(payload_json,'$.text') WHERE turn_item_id IN ('item:0','item:1','item:3')",[]).unwrap();
    let mut params = agent_protocol::handoff::ReadHistory {
        item_id: None,
        session: SessionRef::new(None, "thread".into()).unwrap(),
        offset: 1,
        limit: 1,
        text_offset: 0,
    };
    let page = context_history(&db, &params).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].position, 1);
    assert_eq!(page.items[0].message.text, "item:2");
    assert_eq!(page.next_offset, Some(2));
    params.offset = 0;
    assert!(
        context_history(&db, &params).is_err(),
        "the malformed selected body is observable"
    );
    params.offset = usize::MAX;
    let end = context_history(&db, &params).unwrap();
    assert!(end.items.is_empty() && end.next_offset.is_none());
}

#[test]
fn context_excerpts_cannot_read_a_deleted_conversation() {
    let mut db = database();
    let tx = db.transaction().unwrap();
    let mut removed = thread("thread");
    removed.deleted_at = Some(NOW.into());
    projections::save(&tx, None, &removed).unwrap();
    tx.commit().unwrap();
    let params = agent_protocol::handoff::ReadHistory {
        item_id: None,
        session: SessionRef::new(None, "thread".into()).unwrap(),
        offset: 0,
        limit: 20,
        text_offset: 0,
    };
    assert!(context_history(&db, &params).is_err());
}
