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
fn catalog_pages_commit_identity_metadata_and_journal_together() {
    use super::super::agent::SessionSummary;
    let store = Conversations::memory();
    let page = ["healthy", ""].map(|id| SessionSummary {
        thread: Thread {
            id: Some(native(id)),
            name: Some(id.into()),
            ..Default::default()
        },
        branch: Some("feature".into()),
    });
    assert!(store.discover_page(&page, "scope").is_err());
    assert!(
        store
            .title_list(
                &[(ProviderKind::Codex, "scope".into())],
                &Default::default(),
                &Default::default()
            )
            .unwrap()
            .0
            .data
            .is_empty()
    );
    assert_eq!(
        store
            .lock()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    store.discover_page(&page[..1], "scope").unwrap();
    let target = store.bind(&native("healthy"), "scope").unwrap();
    store.discover_page(&page[..1], "scope").unwrap();
    let (page, branches) = store
        .title_list(
            &[(ProviderKind::Codex, "scope".into())],
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].name.as_deref(), Some("healthy"));
    assert_eq!(branches.get(&target).map(String::as_str), Some("feature"));
    assert_eq!(page.data[0].id.as_ref(), Some(&target));
    assert_eq!(store.native(&target, "scope").unwrap(), native("healthy"));
}

#[test]
fn title_projection_is_globally_ordered_scoped_and_omits_command_and_request_payloads() {
    use super::super::agent::SessionSummary;
    let store = Conversations::memory();
    let rows: Vec<_> = (0..200)
        .map(|index| SessionSummary {
            thread: Thread {
                id: Some(SessionRef {
                    provider: if index % 2 == 0 {
                        ProviderKind::Codex
                    } else {
                        ProviderKind::Claude
                    },
                    id: format!("native-{index}"),
                }),
                name: Some(if index == 199 {
                    "Ä検索対象".into()
                } else {
                    format!("Title {index}")
                }),
                updated_at: Some(if index < 10 { 200. } else { index as f64 }),
                ..Default::default()
            },
            branch: Some(format!("branch-{index}")),
        })
        .collect();
    store.discover_page(&rows, "scope").unwrap();
    store
        .discover_page(
            &[SessionSummary {
                thread: Thread {
                    id: Some(native("inactive")),
                    name: Some("Inactive account".into()),
                    updated_at: Some(10000.),
                    ..Default::default()
                },
                branch: None,
            }],
            "inactive",
        )
        .unwrap();
    let areas = [
        (ProviderKind::Claude, "scope".into()),
        (ProviderKind::Codex, "scope".into()),
    ];
    let (page, branches) = store
        .title_list(&areas, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(page.data.len(), 5);
    assert!(page.has_more_chats);
    assert!(
        branches.len() == 6,
        "retain branches only for visible rows and their lookahead"
    );
    assert!(
        page.data
            .iter()
            .all(|thread| thread.id.as_ref().unwrap().provider == ProviderKind::Codex)
    );
    for thread in &page.data {
        let index = thread
            .name
            .as_ref()
            .unwrap()
            .strip_prefix("Title ")
            .unwrap();
        assert_eq!(
            branches.get(thread.id.as_ref().unwrap()),
            Some(&format!("branch-{index}"))
        );
    }
    let mut expected: Vec<_> = page
        .data
        .iter()
        .map(|thread| thread.id.as_ref().unwrap())
        .collect();
    expected.sort();
    assert_eq!(
        page.data
            .iter()
            .map(|thread| thread.id.as_ref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        !page
            .data
            .iter()
            .any(|thread| thread.name.as_deref() == Some("Inactive account"))
    );
    let query = agent_protocol::models::ListQuery {
        chat_limit: 500,
        ..Default::default()
    };
    let (all, _) = store
        .title_list(&areas, &Default::default(), &query)
        .unwrap();
    assert_eq!(all.data.len(), 200);
    assert!(!all.has_more_chats);
    assert_eq!(all.data[10].name.as_deref(), Some("Ä検索対象"));
    for pair in all.data.windows(2) {
        assert!(pair[0].updated_at >= pair[1].updated_at);
        if pair[0].updated_at == pair[1].updated_at {
            assert!(pair[0].id < pair[1].id);
        }
    }
    let target = page.data[0].id.as_ref().unwrap();
    let command = input(target);
    store.admit(&command, SubmissionDelivery::Queued).unwrap();
    let request = agent_protocol::requests::Request {
        id: "request".into(),
        target: agent_protocol::requests::RequestTarget::Session,
        delivery: agent_protocol::session::RequestDelivery::Awaiting,
        body: agent_protocol::requests::RequestBody::Permission {
            description: "Fixture approval".into(),
            details: "Request payload belongs to the conversation detail".into(),
            choices: Vec::new(),
        },
    };
    store
        .apply(
            target,
            &[SessionChange::Request {
                request: request.clone(),
            }],
        )
        .unwrap();
    let (page, _) = store
        .title_list(&areas, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(page.data[0].id.as_ref(), Some(target));
    assert!(page.data.iter().all(|thread| thread.turns.is_none()
        && thread.requests.is_empty()
        && thread.submissions.is_empty()
        && thread.queued_inputs.is_empty()));
    let detail = store.open_thread(target, 5, false).unwrap().thread;
    assert_eq!(detail.submissions.len(), 1);
    assert_eq!(
        detail.requests.get(&request.id).map(Arc::as_ref),
        Some(&request)
    );
    let query = agent_protocol::models::ListQuery {
        search_term: "ä検索".into(),
        ..Default::default()
    };
    let (filtered, _) = store
        .title_list(&areas, &Default::default(), &query)
        .unwrap();
    assert_eq!(filtered.data.len(), 1);
    assert_eq!(filtered.data[0].name.as_deref(), Some("Ä検索対象"));
}

#[test]
fn pending_imports_are_bounded_scoped_and_resume_after_committed_pages() {
    let store = Conversations::memory();
    let targets: Vec<_> = (0..70)
        .map(|index| {
            store
                .bind(&native(&format!("source-{index}")), "scope")
                .unwrap()
        })
        .collect();
    for index in [2, 31, 69] {
        first_page(&store, &targets[index], vec![], None);
    }
    store
        .import_failed(&targets[4], "a recoverable source read error")
        .unwrap();
    store.bind(&native("other-scope"), "different").unwrap();
    store
        .bind(
            &SessionRef {
                provider: ProviderKind::Claude,
                id: "other-provider".into(),
            },
            "scope",
        )
        .unwrap();
    let mut after = 0;
    let mut found = Vec::new();
    loop {
        let page = store
            .pending_imports(ProviderKind::Codex, "scope", after)
            .unwrap();
        assert!(page.len() <= 32);
        if page.is_empty() {
            break;
        }
        for (position, target) in page {
            assert!(position > after);
            assert_eq!(target.provider, ProviderKind::Codex);
            found.push(target);
            after = position;
        }
    }
    let expected: Vec<_> = targets
        .into_iter()
        .enumerate()
        .filter_map(|(index, target)| (![2, 31, 69].contains(&index)).then_some(target))
        .collect();
    assert_eq!(found, expected);
    let first = store
        .pending_imports(ProviderKind::Codex, "scope", 0)
        .unwrap();
    assert_eq!(first.len(), 32);
    assert_eq!(first[0].1, expected[0]);
    assert!(
        first.iter().any(|(_, target)| target == &expected[3]),
        "a new pass retries failed imports"
    );
    let appended = store.bind(&native("appended"), "scope").unwrap();
    let page = store
        .pending_imports(ProviderKind::Codex, "scope", after)
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].1, appended);
}

#[test]
fn manual_titles_survive_discovery_import_and_provider_auto_titles() {
    let store = Conversations::memory();
    let source = Thread {
        id: Some(native("source")),
        name: Some("Source title".into()),
        ..Default::default()
    };
    store
        .discover_page(
            &[super::super::agent::SessionSummary {
                thread: source.clone(),
                branch: None,
            }],
            "scope",
        )
        .unwrap();
    let target = store.bind(source.id.as_ref().unwrap(), "scope").unwrap();
    store.rename(&target, "My title", true).unwrap();
    let renamed_at = store
        .open_thread(&target, 5, false)
        .unwrap()
        .thread
        .updated_at;
    store
        .discover_page(
            &[super::super::agent::SessionSummary {
                thread: source.clone(),
                branch: None,
            }],
            "scope",
        )
        .unwrap();
    first_page(&store, &target, vec![], None);
    store.rename(&target, "Provider auto title", false).unwrap();
    let metadata = store.open_thread(&target, 5, false).unwrap().thread;
    assert_eq!(metadata.name.as_deref(), Some("My title"));
    assert_eq!(metadata.updated_at, renamed_at);
    assert!(store.rename(&target, "   ", true).is_err());
    store.rename(&target, "Revised", true).unwrap();
    assert_eq!(
        store
            .title_list(
                &[(ProviderKind::Codex, "scope".into())],
                &Default::default(),
                &agent_protocol::models::ListQuery {
                    search_term: "revised".into(),
                    ..Default::default()
                }
            )
            .unwrap()
            .0
            .data[0]
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
    store
        .discover_page(
            &[super::super::agent::SessionSummary {
                thread: recent_source,
                branch: None,
            }],
            "scope",
        )
        .unwrap();
    let recent = store.bind(&native("recent"), "scope").unwrap();
    let target = store.bind(&native("old"), "scope").unwrap();
    first_page(&store, &target, vec![turn("old-turn", "old")], None);
    assert_eq!(
        store
            .title_list(
                &[(ProviderKind::Codex, "scope".into())],
                &Default::default(),
                &Default::default()
            )
            .unwrap()
            .0
            .data[0]
            .id
            .as_ref(),
        Some(&recent)
    );
    store
        .admit(&input(&target), SubmissionDelivery::Sending)
        .unwrap();
    assert_eq!(
        store
            .title_list(
                &[(ProviderKind::Codex, "scope".into())],
                &Default::default(),
                &Default::default()
            )
            .unwrap()
            .0
            .data[0]
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
    assert_eq!(
        store.admit(&input, SubmissionDelivery::Sending).unwrap(),
        None
    );
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
    assert_eq!(
        store.admit(&input, SubmissionDelivery::Sending).unwrap(),
        Some(accepted)
    );
    let mut changed = input.clone();
    changed.effort = Some("high".into());
    assert!(store.admit(&changed, SubmissionDelivery::Sending).is_err());
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
    store.admit(&input, SubmissionDelivery::Sending).unwrap();
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
    store
        .admit(&input(&target), SubmissionDelivery::Sending)
        .unwrap();
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
        store
            .admit(&input(&target), SubmissionDelivery::Sending)
            .unwrap(),
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

#[test]
fn held_queue_keeps_edits_order_and_admission_identity_across_restart() {
    use agent_protocol::queue::QueueAction;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("queue.sqlite");
    let store = Conversations::open(&path).unwrap();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    let originals = ["first", "second", "third"].map(|id| {
        let mut input = input(&target);
        input.client_user_message_id = id.into();
        input
            .input
            .push(agent_protocol::operations::Input::Text { text: id.into() });
        input
            .input
            .push(agent_protocol::operations::Input::LocalImage {
                path: "/isolated/image.png".into(),
            });
        input
    });
    for input in &originals {
        store.admit(input, SubmissionDelivery::Queued).unwrap();
    }
    store.queue_control(&target, &QueueAction::Pause).unwrap();
    store
        .queue_control(
            &target,
            &QueueAction::Move {
                id: "third".into(),
                before: Some("first".into()),
            },
        )
        .unwrap();
    store
        .queue_control(
            &target,
            &QueueAction::Edit {
                id: "third".into(),
                text: "edited message".into(),
            },
        )
        .unwrap();
    assert!(store.claim_queued(&target).unwrap().is_none());
    drop(store);
    let store = Conversations::open(&path).unwrap();
    let thread = store.open_thread(&target, 5, false).unwrap().thread;
    assert!(thread.queue_held);
    assert_eq!(
        thread
            .queued_inputs
            .iter()
            .map(|entry| entry.submission.client_user_message_id.as_str())
            .collect::<Vec<_>>(),
        ["third", "first", "second"]
    );
    assert_eq!(
        thread.queued_inputs[0].submission.input[0],
        agent_protocol::operations::Input::Text {
            text: "edited message".into()
        }
    );
    assert_eq!(
        thread.queued_inputs[0].submission.input[1],
        originals[2].input[1]
    );
    assert_eq!(
        store.previous_command(&originals[2]).unwrap(),
        Some(SubmissionDelivery::Queued)
    );
    assert!(
        store
            .previous_command(&thread.queued_inputs[0].submission)
            .is_err(),
        "editing must not replace the deduplication fingerprint"
    );
    assert!(store.claim_queued(&target).unwrap().is_none());
    store.queue_control(&target, &QueueAction::Resume).unwrap();
    let sent = store.claim_queued(&target).unwrap().unwrap();
    assert_eq!(sent.client_user_message_id.as_str(), "third");
    assert!(
        store
            .queue_control(
                &target,
                &QueueAction::Edit {
                    id: "third".into(),
                    text: "must not change a claimed input".into()
                }
            )
            .is_err()
    );
    drop(store);
    let store = Conversations::open(&path).unwrap();
    assert_eq!(
        store.previous_command(&originals[2]).unwrap(),
        Some(SubmissionDelivery::Unknown)
    );
    let thread = store.open_thread(&target, 5, false).unwrap().thread;
    assert!(thread.queue_held);
    assert!(
        thread
            .queued_inputs
            .iter()
            .any(
                |entry| entry.submission.client_user_message_id == sent.client_user_message_id
                    && entry.delivery == SubmissionDelivery::Unknown
            ),
        "the unconfirmed input and its text remain visible"
    );
    assert_eq!(
        store
            .queued(&target)
            .unwrap()
            .iter()
            .map(|input| input.client_user_message_id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
}

#[test]
fn importing_a_live_body_keeps_newer_stream_updates_and_the_activity_date() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    let full = turn("run", "full native output").items.as_ref().unwrap()[0]
        .as_ref()
        .clone();
    let mut previous = full.clone();
    previous.defer();
    let turn_id: agent_protocol::ids::TurnId = "run".into();
    store
        .apply(
            &target,
            [&SessionChange::Turn {
                turn: Turn {
                    id: turn_id.clone(),
                    status: TurnStatus::Running,
                    items: Some(vec![Arc::new(previous.clone())]),
                    ..Default::default()
                },
                completed: false,
            }],
        )
        .unwrap();
    let date = store
        .open_thread(&target, 5, true)
        .unwrap()
        .thread
        .updated_at;
    assert!(
        store
            .hydrate_item(&target, &turn_id, &previous, &full)
            .unwrap()
    );
    let thread = store.open_thread(&target, 5, true).unwrap().thread;
    assert_eq!(thread.updated_at, date);
    assert_eq!(
        thread.turns.unwrap()[0].items.as_ref().unwrap()[0].as_ref(),
        &full
    );
    let mut newest = turn("run", "newer stream summary").items.as_ref().unwrap()[0]
        .as_ref()
        .clone();
    newest.defer();
    store
        .apply(
            &target,
            [&SessionChange::Item {
                turn_id: turn_id.clone(),
                item: Arc::new(newest.clone()),
            }],
        )
        .unwrap();
    let events: i64 = store
        .lock()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert!(
        !store
            .hydrate_item(&target, &turn_id, &previous, &full)
            .unwrap()
    );
    assert_eq!(
        store.turn(&target, &turn_id).unwrap().items.unwrap()[0].as_ref(),
        &newest
    );
    assert_eq!(
        store
            .lock()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        events
    );
}

#[test]
fn queue_mutations_are_atomic_and_cancellation_releases_only_the_selected_message() {
    use agent_protocol::queue::QueueAction;
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    let first = input(&target);
    let mut second = first.clone();
    second.client_user_message_id = "second".into();
    for input in [&first, &second] {
        store.admit(input, SubmissionDelivery::Queued).unwrap();
    }
    let before = store.queued(&target).unwrap();
    store.lock().execute_batch("CREATE TRIGGER reject_queue_journal BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'isolated storage failure'); END;").unwrap();
    assert!(
        store
            .queue_control(
                &target,
                &QueueAction::Move {
                    id: "second".into(),
                    before: Some(first.client_user_message_id.clone())
                }
            )
            .is_err()
    );
    assert!(
        store
            .queue_control(
                &target,
                &QueueAction::Edit {
                    id: "second".into(),
                    text: "changed".into()
                }
            )
            .is_err()
    );
    assert!(store.claim_queued(&target).is_err());
    assert_eq!(store.queued(&target).unwrap(), before);
    store
        .lock()
        .execute_batch("DROP TRIGGER reject_queue_journal;")
        .unwrap();
    store
        .queue_control(
            &target,
            &QueueAction::Cancel {
                id: first.client_user_message_id.clone(),
            },
        )
        .unwrap();
    assert_eq!(
        store.previous_command(&first).unwrap(),
        Some(SubmissionDelivery::Rejected)
    );
    let thread = store.open_thread(&target, 5, false).unwrap().thread;
    assert_eq!(thread.queued_inputs.len(), 1);
    assert_eq!(store.claim_queued(&target).unwrap(), Some(second));
}

#[test]
fn queue_capacity_counts_waiting_inputs_and_releases_claimed_slots() {
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    first_page(&store, &target, vec![], None);
    assert!(!store.has_queued(&target).unwrap());
    for position in 0..128 {
        let mut input = input(&target);
        input.client_user_message_id = position.to_string().into();
        store.admit(&input, SubmissionDelivery::Queued).unwrap();
    }
    assert!(store.has_queued(&target).unwrap());
    let mut next = input(&target);
    next.client_user_message_id = "next".into();
    assert!(store.admit(&next, SubmissionDelivery::Queued).is_err());
    assert!(store.previous_command(&next).unwrap().is_none());
    let claimed = store.claim_queued(&target).unwrap().unwrap();
    assert_eq!(claimed.client_user_message_id.as_str(), "0");
    store.admit(&next, SubmissionDelivery::Queued).unwrap();
    assert_eq!(store.queued(&target).unwrap().len(), 128);
    assert_eq!(store.queued(&target).unwrap().last(), Some(&next));
    assert!(
        store
            .admit(&claimed, SubmissionDelivery::Queued)
            .unwrap()
            .is_some()
    );
    assert_eq!(store.queued(&target).unwrap().len(), 128);
}

#[test]
fn queue_discard_never_turns_an_uncertain_write_into_a_safe_retry() {
    use agent_protocol::queue::QueueAction;
    let store = Conversations::memory();
    let target = store.bind(&native("source"), "scope").unwrap();
    let input = input(&target);
    store.admit(&input, SubmissionDelivery::Queued).unwrap();
    let date = store
        .open_thread(&target, 1, false)
        .unwrap()
        .thread
        .updated_at;
    first_page(&store, &target, vec![], None);
    let thread = store.open_thread(&target, 1, false).unwrap().thread;
    assert_eq!(thread.updated_at, date);
    assert_eq!(
        thread.submissions[&input.client_user_message_id],
        SubmissionDelivery::Queued
    );
    store.claim_queued(&target).unwrap().unwrap();
    let cancel = QueueAction::Cancel {
        id: input.client_user_message_id.clone(),
    };
    assert!(store.queue_control(&target, &cancel).is_err());
    store.recover().unwrap();
    assert_eq!(
        store.previous_command(&input).unwrap(),
        Some(SubmissionDelivery::Unknown)
    );
    store.queue_control(&target, &cancel).unwrap();
    assert!(
        store
            .open_thread(&target, 1, false)
            .unwrap()
            .thread
            .queued_inputs
            .is_empty()
    );
    assert_eq!(
        store.previous_command(&input).unwrap(),
        Some(SubmissionDelivery::Unknown)
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
