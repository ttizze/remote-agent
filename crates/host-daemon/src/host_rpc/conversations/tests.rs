use super::*;
use agent_protocol::{
    execution::TurnStatus,
    session::{SubmissionDelivery, TextField},
};

fn native(id: &str) -> SessionRef {
    SessionRef {
        provider: ProviderKind::Codex,
        id: id.into(),
    }
}
fn turn(id: &str, text: &str) -> Arc<Turn> {
    Arc::new(Turn {
        id: id.into(),
        status: TurnStatus::Completed,
        items: Some(vec![Arc::new(Item::new(
            "answer".into(),
            agent_protocol::execution::ItemStatus::Completed,
            agent_protocol::models::ItemBody::AssistantText {
                text: text.into(),
                phase: agent_protocol::models::AssistantPhase::Final,
            },
        ))]),
        ..Default::default()
    })
}
fn first_page(
    store: &Conversations,
    target: &SessionRef,
    turns: Vec<Arc<Turn>>,
    cursor: Option<&str>,
) {
    let response = ThreadResponse {
        thread: Thread {
            id: Some(native("source")),
            name: Some("Imported".into()),
            ..Default::default()
        },
        model: None,
    };
    store
        .import_page(
            target,
            Some(&response),
            &HistoryPage {
                turns,
                next_cursor: cursor.map(str::to_owned),
            },
        )
        .unwrap();
}
fn input(target: &SessionRef) -> agent_protocol::operations::Submission {
    agent_protocol::operations::Submission {
        thread_id: target.clone(),
        client_user_message_id: "input".into(),
        input: Vec::new(),
        model: None,
        effort: None,
        service_tier: None,
    }
}

#[test]
fn manual_titles_survive_discovery_import_and_provider_auto_titles() {
    let store = Conversations::memory();
    let source = Thread {
        id: Some(native("source")),
        name: Some("Source title".into()),
        ..Default::default()
    };
    let target = store.discover(&source, "scope", None).unwrap();
    store.rename(&target, "My title", true).unwrap();
    let renamed_at = store
        .open_thread(&target, 5, false)
        .unwrap()
        .thread
        .updated_at;
    store.discover(&source, "scope", None).unwrap();
    first_page(&store, &target, vec![], None);
    store.rename(&target, "Provider auto title", false).unwrap();
    let metadata = store.open_thread(&target, 5, false).unwrap().thread;
    assert_eq!(metadata.name.as_deref(), Some("My title"));
    assert_eq!(metadata.updated_at, renamed_at);
    assert!(store.rename(&target, "   ", true).is_err());
    store.rename(&target, "Revised", true).unwrap();
    assert_eq!(
        store
            .titles(ProviderKind::Codex, "scope", "revised")
            .unwrap()[0]
            .thread
            .name
            .as_deref(),
        Some("Revised")
    );
    let automatic = store.bind(&native("automatic"), "scope").unwrap();
    store.rename(&automatic, "Provider title", false).unwrap();
    assert_eq!(
        store
            .open_thread(&automatic, 5, false)
            .unwrap()
            .thread
            .name
            .as_deref(),
        Some("Provider title")
    );
}

#[test]
fn new_host_activity_moves_a_conversation_above_source_history_and_reading_does_not() {
    let store = Conversations::memory();
    let recent_source = Thread {
        id: Some(native("recent")),
        updated_at: Some(9999.),
        ..Default::default()
    };
    let recent = store.discover(&recent_source, "scope", None).unwrap();
    let target = store.bind(&native("old"), "scope").unwrap();
    first_page(&store, &target, vec![turn("old-turn", "old")], None);
    assert_eq!(
        store.titles(ProviderKind::Codex, "scope", "").unwrap()[0]
            .thread
            .id
            .as_ref(),
        Some(&recent)
    );
    store.admit(&input(&target)).unwrap();
    assert_eq!(
        store.titles(ProviderKind::Codex, "scope", "").unwrap()[0]
            .thread
            .id
            .as_ref(),
        Some(&target)
    );
    let timestamp = store
        .open_thread(&target, 5, false)
        .unwrap()
        .thread
        .updated_at;
    store
        .apply(
            &target,
            [&SessionChange::TurnItems {
                turn_id: "old-turn".into(),
                items: turn("ignored", "old").items.clone().unwrap(),
            }],
        )
        .unwrap();
    assert_eq!(
        store
            .open_thread(&target, 5, false)
            .unwrap()
            .thread
            .updated_at,
        timestamp
    );
    store
        .apply(
            &target,
            [&SessionChange::Text {
                turn_id: "old-turn".into(),
                item_id: "answer".into(),
                field: TextField::AssistantText,
                delta: " changed".into(),
            }],
        )
        .unwrap();
    assert!(
        store
            .open_thread(&target, 5, false)
            .unwrap()
            .thread
            .updated_at
            >= timestamp
    );
}

#[test]
fn collapsed_pages_select_first_user_and_last_final_but_expansion_retains_every_item() {
    use agent_protocol::{
        execution::ItemStatus,
        items::{ItemBody, MessagePart},
        models::AssistantPhase,
    };
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    let user = |id: &str| {
        Arc::new(Item::new(
            id.into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: None,
                content: vec![MessagePart::Text { text: id.into() }],
            },
        ))
    };
    let answer = |id: &str, phase| {
        Arc::new(Item::new(
            id.into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: id.into(),
                phase,
            },
        ))
    };
    let items = vec![
        user("first"),
        answer("progress", AssistantPhase::Commentary),
        answer("earlier-final", AssistantPhase::Final),
        user("followup"),
        answer("last-final", AssistantPhase::Final),
        answer("later-progress", AssistantPhase::Commentary),
    ];
    let source_turn = Arc::new(Turn {
        id: "turn".into(),
        status: TurnStatus::Completed,
        items: Some(items.clone()),
        ..Default::default()
    });
    first_page(&store, &target, vec![source_turn], Some("older"));
    let summary = store.open_thread(&target, 5, false).unwrap().thread;
    let summary_turn = &summary.turns.as_ref().unwrap()[0];
    assert!(summary_turn.items_summary);
    assert_eq!(
        summary_turn
            .items
            .as_ref()
            .unwrap()
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "last-final"]
    );
    let pending = store
        .history(&target, summary.history_cursor.as_ref().unwrap(), false)
        .unwrap();
    assert!(pending.turns.is_empty());
    assert_eq!(pending.next_cursor, summary.history_cursor);
    assert_eq!(
        store.turn(&target, "turn").unwrap().items.as_ref(),
        Some(&items)
    );
    store
        .import_page(
            &target,
            None,
            &HistoryPage {
                turns: vec![turn("older", "older")],
                next_cursor: None,
            },
        )
        .unwrap();
    let older = store
        .history(&target, pending.next_cursor.as_ref().unwrap(), false)
        .unwrap();
    assert_eq!(older.turns[0].id.as_str(), "older");
    assert!(older.next_cursor.is_none());
}

#[test]
fn native_identity_is_private_stable_and_scoped_to_provider_storage() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "first").unwrap();
    assert_eq!(
        store
            .open_thread(&target, 5, true)
            .unwrap()
            .thread
            .id
            .as_ref(),
        Some(&target)
    );
    assert_ne!(target.id, "source");
    assert!(uuid::Uuid::parse_str(&target.id).is_ok());
    assert_eq!(store.native(&target, "first").unwrap(), native("source"));
    assert!(store.native(&target, "second").is_err());
    assert_ne!(store.bind(&native("source"), "second").unwrap(), target);
    let other = SessionRef {
        provider: ProviderKind::Claude,
        id: "source".into(),
    };
    assert_ne!(store.bind(&other, "first").unwrap(), target);
    assert!(
        store
            .native(
                &SessionRef {
                    provider: ProviderKind::Claude,
                    id: target.id.clone()
                },
                "first"
            )
            .is_err()
    );
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(store.bind(&native("source"), "first").unwrap(), target);
}

#[test]
fn interrupted_import_resumes_without_losing_repeated_turn_occurrences() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(
        &store,
        &target,
        vec![turn("repeated", "newest")],
        Some("older"),
    );
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(
        store.import_state(&target).unwrap(),
        (false, true, Some("older".into()))
    );
    store
        .import_page(
            &target,
            None,
            &HistoryPage {
                turns: vec![turn("repeated", "oldest"), turn("middle", "middle")],
                next_cursor: None,
            },
        )
        .unwrap();
    let mut response = store.open_thread(&target, 1, true).unwrap();
    let newer = response.thread.turns.as_ref().unwrap()[0].clone();
    assert_eq!(newer.id.as_str(), "repeated");
    let mut all = vec![newer];
    while let Some(cursor) = response.thread.history_cursor.take() {
        let page = store.history(&target, &cursor, true).unwrap();
        all.splice(0..0, page.turns.clone());
        response.thread.history_cursor = page.next_cursor;
    }
    assert_eq!(
        all.iter().map(|turn| turn.id.as_str()).collect::<Vec<_>>(),
        ["repeated", "middle", "repeated"]
    );
    assert_eq!(
        all[0].items.as_ref().unwrap()[0].body(),
        turn("x", "oldest").items.as_ref().unwrap()[0].body()
    );
    assert_eq!(
        all[2].items.as_ref().unwrap()[0].body(),
        turn("x", "newest").items.as_ref().unwrap()[0].body()
    );
    let foreign = store.bind(&native("other"), "scope").unwrap();
    let cursor = store
        .open_thread(&target, 1, true)
        .unwrap()
        .thread
        .history_cursor
        .unwrap();
    assert!(store.history(&foreign, &cursor, true).is_err());
}

#[test]
fn invalid_page_and_repeated_cursor_roll_back_contents_and_progress() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(
        &store,
        &target,
        vec![turn("latest", "latest")],
        Some("next"),
    );
    let repeated = HistoryPage {
        turns: vec![turn("bad", "bad")],
        next_cursor: Some("next".into()),
    };
    assert!(store.import_page(&target, None, &repeated).is_err());
    let mut incomplete = turn("incomplete", "details");
    Arc::make_mut(&mut incomplete).items_summary = true;
    assert!(
        store
            .import_page(
                &target,
                None,
                &HistoryPage {
                    turns: vec![turn("other", "other"), incomplete],
                    next_cursor: None
                }
            )
            .is_err()
    );
    assert_eq!(
        store.import_state(&target).unwrap(),
        (false, true, Some("next".into()))
    );
    assert!(store.turn(&target, "bad").is_err());
    assert!(store.turn(&target, "other").is_err());
}

#[test]
fn changes_and_command_receipts_survive_restart_and_payload_reuse_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![turn("run", "prefix")], None);
    let input = input(&target);
    assert_eq!(store.admit(&input).unwrap(), None);
    assert_eq!(
        store
            .open_thread(&target, 5, true)
            .unwrap()
            .thread
            .submissions["input"],
        SubmissionDelivery::Sending
    );
    let accepted = SubmissionDelivery::Accepted {
        turn_id: Some("run".into()),
    };
    store
        .apply(
            &target,
            [&SessionChange::Submission {
                id: "input".into(),
                delivery: accepted.clone(),
            }],
        )
        .unwrap();
    store
        .apply(
            &target,
            [&SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::AssistantText,
                delta: " suffix".into(),
            }],
        )
        .unwrap();
    store.rename(&target, "New title", true).unwrap();
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(store.admit(&input).unwrap(), Some(accepted));
    let mut changed = input.clone();
    changed.effort = Some("high".into());
    assert!(store.admit(&changed).is_err());
    let thread = store.open_thread(&target, 5, true).unwrap().thread;
    assert_eq!(thread.name.as_deref(), Some("New title"));
    assert_eq!(
        thread.turns.unwrap()[0].items.as_ref().unwrap()[0].body(),
        turn("x", "prefix suffix").items.as_ref().unwrap()[0].body()
    );
}

#[test]
fn a_receipt_after_the_finished_echo_retires_visible_delivery_but_keeps_replay_evidence() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    let input = input(&target);
    store.admit(&input).unwrap();
    let mut echo = Item::new(
        "echo".into(),
        agent_protocol::execution::ItemStatus::Completed,
        agent_protocol::models::ItemBody::UserMessage {
            text: Some("sent".into()),
            content: vec![],
        },
    );
    echo.client_input_id = Some(input.client_user_message_id.clone());
    store
        .apply(
            &target,
            [&SessionChange::Turn {
                turn: Turn {
                    id: "done".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(echo)]),
                    ..Default::default()
                },
                completed: true,
            }],
        )
        .unwrap();
    assert_eq!(
        store
            .open_thread(&target, 5, true)
            .unwrap()
            .thread
            .submissions["input"],
        SubmissionDelivery::Sending
    );
    let accepted = SubmissionDelivery::Accepted { turn_id: None };
    store
        .apply(
            &target,
            [&SessionChange::Submission {
                id: input.client_user_message_id.clone(),
                delivery: accepted.clone(),
            }],
        )
        .unwrap();
    assert!(
        store
            .open_thread(&target, 5, true)
            .unwrap()
            .thread
            .submissions
            .is_empty()
    );
    assert_eq!(store.previous_command(&input).unwrap(), Some(accepted));
}

#[test]
fn restart_interrupts_owned_execution_and_keeps_uncertain_inputs_from_replaying() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    store.admit(&input(&target)).unwrap();
    store
        .apply(
            &target,
            [&SessionChange::Turn {
                turn: Turn {
                    id: "running".into(),
                    ..Default::default()
                },
                completed: false,
            }],
        )
        .unwrap();
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(
        store.admit(&input(&target)).unwrap(),
        Some(SubmissionDelivery::Unknown)
    );
    let thread = store.open_thread(&target, 5, true).unwrap().thread;
    assert_eq!(
        thread.status,
        agent_protocol::models::SessionStatus::Unavailable
    );
    let turn = &thread.turns.unwrap()[0];
    assert_eq!(turn.status, TurnStatus::Interrupted);
    let error = turn.error.as_ref().unwrap();
    assert_eq!(
        error.category,
        agent_protocol::execution::ErrorCategory::Network
    );
    assert_eq!(
        error.message,
        "Host restarted; the previous execution can no longer be observed"
    );
}

#[test]
fn restarting_does_not_interrupt_execution_that_was_only_imported() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "scope").unwrap();
    let mut source = turn("external", "still working");
    Arc::make_mut(&mut source).status = TurnStatus::Running;
    first_page(&store, &target, vec![source], None);
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(
        store.turn(&target, "external").unwrap().status,
        TurnStatus::Running
    );
    assert!(store.turn(&target, "external").unwrap().error.is_none());
}

#[test]
fn resuming_import_clears_the_transient_error_without_hiding_source_issues() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(
        &store,
        &target,
        vec![turn("latest", "latest")],
        Some("older"),
    );
    store
        .import_failed(&target, "source temporarily unavailable")
        .unwrap();
    let before = store
        .open_thread(&target, 5, true)
        .unwrap()
        .thread
        .history_read_state
        .unwrap();
    assert_eq!(before.kind, HistoryReadKind::Incomplete);
    assert_eq!(before.issues, ["source temporarily unavailable"]);
    store
        .import_page(
            &target,
            None,
            &HistoryPage {
                turns: vec![turn("older", "older")],
                next_cursor: None,
            },
        )
        .unwrap();
    let after = store
        .open_thread(&target, 5, true)
        .unwrap()
        .thread
        .history_read_state
        .unwrap();
    assert_eq!(after.kind, HistoryReadKind::Complete);
    assert!(after.issues.is_empty());
    let incomplete = store.bind(&native("incomplete"), "scope").unwrap();
    let response = ThreadResponse {
        thread: Thread {
            history_read_state: Some(HistoryReadState::new(
                HistoryReadKind::Incomplete,
                vec!["source transcript is truncated".into()],
            )),
            ..Default::default()
        },
        model: None,
    };
    store
        .import_page(
            &incomplete,
            Some(&response),
            &HistoryPage {
                turns: vec![],
                next_cursor: None,
            },
        )
        .unwrap();
    let after = store
        .open_thread(&incomplete, 5, true)
        .unwrap()
        .thread
        .history_read_state
        .unwrap();
    assert_eq!(after.kind, HistoryReadKind::Incomplete);
    assert_eq!(after.issues, ["source transcript is truncated"]);
}

#[test]
fn invalid_delta_cannot_change_the_projection_or_append_a_journal_event() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![turn("run", "original")], None);
    let count = || {
        store
            .lock()
            .query_row("SELECT count(*) FROM events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    let before = count();
    assert!(
        store
            .apply(
                &target,
                [
                    &SessionChange::Status {
                        status: agent_protocol::models::SessionStatus::Running
                    },
                    &SessionChange::Text {
                        turn_id: "run".into(),
                        item_id: "missing".into(),
                        field: TextField::AssistantText,
                        delta: "lost".into()
                    }
                ]
            )
            .is_err()
    );
    assert_eq!(count(), before);
    assert_eq!(
        store.open_thread(&target, 5, true).unwrap().thread.status,
        agent_protocol::models::SessionStatus::Unknown
    );
    assert_eq!(
        store.turn(&target, "run").unwrap().items.unwrap()[0].body(),
        turn("x", "original").items.as_ref().unwrap()[0].body()
    );
}

#[test]
fn a_streaming_delta_changes_only_its_item_and_never_rewrites_the_turn() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    let mut source = turn("run", "prefix");
    let mut untouched = source.items.as_ref().unwrap()[0].as_ref().clone();
    untouched.id = "untouched".into();
    Arc::make_mut(&mut source)
        .items
        .as_mut()
        .unwrap()
        .push(Arc::new(untouched.clone()));
    first_page(&store, &target, vec![source], None);
    store.lock().execute_batch("CREATE TRIGGER forbid_item_rewrite BEFORE DELETE ON items BEGIN SELECT RAISE(ABORT, 'streaming must not rewrite accumulated items'); END;
        CREATE TRIGGER forbid_unrelated_update BEFORE UPDATE ON items WHEN OLD.native_id='untouched' BEGIN SELECT RAISE(ABORT, 'streaming changed an unrelated item'); END;").unwrap();
    store
        .apply(
            &target,
            [&SessionChange::Text {
                turn_id: "run".into(),
                item_id: "answer".into(),
                field: TextField::AssistantText,
                delta: " suffix".into(),
            }],
        )
        .unwrap();
    let stored = store.turn(&target, "run").unwrap();
    let items = stored.items.unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id.as_str(), "answer");
    assert_eq!(
        items[0].body(),
        turn("ignored", "prefix suffix").items.as_ref().unwrap()[0].body()
    );
    assert_eq!(items[1].as_ref(), &untouched);
}

#[test]
fn an_exact_history_window_has_no_empty_continuation_page() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(
        &store,
        &target,
        (0..5)
            .map(|index| turn(&index.to_string(), "answer"))
            .collect(),
        None,
    );
    let thread = store.open_thread(&target, 5, false).unwrap().thread;
    assert_eq!(thread.turns.as_ref().unwrap().len(), 5);
    assert_eq!(thread.history_has_more, Some(false));
    assert_eq!(thread.history_cursor, None);
    assert_eq!(
        thread.history_read_state.unwrap().kind,
        HistoryReadKind::Complete
    );
}

proptest::proptest! {
    #[test]
    fn history_pagination_preserves_order_and_every_occurrence(ids in proptest::collection::vec(0u8..4, 0..50), limit in 1usize..10) {
        let store = Conversations::memory();
        let target = store.bind(&native("source"), "scope").unwrap();
        first_page(&store, &target, ids.iter().enumerate().map(|(index, id)| turn(&id.to_string(), &index.to_string())).collect(), None);
        let response = store.open_thread(&target, limit, true).unwrap();
        let mut turns = response.thread.turns.unwrap();
        let mut cursor = response.thread.history_cursor;
        let mut pages = 0;
        while let Some(next) = cursor {
            pages += 1;
            proptest::prop_assert!(pages <= ids.len() + 1, "history cursor must advance within the finite source history");
            let page = store.history(&target, &next, true).unwrap();
            turns.splice(0..0, page.turns);
            cursor = page.next_cursor;
        }
        let actual = turns.iter().map(|turn| turn.id.parse::<u8>().unwrap()).collect::<Vec<_>>();
        proptest::prop_assert_eq!(actual, ids);
    }
}
