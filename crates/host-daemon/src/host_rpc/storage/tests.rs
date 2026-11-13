use super::*;
use agent_protocol::{
    execution::ItemStatus,
    items::{AssistantPhase, ItemBody},
    operations::{FollowUp, Input},
};
use decision::OperationId;
use decision::{
    Binding, Command, InputContent, InputState, Observation, Thread, TurnState, Workspace,
    records::Details,
};

fn address(name: &str) -> SessionRef {
    SessionRef::new(ProviderKind::Claude, name.into()).unwrap()
}
fn context(seed: u128) -> Context<'static> {
    Context {
        preparation: None,
        rewind_binding: None,
        imported_turns: &[],
        external_busy: false,
        now: 100,
        seed: Uuid::from_u128(seed),
        steers: true,
        steer_ready: true,
    }
}
fn header() -> Header {
    Header {
        state: Thread {
            name: "Conversation".into(),
            workspace: Workspace::Ready("/project".into()),
            provider: ProviderKind::Claude,
            queue_held: false,
        },
        details: Details::default(),
    }
}

#[test]
fn forks_settle_copied_background_work_and_allow_the_next_input() {
    use agent_protocol::{
        conversation::{Turn, Wait},
        execution::{BackgroundTask, BackgroundTaskKind},
    };
    for (later, answering) in [(false, false), (true, false), (false, true)] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let source = address("background-source");
        storage
            .import(
                &source,
                &agent_protocol::models::ThreadResponse {
                    thread: agent_protocol::models::Thread {
                        id: Some(source.clone()),
                        cwd: Some("/project".into()),
                        turns: Some(vec![Arc::new(agent_protocol::models::Turn {
                            id: "answered".into(),
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                Some(&source),
                100,
            )
            .unwrap();
        storage
            .with(|connection| {
                let transaction = connection.transaction().map_err(failure)?;
                let key = thread_key(&transaction, &source)?;
                let scope = rows::WriteScope {
                    key,
                    id: &source,
                    sequence: 1,
                    at: 100,
                    process: storage.process,
                };
                let background = Turn {
                    id: "answered".into(),
                    ordinal: 1,
                    provider: ProviderKind::Claude,
                    state: if answering {
                        TurnState::Running { interrupt: None }
                    } else {
                        TurnState::Waiting(Wait::Background {
                            tasks: vec![BackgroundTask {
                                id: "shell".into(),
                                kind: BackgroundTaskKind::Command,
                                description: None,
                            }],
                        })
                    },
                    queued: false,
                    observed: true,
                    started_at: Some(10),
                    completed_at: None,
                    duration_ms: None,
                    retry_count: 0,
                    compaction: false,
                    error: None,
                };
                let mut changes = vec![
                    Change::Turn(background.clone()),
                    Change::Item(decision::Item {
                        id: "command".into(),
                        turn: background.id.clone(),
                        position: 1,
                        status: ItemStatus::Running,
                        deferred: false,
                        input: None,
                        body: ItemBody::CommandExecution {
                            command: "sleep 1".into(),
                            cwd: None,
                            output: "kept background output".into(),
                            exit_code: None,
                            output_omitted: false,
                        },
                    }),
                ];
                if later {
                    changes.push(Change::Turn(Turn {
                        id: "later".into(),
                        ordinal: 2,
                        state: TurnState::Completed,
                        ..background
                    }));
                }
                let state = State::new(header().state).apply(&changes);
                for change in changes {
                    rows::write(
                        &transaction,
                        &scope,
                        &change,
                        &state.turns,
                        &mut Vec::new(),
                        &mut Vec::new(),
                    )?;
                }
                transaction.commit().map_err(failure)
            })
            .unwrap();
        let before = storage.history(&source, 0).unwrap();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let forked = storage.fork(
            &source,
            &target,
            &if later { "later" } else { "answered" }.into(),
            200,
        );
        if answering {
            assert_eq!(
                forked.unwrap_err(),
                "finish the selected turn before forking"
            );
            assert!(storage.lookup(&target).unwrap().is_none());
            continue;
        }
        forked.unwrap();
        let copied = storage.state(&target).unwrap();
        assert!(copied.active_turn().is_none());
        assert!(copied.bindings.is_empty());
        let history = storage.history(&target, 0).unwrap();
        let turns = history.thread.turns.unwrap();
        assert_eq!(turns.len(), if later { 2 } else { 1 });
        assert!(
            turns
                .iter()
                .all(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
        );
        let item = &turns[0].items.as_ref().unwrap()[0];
        assert_eq!(item.status, ItemStatus::Completed);
        assert!(
            matches!(item.body(), ItemBody::CommandExecution { output, .. } if output == "kept background output")
        );
        assert_eq!(
            storage.history(&source, 0).unwrap().thread.turns,
            before.thread.turns
        );
        let operation = Uuid::new_v4();
        let next = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(operation),
                    receipt: None,
                    command: Command::Submit {
                        id: operation.to_string().into(),
                        content: InputContent {
                            parts: vec![Input::Text {
                                text: "continue the copy".into(),
                            }],
                            model: None,
                            effort: None,
                            service_tier: None,
                            follow_up: FollowUp::Queue,
                        },
                    },
                },
                context(1000),
                None,
            )
            .unwrap();
        assert_eq!(next.step.outcome, Outcome::Accepted);
        assert!(
            next.step
                .actions
                .iter()
                .any(|action| matches!(action, decision::Action::Prepare { .. }))
        );
    }
}

#[test]
fn unknown_native_activity_stays_unknown_until_the_current_source_confirms_idle() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("unknown-native");
    let response = agent_protocol::models::ThreadResponse {
        thread: agent_protocol::models::Thread {
            id: Some(id.clone()),
            status: agent_protocol::models::SessionStatus::Unknown,
            cwd: Some("/project".into()),
            turns: Some(Vec::new()),
            ..Default::default()
        },
        ..Default::default()
    };
    storage.import(&id, &response, Some(&id), 100).unwrap();
    storage
        .decide(&id, Event::Title("Known title".into()), context(1), None)
        .unwrap();
    assert_eq!(
        storage.header(&id).unwrap().details.activity.status(),
        agent_protocol::models::SessionStatus::Unknown
    );
    assert!(storage.activity().unwrap().display.ongoing);
    storage
        .decide(
            &id,
            Event::NativeStatus {
                provider: ProviderKind::Claude,
                source: id.id.clone(),
                status: agent_protocol::models::SessionStatus::Idle,
            },
            context(2),
            None,
        )
        .unwrap();
    assert_eq!(
        storage.header(&id).unwrap().details.activity.status(),
        agent_protocol::models::SessionStatus::Idle
    );
    assert!(!storage.activity().unwrap().display.ongoing);
}

#[test]
fn importing_children_preserves_unlisted_parents_and_resolves_owned_native_parents() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let child = address("child");
    let parent = address("unlisted-parent");
    let response = agent_protocol::models::ThreadResponse {
        thread: agent_protocol::models::Thread {
            id: Some(child.clone()),
            parent_id: Some(parent.clone()),
            turns: Some(Vec::new()),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&child, &response, Some(&child), 100)
        .unwrap();
    assert_eq!(
        storage.header(&child).unwrap().details.parent.as_ref(),
        Some(&parent)
    );
    assert!(!storage.loaded(&parent).unwrap());
    assert!(
        storage
            .summaries("", false)
            .unwrap()
            .iter()
            .all(|thread| thread.id.as_ref() != Some(&parent))
    );
    assert!(!storage.activity().unwrap().display.ongoing);

    assert_eq!(
        storage.header(&parent).unwrap().details.activity.status(),
        agent_protocol::models::SessionStatus::Unknown
    );
    let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&public, &header()).unwrap();
    storage
        .save_source(
            &public,
            ProviderKind::Claude,
            "native-parent",
            None,
            &Default::default(),
        )
        .unwrap();
    let other = address("other-child");
    let mut response = response;
    response.thread.id = Some(other.clone());
    response.thread.parent_id = Some(address("native-parent"));
    storage
        .import(&other, &response, Some(&other), 200)
        .unwrap();
    assert_eq!(
        storage.header(&other).unwrap().details.parent.as_ref(),
        Some(&public)
    );
    // A subsequently discovered reverse edge cannot create an ancestry cycle.
    response.thread.id = Some(parent.clone());
    response.thread.parent_id = Some(child.clone());
    storage
        .import(&parent, &response, Some(&parent), 300)
        .unwrap();
    assert!(storage.header(&parent).unwrap().details.parent.is_none());
    assert_eq!(
        storage.header(&child).unwrap().details.parent.as_ref(),
        Some(&parent)
    );
}

#[test]
fn list_replication_shares_committed_header_versions_and_recovers_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let id = address("listed");
    storage.create(&id, &header()).unwrap();
    let initial = storage.list(None).unwrap();
    let mut replica = decision::records::Replica::new(agent_protocol::sync::Scope::List, 1)
        .apply(1, initial)
        .unwrap();
    let before = replica.cursor().unwrap();
    let mut changes = storage.changes();
    changes.borrow_and_update();
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let sequence = Storage::advance(&transaction)?;
            transaction
                .execute(
                    "UPDATE threads SET seq=?1 WHERE id=?2",
                    params![integer(sequence.sequence)?, id.id],
                )
                .map_err(failure)?;
            Err::<(), String>("forced rollback".into())
        })
        .unwrap_err();
    assert!(!changes.has_changed().unwrap());
    assert_eq!(
        replica
            .clone()
            .apply(1, storage.list(Some(before)).unwrap())
            .unwrap(),
        replica
    );
    let committed = storage
        .decide(
            &id,
            Event::Command {
                id: OperationId::Client(Uuid::new_v4()),
                receipt: None,
                command: Command::Rename("Renamed".into()),
            },
            context(90),
            None,
        )
        .unwrap();
    assert!(changes.has_changed().unwrap());
    replica = replica
        .apply(1, storage.list(Some(before)).unwrap())
        .unwrap();
    let row = &replica.rows()[&Key::Thread(id.clone())];
    assert_eq!(row.version, committed.cursor.sequence);
    assert!(
        matches!(&row.content,Content::Full(record) | Content::Summary(record) if matches!(record.as_ref(),Record::Thread(header) if header.state.name=="Renamed"))
    );
    assert_eq!(replica.cursor(), Some(committed.cursor));
    drop(storage);
    let reopened = Storage::open(&path);
    assert_eq!(
        replica
            .clone()
            .apply(1, reopened.list(Some(before)).unwrap())
            .unwrap(),
        replica
    );
    let foreign = Cursor {
        store: Uuid::new_v4(),
        ..before
    };
    assert!(matches!(
        reopened.list(Some(foreign)).unwrap(),
        Message::Snapshot { window: None, .. }
    ));
}

#[test]
fn list_catchup_over_one_thousand_rows_resets_to_a_complete_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let initial = storage.list(None).unwrap();
    let replica = decision::records::Replica::new(agent_protocol::sync::Scope::List, 1)
        .apply(1, initial)
        .unwrap();
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let sequence = Storage::advance(&transaction)?.sequence;
            for index in 0..1001 {
                projects::write(
                    &transaction,
                    &agent_protocol::models::Project {
                        id: format!("project-{index}"),
                        name: format!("Project {index}"),
                        ..Default::default()
                    },
                    sequence,
                )?;
            }
            transaction.commit().map_err(failure)
        })
        .unwrap();
    let message = storage.list(replica.cursor()).unwrap();
    assert!(matches!(&message,Message::Snapshot {window:None,rows,..} if rows.len()==1001));
    assert_eq!(replica.apply(1, message).unwrap().rows().len(), 1001);
}

#[test]
fn catalog_claims_survive_restart_and_do_not_restore_superseded_native_sources() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    storage.queue_catalog(ProviderKind::Claude, 0).unwrap();
    let obsolete = storage.claim(0, 1).unwrap().remove(0);
    drop(storage);
    let storage = Storage::open(&path);
    let current = storage.claim(0, 1).unwrap().remove(0);
    assert_eq!(current.id, obsolete.id);
    assert!(current.attempt > obsolete.attempt);
    let id = address("discovered");
    let child = address("child");
    let mut child_header = header();
    child_header.details.parent = Some(id.clone());
    let headers = [(child.clone(), child_header), (id.clone(), header())];
    assert!(storage.catalog_page(&obsolete, &headers, None, 0).is_err());
    assert!(storage.lookup(&id).unwrap().is_none());
    storage.catalog_page(&current, &headers, None, 0).unwrap();
    assert_eq!(
        storage.header(&child).unwrap().details.parent,
        Some(id.clone())
    );
    assert!(!storage.loaded(&id).unwrap());
    let owner = address("owned");
    storage.create(&owner, &header()).unwrap();
    ready(&storage, &owner);
    let active = address("native-owned");
    let current = storage.claim(60_000, 1).unwrap().remove(0);
    storage
        .catalog_page(&current, &[(active.clone(), header())], None, 60_000)
        .unwrap();
    assert!(storage.lookup(&active).unwrap().is_none());
    assert_eq!(storage.native_owner(&active).unwrap(), Some(owner));
}
fn submit(seed: u128) -> Event {
    Event::Command {
        id: OperationId::Client(Uuid::from_u128(seed)),
        receipt: None,
        command: Command::Submit {
            id: Uuid::from_u128(seed).to_string().into(),
            content: InputContent {
                parts: vec![Input::Text {
                    text: "hello".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: FollowUp::Queue,
            },
        },
    }
}
fn ready(storage: &Storage, id: &SessionRef) {
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, id)?;
            let mut state = rows::load(&transaction, key, storage.process)?;
            let binding = Change::Binding {
                provider: state.thread.provider,
                value: Some(Binding::Ready {
                    covered: state.history_end(),
                    source: format!("native-{}", id.id),
                }),
            };
            state = state.apply(std::slice::from_ref(&binding));
            rows::write(
                &transaction,
                &rows::WriteScope {
                    key,
                    id,
                    sequence: 0,
                    at: 0,
                    process: storage.process,
                },
                &binding,
                &state.turns,
                &mut Vec::new(),
                &mut Vec::new(),
            )?;
            transaction.commit().map_err(failure)
        })
        .unwrap();
}

#[test]
fn admission_receipt_and_rows_commit_together_and_replay_does_not_allocate_again() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("one");
    assert!(storage.create(&id, &header()).unwrap());
    ready(&storage, &id);
    let first = storage.decide(&id, submit(1), context(1), None).unwrap();
    assert_eq!(first.step.outcome, Outcome::Accepted);
    let admitted_state = storage.state(&id).unwrap();
    assert_eq!(admitted_state.inputs.len(), 1);
    assert!(admitted_state.turns.is_empty());
    let keys = first
        .rows
        .iter()
        .map(|row| &row.key)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), first.rows.len());
    let replay = storage.decide(&id, submit(1), context(2), None).unwrap();
    assert_eq!(replay.cursor, first.cursor);
    assert_eq!(storage.state(&id).unwrap(), admitted_state);
    assert!(replay.step.actions.is_empty());
    assert!(replay.rows.is_empty());
    assert!(replay.removed.is_empty());
    let other = address("two");
    storage.create(&other, &header()).unwrap();
    assert!(
        storage
            .decide(&other, submit(1), context(3), None)
            .unwrap_err()
            .contains("another conversation")
    );
}

#[test]
fn one_day_removal_retention_resets_old_cursors_and_keeps_command_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("retention");
    let mut admitted_at = context(1);
    admitted_at.external_busy = true;
    storage
        .decide(&target, submit(1), admitted_at, Some(&header()))
        .unwrap();
    let Message::Snapshot {
        cursor: before,
        window,
        ..
    } = storage.snapshot(&target, None, None).unwrap()
    else {
        panic!("snapshot")
    };
    let removed = storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(2)),
                receipt: None,
                command: Command::RemoveInput(Uuid::from_u128(1).to_string().into()),
            },
            context(2),
            None,
        )
        .unwrap();
    assert_eq!(removed.removed.len(), 1);
    let day = 24 * 60 * 60 * 1000;
    storage.claim(100 + day - 1, 0).unwrap();
    assert!(
        matches!(storage.snapshot(&target,Some(before),window).unwrap(),Message::Changes {removed,..} if removed.len()==1)
    );
    storage.claim(100 + day, 0).unwrap();
    let Message::Snapshot { rows, .. } = storage.snapshot(&target, Some(before), window).unwrap()
    else {
        panic!("an expired deletion requires a replacing snapshot")
    };
    assert!(!rows.iter().any(|row| matches!(row.key, Key::Input { .. })));
    assert!(
        matches!(storage.snapshot(&target,Some(removed.cursor),window).unwrap(),Message::Changes {removed,..} if removed.is_empty())
    );
    assert_eq!(
        storage
            .receipts(&target, &[Uuid::from_u128(1), Uuid::from_u128(2)])
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        storage
            .decide(&target, submit(1), context(3), None)
            .unwrap()
            .cursor,
        removed.cursor
    );
}

#[test]
fn creation_and_first_input_are_one_recoverable_transaction() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let before = storage.with(|connection| cursor(connection)).unwrap();
    storage.with(|connection| connection.execute_batch("CREATE TRIGGER reject_initial_input BEFORE INSERT ON inputs BEGIN SELECT RAISE(ABORT,'initial input failed'); END").map_err(failure)).unwrap();
    assert!(
        storage
            .decide(&id, submit(800), context(800), Some(&header()))
            .unwrap_err()
            .contains("initial input failed")
    );
    assert!(storage.lookup(&id).unwrap().is_none());
    assert!(
        storage
            .receipts(&id, &[Uuid::from_u128(800)])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        storage.with(|connection| cursor(connection)).unwrap(),
        before
    );
    storage
        .with(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_initial_input")
                .map_err(failure)
        })
        .unwrap();
    let created = storage
        .decide(&id, submit(800), context(800), Some(&header()))
        .unwrap();
    assert_eq!(created.step.outcome, Outcome::Accepted);
    assert_eq!(storage.state(&id).unwrap().inputs.len(), 1);
    assert!(
        created
            .rows
            .iter()
            .all(|row| row.version == created.cursor.sequence)
    );
    assert_eq!(
        storage.receipts(&id, &[Uuid::from_u128(800)]).unwrap()[0]
            .1
            .cursor,
        created.cursor
    );
    let replay = storage
        .decide(&id, submit(800), context(801), Some(&header()))
        .unwrap();
    assert!(replay.step.actions.is_empty());
    assert!(replay.rows.is_empty());
    assert_eq!(replay.cursor, created.cursor);
}

#[test]
fn providerless_and_native_addresses_keep_separate_rows_receipts_and_parents() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let value = Uuid::new_v4().to_string();
    let public = SessionRef::new(None, value.clone()).unwrap();
    let native = SessionRef::new(ProviderKind::Claude, value).unwrap();
    assert!(storage.create(&public, &header()).unwrap());
    let mut child = header();
    child.details.parent = Some(public.clone());
    assert!(storage.create(&native, &child).unwrap());
    storage
        .decide(&public, submit(700), context(700), None)
        .unwrap();
    assert!(
        storage
            .receipts(&native, &[Uuid::from_u128(700)])
            .unwrap()
            .is_empty()
    );
    assert!(
        storage
            .decide(&native, submit(700), context(701), None)
            .unwrap_err()
            .contains("another conversation")
    );
    assert_eq!(
        storage.header(&native).unwrap().details.parent,
        Some(public.clone())
    );
    assert_eq!(
        storage.identities().unwrap(),
        vec![public.clone(), native.clone()]
    );
    assert_eq!(storage.headers(false).unwrap().len(), 2);
    drop(storage);
    let reopened = Storage::open(&path);
    assert_eq!(
        reopened.header(&native).unwrap().details.parent,
        Some(public.clone())
    );
    assert_eq!(
        reopened
            .receipts(&public, &[Uuid::from_u128(700)])
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_reopened_store_fences_in_flight_delivery_without_replaying_the_native_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let id = address("one");
    let storage = Storage::open(&path);
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    let first = storage.decide(&id, submit(1), context(1), None).unwrap();
    let action = first.step.actions[0].clone();
    drop(storage);
    let reopened = Storage::open(&path);
    let recovery = reopened
        .decide(
            &id,
            Event::Restart {
                updating: false,
                continue_after_restart: false,
            },
            context(2),
            None,
        )
        .unwrap();
    assert!(recovery.step.actions.is_empty());
    assert!(!reopened.state(&id).unwrap().permits(&action));
    assert!(matches!(
        reopened
            .state(&id)
            .unwrap()
            .inputs
            .values()
            .next()
            .unwrap()
            .state,
        InputState::Unknown {
            status: decision::UnknownState::Fenced,
            ..
        }
    ));
    assert_eq!(
        reopened
            .decide(&id, submit(1), context(3), None)
            .unwrap()
            .step
            .outcome,
        Outcome::Accepted
    );
}

#[test]
fn a_late_item_update_keeps_its_original_position_and_does_not_reopen_the_turn() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("one");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    let first = storage.decide(&id, submit(1), context(1), None).unwrap();
    let decision::Action::Deliver {
        id: input,
        action,
        attempt,
        ..
    } = &first.step.actions[0]
    else {
        panic!("delivery")
    };
    storage
        .decide(
            &id,
            Event::Delivered {
                native_turn: None,
                id: input.clone(),
                action: *action,
            },
            context(2),
            None,
        )
        .unwrap();
    let observation = |text: &str| Observation::Item {
        deferred: false,
        id: "reply".into(),
        status: ItemStatus::Running,
        body: ItemBody::AssistantText {
            text: text.into(),
            phase: AssistantPhase::Final,
        },
    };
    storage
        .decide(
            &id,
            Event::Provider {
                native_items: Vec::new(),
                native_turn: None,
                attempt: attempt.id,
                observation: observation("first"),
            },
            context(3),
            None,
        )
        .unwrap();
    let position = storage
        .item(&id, &"reply".into())
        .unwrap()
        .unwrap()
        .position;
    storage
        .decide(
            &id,
            Event::Provider {
                native_items: Vec::new(),
                native_turn: None,
                attempt: attempt.id,
                observation: Observation::Completed,
            },
            context(4),
            None,
        )
        .unwrap();
    storage
        .decide(
            &id,
            Event::Provider {
                native_items: Vec::new(),
                native_turn: None,
                attempt: attempt.id,
                observation: observation("last"),
            },
            context(5),
            None,
        )
        .unwrap();
    assert_eq!(
        storage
            .item(&id, &"reply".into())
            .unwrap()
            .unwrap()
            .position,
        position
    );
    assert_eq!(
        storage.state(&id).unwrap().turns[&attempt.turn].state,
        TurnState::Completed
    );
    let removal = Event::Provider {
        attempt: attempt.id,
        native_turn: None,
        native_items: vec![],
        observation: Observation::RemoveItem("reply".into()),
    };
    let removed = storage
        .decide(&id, removal.clone(), context(6), None)
        .unwrap();
    assert!(storage.item(&id, &"reply".into()).unwrap().is_none());
    assert_eq!(
        removed.removed,
        vec![Removal {
            key: Key::Item {
                thread: id.clone(),
                id: "reply".into()
            },
            version: removed.cursor.sequence
        }]
    );
    assert!(
        storage
            .decide(&id, removal, context(7), None)
            .unwrap()
            .removed
            .is_empty()
    );
}

#[test]
fn a_failed_unconfirmed_delivery_is_fenced_but_a_failed_steer_keeps_its_turn_live() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("delivery-lifetime");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    for confirmed in [false, true] {
        let seed = if confirmed { 20 } else { 10 };
        let admitted = storage
            .decide(
                &id,
                submit(if confirmed { 2 } else { 1 }),
                context(seed),
                None,
            )
            .unwrap();
        let decision::Action::Deliver {
            id: input,
            action,
            attempt,
            ..
        } = &admitted.step.actions[0]
        else {
            panic!("delivery missing")
        };
        assert!(
            storage
                .execution(&id, attempt.id)
                .unwrap()
                .unwrap()
                .accepting
        );
        assert!(storage.state(&id).unwrap().turns.is_empty());
        if confirmed {
            storage
                .decide(
                    &id,
                    Event::Delivered {
                        id: input.clone(),
                        action: *action,
                        native_turn: Some("native-turn".into()),
                    },
                    context(seed + 1),
                    None,
                )
                .unwrap();
            let mut steer = submit(3);
            if let Event::Command {
                command: Command::Submit { content, .. },
                ..
            } = &mut steer
            {
                content.follow_up = FollowUp::Steer;
            }
            let admitted = storage.decide(&id, steer, context(seed + 2), None).unwrap();
            let decision::Action::Deliver {
                id: input,
                action,
                attempt: steer_attempt,
                ..
            } = &admitted.step.actions[0]
            else {
                panic!("steer missing")
            };
            assert_eq!(steer_attempt.id, attempt.id);
            storage
                .decide(
                    &id,
                    Event::DeliveryFailed {
                        id: input.clone(),
                        action: *action,
                        error: agent_protocol::execution::ExecutionError {
                            message: "rejected steer".into(),
                            ..Default::default()
                        },
                        unknown: false,
                    },
                    context(seed + 3),
                    None,
                )
                .unwrap();
            assert!(
                storage
                    .execution(&id, attempt.id)
                    .unwrap()
                    .unwrap()
                    .accepting
            );
        } else {
            storage
                .decide(
                    &id,
                    Event::DeliveryFailed {
                        id: input.clone(),
                        action: *action,
                        error: agent_protocol::execution::ExecutionError {
                            message: "not accepted".into(),
                            ..Default::default()
                        },
                        unknown: false,
                    },
                    context(seed + 1),
                    None,
                )
                .unwrap();
            assert!(
                !storage
                    .execution(&id, attempt.id)
                    .unwrap()
                    .unwrap()
                    .accepting
            );
            assert!(
                storage
                    .running_sources(ProviderKind::Claude)
                    .unwrap()
                    .is_empty()
            );
            let before = storage.history(&id, 0).unwrap();
            let ignored = storage
                .decide(
                    &id,
                    Event::Provider {
                        attempt: attempt.id,
                        native_turn: Some("late".into()),
                        native_items: vec![],
                        observation: Observation::Timing {
                            started_at: Some(12),
                            completed_at: None,
                            duration_ms: None,
                        },
                    },
                    context(seed + 2),
                    None,
                )
                .unwrap();
            assert!(ignored.rows.is_empty());
            assert_eq!(storage.history(&id, 0).unwrap(), before);
        }
    }
}

#[test]
fn native_ack_and_item_bindings_commit_with_the_canonical_rows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    let admitted = storage.decide(&id, submit(1), context(1), None).unwrap();
    let decision::Action::Deliver {
        id: input,
        action,
        attempt,
        ..
    } = &admitted.step.actions[0]
    else {
        panic!("delivery missing")
    };
    storage
        .decide(
            &id,
            Event::Delivered {
                id: input.clone(),
                action: *action,
                native_turn: Some("native-turn".into()),
            },
            context(2),
            None,
        )
        .unwrap();
    storage.with(|connection| connection.execute_batch("CREATE TRIGGER fail_native_binding BEFORE INSERT ON item_natives BEGIN SELECT RAISE(ABORT,'binding failed'); END").map_err(failure)).unwrap();
    let event = Event::Provider {
        attempt: attempt.id,
        native_turn: Some("native-turn".into()),
        native_items: vec![("native-item".into(), "reply".into())],
        observation: Observation::Item {
            deferred: false,
            id: "reply".into(),
            status: ItemStatus::Completed,
            body: ItemBody::AssistantText {
                text: "answer".into(),
                phase: AssistantPhase::Final,
            },
        },
    };
    let before = storage.with(|connection| cursor(connection)).unwrap();
    assert!(
        storage
            .decide(&id, event.clone(), context(3), None)
            .unwrap_err()
            .contains("binding failed")
    );
    assert!(storage.item(&id, &"reply".into()).unwrap().is_none());
    assert_eq!(
        storage.with(|connection| cursor(connection)).unwrap(),
        before
    );
    storage
        .with(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_native_binding")
                .map_err(failure)
        })
        .unwrap();
    storage
        .decide(&id, event.clone(), context(4), None)
        .unwrap();
    let committed = storage.decide(&id, event, context(5), None).unwrap();
    assert!(committed.rows.is_empty());
    drop(storage);
    let reopened = Storage::open(&path);
    reopened
        .with(|connection| {
            let key = thread_key(connection, &id)?;
            let hint = history::fork_hint(connection, key, &attempt.turn, ProviderKind::Claude)?;
            assert_eq!(hint.unwrap().1, "native-turn");
            let item: String = connection
                .query_row(
                    "SELECT item_id FROM item_natives WHERE native_id='native-item'",
                    [],
                    |row| row.get(0),
                )
                .map_err(failure)?;
            assert_eq!(item, "reply");
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_failed_receipt_write_rolls_back_admission_and_its_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("one");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    let before=storage.with(|connection| {
        let before=cursor(connection)?;
        connection.execute_batch("CREATE TRIGGER reject_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(ABORT,'receipt write failed'); END;").map_err(failure)?;
        Ok(before)
    }).unwrap();
    assert!(storage.decide(&id, submit(1), context(1), None).is_err());
    assert!(storage.state(&id).unwrap().inputs.is_empty());
    storage
        .with(|connection| {
            assert_eq!(cursor(connection)?, before);
            connection
                .execute_batch("DROP TRIGGER reject_receipt")
                .map_err(failure)
        })
        .unwrap();
    let admitted = storage.decide(&id, submit(1), context(2), None).unwrap();
    assert_eq!(admitted.step.outcome, Outcome::Accepted);
    assert_eq!(storage.state(&id).unwrap().inputs.len(), 1);
}

#[test]
fn receipts_survive_conversation_deletion_and_rejections_are_also_replayed() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("one");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    storage.decide(&id, submit(1), context(1), None).unwrap();
    storage
        .with(|connection| {
            connection
                .execute(
                    "DELETE FROM threads WHERE provider IS ?1 AND id=?2",
                    params![id.provider.map(ProviderKind::key), id.id],
                )
                .map(|_| ())
                .map_err(failure)
        })
        .unwrap();
    let replay = storage.decide(&id, submit(1), context(2), None).unwrap();
    assert_eq!(replay.step.outcome, Outcome::Accepted);
    assert!(storage.state(&id).is_err());
    assert!(replay.step.actions.is_empty());
    let rejected = storage.decide(&id, submit(3), context(3), None).unwrap();
    assert!(matches!(rejected.step.outcome, Outcome::Rejected(_)));
    let replay = storage.decide(&id, submit(3), context(4), None).unwrap();
    assert_eq!(replay.cursor, rejected.cursor);
    assert_eq!(replay.step.outcome, rejected.step.outcome);
    assert!(replay.step.receipt.is_none());
}

fn enqueue(storage: &Storage, seed: u128, lane: &str) {
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            jobs::enqueue(
                &transaction,
                Uuid::from_u128(seed),
                lane,
                0,
                &jobs::Work::Continue {
                    target: address("deleted"),
                    source: "source".into(),
                },
            )?;
            transaction.commit().map_err(failure)
        })
        .unwrap();
}

fn receive(storage: &Storage, claim: &jobs::Claim) {
    let jobs::Work::Continue { target, source } = &claim.work else {
        panic!("expected continuation");
    };
    let Event::Command {
        command: Command::Submit { content, .. },
        ..
    } = submit(99)
    else {
        unreachable!()
    };
    let committed = storage
        .decide(
            target,
            Event::Command {
                id: OperationId::Job(claim.id),
                receipt: None,
                command: Command::ContinueAfterRestart {
                    source: source.clone(),
                    content,
                },
            },
            context(claim.id.as_u128() + 100),
            None,
        )
        .unwrap();
    assert!(matches!(committed.step.outcome, Outcome::Rejected(_)));
}

#[test]
fn retry_backoff_blocks_only_its_lane_and_live_claims_are_never_claimed_twice() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    enqueue(&storage, 1, "a");
    enqueue(&storage, 2, "a");
    enqueue(&storage, 3, "b");
    let claims = storage.claim(0, 10).unwrap();
    assert_eq!(claims.len(), 2);
    let first = claims
        .iter()
        .find(|claim| claim.id == Uuid::from_u128(1))
        .unwrap();
    let other = claims
        .iter()
        .find(|claim| claim.id == Uuid::from_u128(3))
        .unwrap();
    assert!(storage.claim(0, 10).unwrap().is_empty());
    assert!(storage.delivered(first.id, first.attempt).is_err());
    assert!(
        storage
            .retry(first.id, first.attempt, 100, "transient")
            .unwrap()
    );
    receive(&storage, other);
    assert!(storage.delivered(other.id, other.attempt).unwrap());
    enqueue(&storage, 4, "b");
    let independent = storage.claim(99, 10).unwrap();
    assert_eq!(independent.len(), 1);
    assert_eq!(independent[0].id, Uuid::from_u128(4));
    let again = storage.claim(100, 10).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, first.id);
    assert_eq!(again[0].attempt, first.attempt + 1);
    assert!(
        !storage
            .retry(first.id, first.attempt, 200, "stale")
            .unwrap()
    );
    receive(&storage, &again[0]);
    assert!(!storage.delivered(first.id, first.attempt).unwrap());
    assert!(storage.delivered(again[0].id, again[0].attempt).unwrap());
    let next = storage.claim(100, 10).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].id, Uuid::from_u128(2));
}

#[test]
fn process_recovery_retries_a_claim_but_rejects_its_old_completion() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    enqueue(&storage, 1, "a");
    let old = storage.claim(0, 1).unwrap().remove(0);
    drop(storage);
    let reopened = Storage::open(&path);
    let next = reopened.claim(0, 1).unwrap().remove(0);
    assert_eq!(next.id, old.id);
    assert!(next.attempt > old.attempt);
    assert!(!reopened.delivered(old.id, old.attempt).unwrap());
    receive(&reopened, &next);
    assert!(reopened.delivered(next.id, next.attempt).unwrap());
    assert!(reopened.claim(0, 10).unwrap().is_empty());
}

#[test]
fn native_reimport_keeps_reused_turn_and_item_occurrences_after_reopen() {
    use agent_protocol::{
        items::Item as NativeItem,
        models::{Thread as NativeThread, ThreadResponse, Turn as NativeTurn, TurnStatus},
    };
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let id = address("external-history");
    let response = |texts: [&str; 2]| ThreadResponse {
        thread: NativeThread {
            id: Some(id.clone()),
            status: agent_protocol::models::SessionStatus::Idle,
            cwd: Some("/project".into()),
            turns: Some(
                texts
                    .into_iter()
                    .map(|text| {
                        Arc::new(NativeTurn {
                            id: "reused-turn".into(),
                            provider: Some(ProviderKind::Claude),
                            status: TurnStatus::Completed,
                            items: Some(vec![Arc::new(NativeItem::new(
                                "reused-item".into(),
                                ItemStatus::Completed,
                                ItemBody::AssistantText {
                                    text: text.into(),
                                    phase: Default::default(),
                                },
                            ))]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    let storage = Storage::open(&path);
    storage
        .import(&id, &response(["first", "second"]), Some(&id), 10)
        .unwrap();
    let before = storage.history(&id, 0).unwrap().thread.turns.unwrap();
    assert_ne!(before[0].id, before[1].id);
    assert_ne!(
        before[0].items.as_ref().unwrap()[0].id,
        before[1].items.as_ref().unwrap()[0].id
    );
    drop(storage);
    let storage = Storage::open(&path);
    for now in [20, 30] {
        storage.queue_import(&id, &id, now).unwrap();
        let claim = storage.claim(now, 1).unwrap().remove(0);
        storage
            .reimport(
                &id,
                &id,
                claim.id,
                claim.attempt,
                &response(["first updated", "second updated"]),
                now,
            )
            .unwrap();
        // A lost reply can deliver the same completed job again.
        storage
            .reimport(
                &id,
                &id,
                claim.id,
                claim.attempt,
                &response(["must not replace", "must not replace"]),
                now,
            )
            .unwrap();
        let after = storage.history(&id, 0).unwrap().thread.turns.unwrap();
        assert_eq!(after.len(), 2);
        for (index, expected) in ["first", "second"].into_iter().enumerate() {
            assert_eq!(after[index].id, before[index].id);
            let items = after[index].items.as_ref().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].id, before[index].items.as_ref().unwrap()[0].id);
            assert!(matches!(items[0].body(),ItemBody::AssistantText {text,..} if text==expected));
        }
    }
}

#[test]
fn catalog_failure_keeps_rows_and_only_the_current_claim_changes_their_confidence() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("cached-native");
    let mut native = header();
    native.details.activity.waiting = true;
    native.details.activity.approval = true;
    native.details.activity.status = agent_protocol::execution::SessionStatus::Running;
    storage.queue_catalog(ProviderKind::Claude, 0).unwrap();
    let first = storage.claim(0, 1).unwrap().remove(0);
    storage
        .catalog_page(&first, &[(id.clone(), native)], None, 0)
        .unwrap();
    let current = storage.claim(60_000, 1).unwrap().remove(0);
    let before = storage.header(&id).unwrap();
    storage
        .catalog_failed(&first, ProviderKind::Claude)
        .unwrap();
    assert_eq!(storage.header(&id).unwrap(), before);
    storage
        .catalog_failed(&current, ProviderKind::Claude)
        .unwrap();
    let stale = storage.header(&id).unwrap();
    assert!(stale.details.activity.stale);
    let activity = storage.activity().unwrap();
    assert_eq!(
        activity.statuses,
        [(id.clone(), agent_protocol::models::SessionStatus::Unknown)]
    );
    assert!(activity.waiting.is_empty());
    assert!(
        activity.display.ongoing,
        "unknown state cannot certify completion"
    );
    assert_eq!(stale.state.name, before.state.name);
    assert_eq!(stale.details.updated_at, before.details.updated_at);
    let cursor = storage.list(None).unwrap();
    storage
        .catalog_failed(&current, ProviderKind::Claude)
        .unwrap();
    assert_eq!(
        storage.list(None).unwrap(),
        cursor,
        "repeated errors do not publish another version"
    );
    storage
        .catalog_page(&current, &[(id.clone(), before.clone())], None, 60_000)
        .unwrap();
    assert_eq!(storage.header(&id).unwrap(), before);
    storage
        .catalog_failed(&current, ProviderKind::Claude)
        .unwrap();
    assert_eq!(
        storage.header(&id).unwrap(),
        before,
        "late failures cannot undo a completed scan"
    );
}

#[test]
fn unmaterialized_native_work_queues_input_until_the_same_source_becomes_idle() {
    use agent_protocol::models::{SessionStatus, Thread as NativeThread, ThreadResponse};
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("unmaterialized");
    storage
        .import(
            &id,
            &ThreadResponse {
                thread: NativeThread {
                    id: Some(id.clone()),
                    cwd: Some("/project".into()),
                    status: SessionStatus::Running,
                    turns: Some(Vec::new()),
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(&id),
            1,
        )
        .unwrap();
    assert!(
        storage
            .header(&id)
            .unwrap()
            .details
            .activity
            .unobserved_running()
    );
    let admitted = storage.decide(&id, submit(10), context(10), None).unwrap();
    assert_eq!(admitted.step.outcome, Outcome::Accepted);
    assert!(admitted.step.actions.is_empty());
    assert!(matches!(
        storage
            .state(&id)
            .unwrap()
            .inputs
            .values()
            .next()
            .unwrap()
            .state,
        InputState::Queued { .. }
    ));
    let old = storage
        .decide(
            &id,
            Event::NativeStatus {
                provider: ProviderKind::Claude,
                source: "another-source".into(),
                status: SessionStatus::Idle,
            },
            context(11),
            None,
        )
        .unwrap();
    assert!(old.step.actions.is_empty());
    assert_eq!(old.cursor, admitted.cursor);
    let idle = Event::NativeStatus {
        provider: ProviderKind::Claude,
        source: id.id.clone(),
        status: SessionStatus::Idle,
    };
    let delivered = storage
        .decide(&id, idle.clone(), context(12), None)
        .unwrap();
    assert!(
        matches!(&delivered.step.actions[..],[decision::Action::Deliver {attempt,..}] if attempt.source==id.id)
    );
    assert!(
        !storage
            .header(&id)
            .unwrap()
            .details
            .activity
            .unobserved_running()
    );
    let replay = storage.decide(&id, idle, context(13), None).unwrap();
    assert!(replay.step.actions.is_empty());
    assert_eq!(replay.cursor, delivered.cursor);
}

#[test]
fn external_turn_completion_refreshes_text_before_releasing_queued_input() {
    use agent_protocol::models::{
        SessionStatus, Thread as NativeThread, ThreadResponse, Turn as NativeTurn, TurnStatus,
    };
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("external-running");
    let response = |status, text: &str| ThreadResponse {
        thread: NativeThread {
            id: Some(id.clone()),
            cwd: Some("/project".into()),
            status: if status == TurnStatus::Running {
                SessionStatus::Running
            } else {
                SessionStatus::Idle
            },
            turns: Some(vec![Arc::new(NativeTurn {
                id: "native-turn".into(),
                status,
                items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                    "answer".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: text.into(),
                        phase: Default::default(),
                    },
                ))]),
                ..Default::default()
            })]),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&id, &response(TurnStatus::Running, "working"), Some(&id), 1)
        .unwrap();
    let admitted = storage.decide(&id, submit(20), context(20), None).unwrap();
    assert!(admitted.step.actions.is_empty());
    let idle = Event::NativeStatus {
        provider: ProviderKind::Claude,
        source: id.id.clone(),
        status: SessionStatus::Idle,
    };
    for _ in 0..2 {
        assert!(
            storage
                .decide(&id, idle.clone(), context(21), None)
                .unwrap()
                .step
                .actions
                .is_empty()
        );
    }
    let jobs = storage.claim(100, 10).unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "native status changes coalesce into one durable refresh"
    );
    let claim = &jobs[0];
    storage
        .reimport(
            &id,
            &id,
            claim.id,
            claim.attempt,
            &response(TurnStatus::Completed, "finished outside Bex"),
            100,
        )
        .unwrap();
    let history = storage.history(&id, 0).unwrap();
    let turns = history.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id.as_str(), "native-turn");
    assert!(
        matches!(turns[0].items.as_ref().unwrap()[0].body(),ItemBody::AssistantText {text,..} if text=="finished outside Bex")
    );
    let next = storage.decide(&id, Event::Wake, context(22), None).unwrap();
    assert!(
        matches!(&next.step.actions[..],[decision::Action::Deliver {attempt,..}] if attempt.source==id.id)
    );
    assert!(
        storage
            .decide(&id, Event::Wake, context(23), None)
            .unwrap()
            .step
            .actions
            .is_empty()
    );
}

#[test]
fn catalog_refresh_during_a_claim_is_retained_and_empty_failures_are_replicated() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let ready = |storage: &Storage| {
        let message = storage.list(None).unwrap();
        let replica = decision::records::Replica::new(agent_protocol::sync::Scope::List, 1)
            .apply(1, message)
            .unwrap();
        decision::records::catalog_ready(&replica)
    };
    storage.queue_catalog(ProviderKind::Claude, 100).unwrap();
    assert!(!ready(&storage));
    let claim = storage.claim(100, 1).unwrap().pop().unwrap();
    storage.queue_catalog(ProviderKind::Claude, 101).unwrap();
    storage.catalog_page(&claim, &[], None, 102).unwrap();
    assert!(!ready(&storage));
    let again = storage.claim(102, 1).unwrap().pop().unwrap();
    storage
        .retry(again.id, again.attempt, 1000, "native catalog unavailable")
        .unwrap();
    assert!(ready(&storage));
    let Message::Snapshot { rows, .. } = storage.list(None).unwrap() else {
        panic!("snapshot")
    };
    assert!(rows.iter().any(|row|matches!(&row.content,Content::Full(record) if matches!(record.as_ref(),Record::Catalog(decision::records::Catalog::Failed(error)) if error=="native catalog unavailable"))));
    let retry = storage.claim(1000, 1).unwrap().pop().unwrap();
    let before = storage.list(None).unwrap();
    storage
        .retry(retry.id, retry.attempt, 2000, "native catalog unavailable")
        .unwrap();
    assert_eq!(storage.list(None).unwrap(), before);
    let retry = storage.claim(2000, 1).unwrap().pop().unwrap();
    storage.catalog_page(&retry, &[], None, 2001).unwrap();
    assert!(ready(&storage));
}

#[test]
fn initial_import_commits_history_with_its_job_and_recovers_after_a_failed_commit() {
    use agent_protocol::models::{Thread as NativeThread, ThreadResponse, Turn as NativeTurn};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let id = address("initial-import");
    let response = ThreadResponse {
        thread: NativeThread {
            id: Some(id.clone()),
            history_read_state: Some(agent_protocol::session::HistoryReadState::new(
                agent_protocol::session::HistoryReadKind::Complete,
                vec![],
            )),
            turns: Some(vec![Arc::new(NativeTurn {
                id: "saved-turn".into(),
                status: agent_protocol::execution::TurnStatus::Completed,
                items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                    "saved-answer".into(),
                    ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: "Saved answer".into(),
                        phase: Default::default(),
                    },
                ))]),
                ..Default::default()
            })]),
            ..Default::default()
        },
        ..Default::default()
    };
    let storage = Storage::open(&path);
    storage.import_status(&id, 100).unwrap();
    let first = storage.claim(100, 1).unwrap().pop().unwrap();
    let before = storage.history(&id, 0).unwrap();
    storage.with(|connection| connection.execute_batch("CREATE TRIGGER fail_import BEFORE DELETE ON jobs BEGIN SELECT RAISE(ABORT,'import commit failed'); END;").map_err(failure)).unwrap();
    assert!(
        storage
            .reimport(&id, &id, first.id, first.attempt, &response, 101)
            .unwrap_err()
            .contains("import commit failed")
    );
    assert_eq!(storage.history(&id, 0).unwrap(), before);
    assert!(!storage.loaded(&id).unwrap());
    drop(storage);
    let storage = Storage::open(&path);
    storage
        .with(|connection| {
            connection
                .execute_batch("DROP TRIGGER fail_import;")
                .map_err(failure)
        })
        .unwrap();
    let current = storage.claim(102, 1).unwrap().pop().unwrap();
    assert_eq!(current.id, first.id);
    assert!(
        storage
            .reimport(&id, &id, first.id, first.attempt, &response, 102)
            .unwrap_err()
            .contains("claim is obsolete")
    );
    storage
        .reimport(&id, &id, current.id, current.attempt, &response, 103)
        .unwrap();
    storage
        .reimport(&id, &id, current.id, current.attempt, &before, 104)
        .unwrap();
    assert!(storage.loaded(&id).unwrap());
    assert!(storage.claim(104, 1).unwrap().is_empty());
    let cursor = storage.with(|connection| cursor(connection)).unwrap();
    let repeated = storage.import_status(&id, 105).unwrap();
    assert_eq!(repeated.cursor, cursor);
    assert!(repeated.rows.is_empty());
    assert!(storage.claim(105, 1).unwrap().is_empty());
    let loaded = storage.history(&id, 0).unwrap();
    assert_eq!(
        loaded.thread.turns.unwrap()[0].items.as_ref().unwrap()[0].body(),
        response.thread.turns.unwrap()[0].items.as_ref().unwrap()[0].body()
    );
}

#[test]
fn restoring_a_missing_outbox_input_fences_a_delayed_original_command() {
    for exists in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        if exists {
            storage.create(&id, &header()).unwrap();
        }
        let input = Uuid::new_v4();
        let event = |command| Event::Command {
            id: OperationId::Client(input),
            receipt: None,
            command,
        };
        let fenced = storage
            .decide(
                &id,
                event(Command::ResolveUnknown {
                    id: input.to_string().into(),
                    retry: false,
                }),
                context(1),
                None,
            )
            .unwrap();
        assert!(matches!(fenced.step.outcome, Outcome::Rejected(_)));
        let delayed = storage
            .decide(
                &id,
                event(Command::Submit {
                    id: input.to_string().into(),
                    content: InputContent {
                        parts: vec![Input::Text {
                            text: "delayed original".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: FollowUp::Queue,
                    },
                }),
                context(2),
                Some(&header()),
            )
            .unwrap();
        assert_eq!(delayed.step.outcome, fenced.step.outcome);
        assert!(delayed.step.actions.is_empty());
        assert_eq!(storage.lookup(&id).unwrap().is_some(), exists);
        if exists {
            assert!(storage.state(&id).unwrap().inputs.is_empty());
        }
    }
}

#[test]
fn delivery_proof_and_fencing_settle_in_transaction_order() {
    for proof_first in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        storage.create(&target, &header()).unwrap();
        ready(&storage, &target);
        let delivered = storage
            .decide(&target, submit(101), context(101), None)
            .unwrap();
        let decision::Action::Deliver {
            id,
            action,
            attempt,
            ..
        } = delivered.step.actions[0].clone()
        else {
            panic!("delivery")
        };
        storage
            .decide(
                &target,
                Event::DeliveryFailed {
                    id: id.clone(),
                    action,
                    unknown: true,
                    error: Default::default(),
                },
                context(102),
                None,
            )
            .unwrap();
        let resolution = storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(Uuid::new_v4()),
                    receipt: None,
                    command: Command::ResolveUnknown {
                        id: id.clone(),
                        retry: false,
                    },
                },
                context(103),
                None,
            )
            .unwrap();
        let decision::Action::FenceDelivery { action: fence, .. } = resolution.step.actions[0]
        else {
            panic!("fence")
        };
        let proof = Event::Provider {
            attempt: attempt.id,
            native_turn: Some("native-turn".into()),
            native_items: vec![],
            observation: Observation::Timing {
                started_at: Some(100),
                completed_at: None,
                duration_ms: None,
            },
        };
        if proof_first {
            storage
                .decide(&target, proof.clone(), context(104), None)
                .unwrap();
        }
        assert_eq!(
            storage
                .fence_delivery(&target, &id, fence, &attempt)
                .unwrap(),
            (!proof_first).then_some(true)
        );
        if !proof_first {
            assert_eq!(
                storage
                    .decide(&target, proof, context(105), None)
                    .unwrap()
                    .step
                    .outcome,
                Outcome::Ignored
            );
            storage
                .decide(
                    &target,
                    Event::DeliveryFenced {
                        id: id.clone(),
                        action: fence,
                    },
                    context(106),
                    None,
                )
                .unwrap();
        }
        let execution = storage.execution(&target, attempt.id).unwrap().unwrap();
        assert_eq!(execution.accepting, proof_first);
        assert_eq!(
            storage
                .state(&target)
                .unwrap()
                .turns
                .contains_key(&attempt.turn),
            proof_first
        );
        assert!(!storage.state(&target).unwrap().inputs.contains_key(&id));
    }
}

#[test]
fn the_first_native_turn_executes_a_pending_stop_only_once() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&target, &header()).unwrap();
    ready(&storage, &target);
    let admitted = storage
        .decide(&target, submit(100), context(100), None)
        .unwrap();
    let decision::Action::Deliver {
        id,
        action,
        attempt,
        ..
    } = admitted.step.actions[0].clone()
    else {
        panic!("delivery")
    };
    storage
        .decide(
            &target,
            Event::Delivered {
                id,
                action,
                native_turn: None,
            },
            context(101),
            None,
        )
        .unwrap();
    storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(Uuid::new_v4()),
                receipt: None,
                command: Command::Interrupt {
                    turn: Some(attempt.turn.clone()),
                },
            },
            context(102),
            None,
        )
        .unwrap();
    let event = Event::Provider {
        attempt: attempt.id,
        native_turn: Some("native-turn".into()),
        native_items: vec![],
        observation: Observation::Timing {
            started_at: Some(100),
            completed_at: None,
            duration_ms: None,
        },
    };
    let first = storage
        .decide(&target, event.clone(), context(103), None)
        .unwrap();
    assert!(
        matches!(first.step.actions.as_slice(), [decision::Action::Interrupt { attempt: current, .. }] if current == &attempt)
    );
    assert!(
        storage
            .decide(&target, event, context(104), None)
            .unwrap()
            .step
            .actions
            .is_empty()
    );
}

#[test]
fn repeated_catalog_native_parent_does_not_version_an_unchanged_public_child() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let parent = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&parent, &header()).unwrap();
    storage
        .save_source(
            &parent,
            ProviderKind::Claude,
            "native-parent",
            None,
            &Default::default(),
        )
        .unwrap();
    let child = address("native-child");
    let mut child_header = header();
    child_header.details.parent = Some(address("native-parent"));
    storage.queue_catalog(ProviderKind::Claude, 100).unwrap();
    let first = storage.claim(100, 1).unwrap().pop().unwrap();
    storage
        .catalog_page(&first, &[(child.clone(), child_header.clone())], None, 100)
        .unwrap();
    assert_eq!(storage.header(&child).unwrap().details.parent, Some(parent));
    let replica = decision::records::Replica::new(agent_protocol::sync::Scope::List, 1)
        .apply(1, storage.list(None).unwrap())
        .unwrap();
    let key = Key::Thread(child.clone());
    let before = replica.rows()[&key].clone();
    let again = storage.claim(60_100, 1).unwrap().pop().unwrap();
    assert!(
        matches!(storage.list(replica.cursor()).unwrap(), Message::Changes { from, through, ref rows, ref removed } if from == through && rows.is_empty() && removed.is_empty())
    );
    storage
        .catalog_page(&again, &[(child, child_header)], None, 60_100)
        .unwrap();
    let message = storage.list(replica.cursor()).unwrap();
    assert!(
        matches!(&message, Message::Changes { from, through, rows, removed } if from == through && rows.is_empty() && removed.is_empty())
    );
    let replica = replica.apply(1, message).unwrap();
    assert_eq!(replica.rows()[&key], before);
}

#[test]
fn completed_catalog_scans_remove_missing_rows_once_and_preserve_pages_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let kept = address("kept");
    let vanished = address("vanished");
    let parent = address("parent");
    let child = address("child");
    let owned = address("owned");
    storage.create(&owned, &header()).unwrap();
    let mut child_header = header();
    child_header.details.parent = Some(parent.clone());
    storage.queue_catalog(ProviderKind::Claude, 0).unwrap();
    let claim = storage.claim(0, 1).unwrap().remove(0);
    storage
        .catalog_page(
            &claim,
            &[
                (kept.clone(), header()),
                (vanished.clone(), header()),
                (parent.clone(), header()),
                (child.clone(), child_header.clone()),
            ],
            None,
            0,
        )
        .unwrap();
    let mut replica = decision::records::Replica::new(agent_protocol::sync::Scope::List, 1)
        .apply(1, storage.list(None).unwrap())
        .unwrap();
    let before = replica.cursor();
    let before_remove = replica.clone();
    let claim = storage.claim(60_000, 1).unwrap().remove(0);
    storage
        .catalog_page(
            &claim,
            &[(kept.clone(), header())],
            Some("page-2".into()),
            60_000,
        )
        .unwrap();
    assert!(
        storage.lookup(&vanished).unwrap().is_some(),
        "a partial scan cannot establish deletion"
    );
    drop(storage);
    let storage = Storage::open(&path);
    let claim = storage.claim(60_000, 1).unwrap().remove(0);
    storage
        .catalog_failed(&claim, ProviderKind::Claude)
        .unwrap();
    assert!(storage.lookup(&vanished).unwrap().is_some());
    storage
        .catalog_page(
            &claim,
            &[(child.clone(), child_header.clone())],
            None,
            60_000,
        )
        .unwrap();
    assert!(
        storage.lookup(&vanished).unwrap().is_some(),
        "one completed scan can miss a row moved between pages"
    );
    drop(storage);
    let storage = Storage::open(&path);
    let claim = storage.claim(120_000, 1).unwrap().remove(0);
    storage
        .catalog_page(
            &claim,
            &[(kept.clone(), header()), (child.clone(), child_header)],
            None,
            120_000,
        )
        .unwrap();
    replica = replica.apply(1, storage.list(before).unwrap()).unwrap();
    for id in [&kept, &child, &owned] {
        assert!(
            replica.rows().contains_key(&Key::Thread(id.clone())),
            "{id}"
        );
    }
    for id in [&vanished, &parent] {
        assert!(
            !replica.rows().contains_key(&Key::Thread(id.clone())),
            "{id}"
        );
    }
    assert!(storage.lookup(&vanished).unwrap().is_none());
    assert_eq!(
        storage.header(&child).unwrap().details.parent,
        Some(parent.clone())
    );
    // Reappearing identical metadata becomes visible again, even when the
    // parent identity was retained only for a child relationship.
    let claim = storage.claim(180_000, 1).unwrap().remove(0);
    storage
        .catalog_page(
            &claim,
            &[
                (kept.clone(), header()),
                (parent.clone(), header()),
                (vanished.clone(), header()),
            ],
            None,
            180_000,
        )
        .unwrap();
    let next = before_remove
        .apply(1, storage.list(before).unwrap())
        .unwrap();
    assert!(next.rows().contains_key(&Key::Thread(parent)));
    assert!(next.rows().contains_key(&Key::Thread(vanished)));
}

#[test]
fn steering_waits_for_the_first_native_turn_and_dispatches_once_after_it_is_bound() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&target, &header()).unwrap();
    ready(&storage, &target);
    let admitted = storage
        .decide(&target, submit(100), context(100), None)
        .unwrap();
    let decision::Action::Deliver {
        id,
        action,
        attempt,
        ..
    } = admitted.step.actions[0].clone()
    else {
        panic!("delivery")
    };
    storage
        .decide(
            &target,
            Event::Delivered {
                id,
                action,
                native_turn: None,
            },
            context(101),
            None,
        )
        .unwrap();
    let mut steer = submit(102);
    if let Event::Command {
        command: Command::Submit { content, .. },
        ..
    } = &mut steer
    {
        content.follow_up = FollowUp::Steer;
    }
    let waiting = storage.decide(&target, steer, context(102), None).unwrap();
    assert_eq!(waiting.step.outcome, Outcome::Accepted);
    assert!(waiting.step.actions.is_empty());
    assert!(
        matches!(storage.state(&target).unwrap().inputs[Uuid::from_u128(102).to_string().as_str()].state, InputState::Queued { steer: Some(current) } if current == attempt.id)
    );
    assert!(
        storage
            .decide(&target, Event::Wake, context(103), None)
            .unwrap()
            .step
            .actions
            .is_empty()
    );
    storage
        .decide(
            &target,
            Event::Provider {
                attempt: attempt.id,
                native_turn: Some("native-turn".into()),
                native_items: vec![],
                observation: Observation::Timing {
                    started_at: Some(100),
                    completed_at: None,
                    duration_ms: None,
                },
            },
            context(104),
            None,
        )
        .unwrap();
    let ready = storage
        .decide(&target, Event::Wake, context(105), None)
        .unwrap();
    assert!(
        matches!(ready.step.actions.as_slice(), [decision::Action::Deliver { steer: true, attempt: current, .. }] if current == &attempt)
    );
    assert!(
        storage
            .decide(&target, Event::Wake, context(106), None)
            .unwrap()
            .step
            .actions
            .is_empty()
    );
}

#[test]
fn rewind_removes_the_suffix_without_resetting_loaded_pages_and_reimport_cannot_resurrect_it() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("rewind");
    let response = agent_protocol::models::ThreadResponse {
        thread: agent_protocol::models::Thread {
            id: Some(target.clone()),
            status: agent_protocol::models::SessionStatus::Idle,
            turns: Some(
                (1..=2)
                    .map(|index| {
                        Arc::new(agent_protocol::models::Turn {
                            id: format!("native-{index}").into(),
                            status: agent_protocol::models::TurnStatus::Completed,
                            items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                                format!("answer-{index}").into(),
                                ItemStatus::Completed,
                                ItemBody::AssistantText {
                                    text: format!("answer {index}"),
                                    phase: Default::default(),
                                },
                            ))]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&target, &response, Some(&target), 100)
        .unwrap();
    let before = storage.snapshot(&target, None, None).unwrap();
    let replica = decision::records::Replica::new(
        agent_protocol::sync::Scope::Conversation(target.clone()),
        1,
    )
    .apply(1, before)
    .unwrap();
    let window = replica.window();
    assert!(window.is_some());
    assert!(matches!(
        storage.snapshot(&target, replica.cursor(), window).unwrap(),
        Message::Changes { .. }
    ));
    let turns = storage.history(&target, 0).unwrap().thread.turns.unwrap();
    let rewind = storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(Uuid::new_v4()),
                receipt: None,
                command: Command::Rewind(turns[1].id.clone()),
            },
            context(200),
            None,
        )
        .unwrap();
    assert_eq!(rewind.step.outcome, Outcome::Accepted);
    let snapshot = storage.snapshot(&target, replica.cursor(), window).unwrap();
    assert!(matches!(&snapshot, Message::Changes { removed, .. } if !removed.is_empty()));
    let after = replica.apply(1, snapshot).unwrap();
    assert_eq!(
        after.window(),
        window,
        "rewind preserves already loaded page boundaries"
    );
    assert_eq!(
        decision::records::project(&after)
            .unwrap()
            .thread
            .turns
            .unwrap()
            .len(),
        1
    );
    let state = storage.state(&target).unwrap();
    assert!(
        matches!(&state.bindings[&ProviderKind::Claude], Binding::Fork { source, through } if source == &target.id && through.as_str() == "native-1")
    );
    // Even a subsequent explicit import from this native source must respect
    // the saved rollback tombstone, rather than recreating the removed suffix.
    storage
        .save_source(
            &target,
            ProviderKind::Claude,
            &target.id,
            None,
            &Default::default(),
        )
        .unwrap();
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, &target)?;
            let binding = Change::Binding {
                provider: ProviderKind::Claude,
                value: Some(Binding::Ready {
                    source: target.id.clone(),
                    covered: state.history_end(),
                }),
            };
            let state = state.apply(std::slice::from_ref(&binding));
            rows::write(
                &transaction,
                &rows::WriteScope {
                    key,
                    id: &target,
                    sequence: 0,
                    at: 300,
                    process: storage.process,
                },
                &binding,
                &state.turns,
                &mut Vec::new(),
                &mut Vec::new(),
            )?;
            transaction.commit().map_err(failure)
        })
        .unwrap();
    storage.queue_import(&target, &target, 300).unwrap();
    let claim = storage.claim(300, 1).unwrap().remove(0);
    let imported = storage
        .reimport(&target, &target, claim.id, claim.attempt, &response, 300)
        .unwrap();
    assert_eq!(imported.step.outcome, Outcome::Accepted);
    let retained = storage.history(&target, 0).unwrap().thread.turns.unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].id, turns[0].id);
}

#[test]
fn setup_stop_is_rejected_by_the_storage_transaction_after_agent_creation_started() {
    use super::workspace::SetupChange;
    use agent_protocol::conversation::records::WorkspacePreparation;
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("setup-stop");
    storage.create(&target, &header()).unwrap();
    let input: ClientInputId = Uuid::from_u128(1).to_string().into();
    storage
        .decide(&target, submit(1), context(1), None)
        .unwrap();
    let state = storage.state(&target).unwrap();
    let InputState::Preparing { action, .. } = state.inputs[&input].state else {
        panic!("not preparing")
    };
    storage
        .setup(
            &target,
            action,
            SetupChange::Begin {
                input,
                workspace: Box::new(WorkspacePreparation {
                    project_cwd: "/project".into(),
                    strategy: None,
                    generation: action,
                    created_worktree: false,
                    setup: Some(agent_protocol::workspace::WorkspaceSetup::new(None, 100)),
                    visible: true,
                }),
            },
            100,
        )
        .unwrap();
    assert!(
        storage
            .header(&target)
            .unwrap()
            .details
            .workspace
            .unwrap()
            .setup
            .unwrap()
            .can_cancel
    );
    storage
        .setup(&target, action, SetupChange::AgentStarted, 101)
        .unwrap();
    let rejected = storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(Uuid::new_v4()),
                receipt: None,
                command: Command::Interrupt { turn: None },
            },
            context(2),
            None,
        )
        .unwrap();
    assert!(matches!(rejected.step.outcome, Outcome::Rejected(_)));
    assert!(matches!(
        storage
            .state(&target)
            .unwrap()
            .inputs
            .values()
            .next()
            .unwrap()
            .state,
        InputState::Preparing { .. }
    ));
}

#[test]
fn a_catalog_row_seen_again_resets_missing_scan_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("moved-between-pages");
    storage.queue_catalog(ProviderKind::Claude, 0).unwrap();
    for (index, present) in [true, false, true, false, false].into_iter().enumerate() {
        let now = index as i64 * 60_000;
        let claim = storage.claim(now, 1).unwrap().remove(0);
        let headers = if present {
            vec![(target.clone(), header())]
        } else {
            Vec::new()
        };
        storage.catalog_page(&claim, &headers, None, now).unwrap();
        assert_eq!(storage.lookup(&target).unwrap().is_some(), index != 4);
    }
}

#[test]
fn a_cached_loading_window_resets_to_the_latest_ten_turns_after_import() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("loading");
    storage.queue_catalog(ProviderKind::Claude, 0).unwrap();
    let claim = storage.claim(0, 1).unwrap().remove(0);
    storage
        .catalog_page(&claim, &[(target.clone(), header())], None, 0)
        .unwrap();
    let replica = decision::records::Replica::new(
        agent_protocol::sync::Scope::Conversation(target.clone()),
        1,
    )
    .apply(1, storage.snapshot(&target, None, None).unwrap())
    .unwrap();
    assert_eq!(replica.window().unwrap().turn, 0);
    let response = agent_protocol::models::ThreadResponse {
        thread: agent_protocol::models::Thread {
            id: Some(target.clone()),
            turns: Some(
                (1..=12)
                    .map(|index| {
                        Arc::new(agent_protocol::models::Turn {
                            id: format!("native-{index}").into(),
                            status: agent_protocol::models::TurnStatus::Completed,
                            items: Some(vec![
                                Arc::new(agent_protocol::items::Item::new(
                                    format!("user-{index}").into(),
                                    ItemStatus::Completed,
                                    ItemBody::UserMessage {
                                        text: Some(format!("question {index}")),
                                        content: vec![],
                                    },
                                )),
                                Arc::new(agent_protocol::items::Item::new(
                                    format!("answer-{index}").into(),
                                    ItemStatus::Completed,
                                    ItemBody::AssistantText {
                                        text: format!("answer {index}"),
                                        phase: Default::default(),
                                    },
                                )),
                            ]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&target, &response, Some(&target), 100)
        .unwrap();
    let message = storage
        .snapshot(&target, replica.cursor(), replica.window())
        .unwrap();
    assert!(matches!(message, Message::Snapshot { .. }));
    let next = replica.apply(1, message).unwrap();
    let thread = decision::records::project(&next).unwrap().thread;
    assert_eq!(thread.turns.as_ref().unwrap().len(), 10);
    assert_eq!(thread.history_has_more, Some(true));
}

#[test]
fn compact_capacity_failure_is_not_resumed_after_loading_the_durable_decision_state() {
    for lost in [false, true] {
        for (text, compact) in [
            ("/compact", true),
            ("  /CoMpAcT\n", true),
            ("/compact please", false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let storage = Storage::open(&directory.path().join("state.sqlite"));
            let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
            let mut header = header();
            header.state.provider = ProviderKind::Codex;
            storage.create(&target, &header).unwrap();
            ready(&storage, &target);
            let mut event = submit(1);
            let Event::Command {
                command: Command::Submit { content, .. },
                ..
            } = &mut event
            else {
                unreachable!()
            };
            content.parts = vec![Input::Text { text: text.into() }];
            let admitted = storage.decide(&target, event, context(1), None).unwrap();
            let [
                decision::Action::Deliver {
                    id,
                    action,
                    attempt,
                    ..
                },
            ] = &admitted.step.actions[..]
            else {
                panic!("delivery")
            };
            storage
                .decide(
                    &target,
                    Event::Delivered {
                        id: id.clone(),
                        action: *action,
                        native_turn: Some("native-turn".into()),
                    },
                    context(2),
                    None,
                )
                .unwrap();
            let error = agent_protocol::execution::ExecutionError {
                message: "capacity".into(),
                provider_code: Some("serverOverloaded".into()),
                ..Default::default()
            };
            let event = if lost {
                Event::ExecutionLost {
                    attempt: attempt.id,
                    error,
                }
            } else {
                Event::Provider {
                    native_items: Vec::new(),
                    native_turn: None,
                    attempt: attempt.id,
                    observation: Observation::Failed(Some(error)),
                }
            };
            let failed = storage.decide(&target, event, context(3), None).unwrap();
            assert_eq!(
                failed.step.actions.is_empty(),
                compact,
                "lost={lost}, text={text:?}"
            );
            if compact {
                assert!(
                    matches!(&storage.state(&target).unwrap().turns[&attempt.turn].state, TurnState::Failed(error) if error.message == "capacity")
                );
            } else {
                assert_eq!(failed.step.actions, [decision::Action::WaitUntil(2100)]);
            }
        }
    }
}

#[test]
fn a_claimed_legacy_import_failure_is_unavailable_and_can_be_retried() {
    use agent_protocol::session::{HistoryReadKind, HistoryReadState};
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("managed-history-failure");
    let mut header = header();
    header.details.history_read_state =
        Some(HistoryReadState::new(HistoryReadKind::Loading, vec![]));
    storage.create(&target, &header).unwrap();
    assert!(storage.loaded(&target).unwrap());
    ready(&storage, &target);
    let source = storage.active_source(&target).unwrap().unwrap();
    storage.queue_import(&target, &source, 100).unwrap();
    let claim = storage
        .claim(100, 16)
        .unwrap()
        .into_iter()
        .find(|claim| matches!(&claim.work, jobs::Work::Import { target: id, .. } if id == &target))
        .unwrap();
    storage
        .import_failed(
            &target,
            claim.id,
            claim.attempt,
            "native unavailable".into(),
        )
        .unwrap();
    let failed = storage
        .header(&target)
        .unwrap()
        .details
        .history_read_state
        .unwrap();
    assert_eq!(failed.kind, HistoryReadKind::Unavailable);
    let before = storage.snapshot(&target, None, None).unwrap();
    let repeated = storage
        .import_failed(
            &target,
            claim.id,
            claim.attempt,
            "native unavailable".into(),
        )
        .unwrap();
    assert!(repeated.rows.is_empty());
    assert_eq!(storage.snapshot(&target, None, None).unwrap(), before);
    storage.queue_import(&target, &source, 101).unwrap();
    let claim = storage
        .claim(101, 16)
        .unwrap()
        .into_iter()
        .find(|claim| matches!(&claim.work, jobs::Work::Import { target: id, .. } if id == &target))
        .unwrap();
    let retried = storage
        .reimport(
            &target,
            &source,
            claim.id,
            claim.attempt,
            &agent_protocol::models::ThreadResponse {
                thread: agent_protocol::models::Thread {
                    id: Some(source.clone()),
                    turns: Some(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
            102,
        )
        .unwrap();
    assert_eq!(retried.step.outcome, Outcome::Accepted);
    assert!(
        storage
            .header(&target)
            .unwrap()
            .details
            .history_read_state
            .is_none_or(|read| read.kind == HistoryReadKind::Complete)
    );
}

#[test]
fn a_private_native_source_cannot_create_a_second_conversation_row() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let public = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage.create(&public, &header()).unwrap();
    ready(&storage, &public);
    let native = storage.active_source(&public).unwrap().unwrap();
    let before = storage.list(None).unwrap();
    assert!(storage.import_status(&native, 100).is_err());
    assert_eq!(storage.list(None).unwrap(), before);
    assert!(storage.lookup(&native).unwrap().is_none());
}

#[test]
fn item_only_header_updates_follow_t3_shells_five_second_resend_window() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let id = address("quiet-header");
    storage.create(&id, &header()).unwrap();
    ready(&storage, &id);
    let sent = storage
        .decide(&id, submit(1100), context(1100), None)
        .unwrap();
    let decision::Action::Deliver {
        id: input,
        action,
        attempt,
        ..
    } = &sent.step.actions[0]
    else {
        panic!("delivery")
    };
    storage
        .decide(
            &id,
            Event::Delivered {
                id: input.clone(),
                action: *action,
                native_turn: Some("native-turn".into()),
            },
            context(1101),
            None,
        )
        .unwrap();
    let original = storage.header(&id).unwrap();
    let event = |observation| Event::Provider {
        attempt: attempt.id,
        native_turn: None,
        native_items: vec![],
        observation,
    };
    for index in 0..10 {
        let committed = storage
            .decide(
                &id,
                event(Observation::Item {
                    id: format!("paragraph-{index}").into(),
                    deferred: false,
                    status: ItemStatus::Running,
                    body: ItemBody::AssistantText {
                        text: format!("paragraph {index}\n\n"),
                        phase: AssistantPhase::default(),
                    },
                }),
                Context {
                    now: 1000 + index * 400,
                    ..context(1200 + index as u128)
                },
                None,
            )
            .unwrap();
        assert!(
            !committed
                .rows
                .iter()
                .any(|row| row.key == Key::Thread(id.clone()))
        );
        assert_eq!(storage.header(&id).unwrap(), original);
    }
    for (at, updated) in [(5099, false), (5100, true)] {
        let before = storage.with(|connection| cursor(connection)).unwrap();
        let committed = storage
            .decide(
                &id,
                event(Observation::Timing {
                    started_at: Some(42),
                    completed_at: None,
                    duration_ms: Some(at as u64),
                }),
                Context {
                    now: at,
                    ..context(at as u128)
                },
                None,
            )
            .unwrap();
        assert_eq!(
            committed
                .rows
                .iter()
                .any(|row| row.key == Key::Thread(id.clone())),
            updated,
            "at={at}"
        );
        let Message::Changes { rows, .. } = storage.list(Some(before)).unwrap() else {
            panic!("list catchup")
        };
        assert_eq!(
            rows.iter().any(|row| row.key == Key::Thread(id.clone())),
            updated
        );
    }
    let ended = storage
        .decide(
            &id,
            event(Observation::Completed),
            Context {
                now: 5101,
                ..context(1300)
            },
            None,
        )
        .unwrap();
    assert!(
        ended
            .rows
            .iter()
            .any(|row| row.key == Key::Thread(id.clone())),
        "status changes publish immediately"
    );
    assert_eq!(storage.header(&id).unwrap().details.updated_at, 5101);
}

#[test]
fn opening_keeps_message_text_inline_and_defers_work_details_newest_first() {
    use agent_protocol::models::{
        Item as NativeItem, Thread as NativeThread, ThreadResponse, Turn as NativeTurn, TurnStatus,
    };
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("messages-inline");
    let mut deferred = NativeItem::new(
        "command-0".into(),
        ItemStatus::Completed,
        ItemBody::CommandExecution {
            command: "old".into(),
            cwd: None,
            output: String::new(),
            output_omitted: false,
            exit_code: Some(0),
        },
    );
    deferred.defer();
    let turns = (0..10)
        .map(|n| {
            Arc::new(NativeTurn {
                id: format!("turn-{n}").into(),
                status: TurnStatus::Completed,
                items: Some(vec![
                    Arc::new(NativeItem::new(
                        format!("user-{n}").into(),
                        ItemStatus::Completed,
                        ItemBody::UserMessage {
                            text: Some("question".into()),
                            content: vec![],
                        },
                    )),
                    Arc::new(NativeItem::new(
                        format!("text-{n}").into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: "message".repeat(3000),
                            phase: Default::default(),
                        },
                    )),
                    Arc::new(if n == 0 {
                        deferred.clone()
                    } else {
                        NativeItem::new(
                            format!("command-{n}").into(),
                            ItemStatus::Completed,
                            ItemBody::CommandExecution {
                                command: "work".into(),
                                cwd: None,
                                output: "x".repeat(64 * 1024),
                                output_omitted: false,
                                exit_code: Some(0),
                            },
                        )
                    }),
                ]),
                ..Default::default()
            })
        })
        .collect();
    storage
        .import(
            &target,
            &ThreadResponse {
                thread: NativeThread {
                    id: Some(target.clone()),
                    turns: Some(turns),
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(&target),
            100,
        )
        .unwrap();
    let Message::Snapshot {
        cursor,
        window,
        rows,
    } = storage.snapshot(&target, None, None).unwrap()
    else {
        panic!("snapshot");
    };
    let mut message_count = 0;
    for row in &rows {
        let (Content::Full(value) | Content::Summary(value)) = &row.content;
        if let Record::Item(item) = value.as_ref()
            && matches!(
                item.content.body(),
                ItemBody::UserMessage { .. } | ItemBody::AssistantText { .. }
            )
        {
            message_count += 1;
            assert!(matches!(row.content, Content::Full(_)));
        }
    }
    assert_eq!(message_count, 20);
    let item = |id: &str| {
        rows.iter()
            .find(|row| {
                row.key
                    == Key::Item {
                        thread: target.clone(),
                        id: id.into(),
                    }
            })
            .unwrap()
    };
    assert!(matches!(item("command-1").content, Content::Summary(_)));
    assert!(matches!(item("command-9").content, Content::Full(_)));
    storage
        .hydrate_body(
            &target,
            &"command-0".into(),
            item("command-0").version,
            ItemBody::CommandExecution {
                command: "old".into(),
                cwd: None,
                output: "recovered details".into(),
                output_omitted: false,
                exit_code: Some(0),
            },
        )
        .unwrap();
    let Message::Changes { rows, .. } = storage.snapshot(&target, Some(cursor), window).unwrap()
    else {
        panic!("catch-up");
    };
    assert_eq!(rows.len(), 1);
    assert!(
        matches!(rows[0].content, Content::Full(_)),
        "unrelated work must not consume a catch-up row's budget"
    );
}

#[test]
fn importing_a_prefix_keeps_each_providers_covered_position_on_the_same_turn() {
    use agent_protocol::models::{
        Thread as NativeThread, ThreadResponse, Turn as NativeTurn, TurnStatus,
    };
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let native = address("remap-native");
    let turn = |id: &str| {
        Arc::new(NativeTurn {
            id: id.into(),
            status: TurnStatus::Completed,
            items: Some(vec![]),
            ..Default::default()
        })
    };
    let response = ThreadResponse {
        thread: NativeThread {
            id: Some(target.clone()),
            active_provider: Some(ProviderKind::Claude),
            turns: Some(vec![turn("first"), turn("anchor")]),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&target, &response, Some(&native), 100)
        .unwrap();
    storage
        .with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let key = thread_key(&transaction, &target)?;
            let state = rows::load(&transaction, key, storage.process)?;
            rows::write(
                &transaction,
                &rows::WriteScope {
                    key,
                    id: &target,
                    sequence: cursor(&transaction)?.sequence,
                    at: 100,
                    process: storage.process,
                },
                &Change::Binding {
                    provider: ProviderKind::Codex,
                    value: Some(Binding::Ready {
                        source: "inactive".into(),
                        covered: 2,
                    }),
                },
                &state.turns,
                &mut vec![],
                &mut vec![],
            )?;
            transaction.commit().map_err(failure)
        })
        .unwrap();
    storage.queue_import(&target, &native, 101).unwrap();
    let claim = storage.claim(101, 16).unwrap().into_iter().find(|claim| matches!(&claim.work, jobs::Work::Import { target: saved, .. } if saved == &target)).unwrap();
    let imported = ThreadResponse {
        thread: NativeThread {
            id: Some(native.clone()),
            turns: Some(vec![turn("prefix"), turn("anchor")]),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .reimport(&target, &native, claim.id, claim.attempt, &imported, 102)
        .unwrap();
    let state = storage.state(&target).unwrap();
    assert_eq!(state.turns[&TurnId::from("anchor")].ordinal, 3);
    assert!(matches!(
        state.bindings.get(&ProviderKind::Codex),
        Some(Binding::Ready { covered: 3, .. })
    ));
    assert!(matches!(
        state.bindings.get(&ProviderKind::Claude),
        Some(Binding::Ready { covered: 3, .. })
    ));
}

#[test]
fn stopped_checkout_cleanup_survives_a_restart_before_the_resource_effect() {
    use agent_protocol::workspace::{SetupPhase, WorkspaceSetup, WorkspaceStrategy};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let target = address("stopped-checkout");
    let generation = Uuid::new_v4();
    let mut setup = WorkspaceSetup::new(Some("main".into()), 0);
    setup.worktree_path = Some("/owned-checkout".into());
    setup.branch = Some("owned-branch".into());
    let mut header = header();
    header.state.workspace = Workspace::Folder("/owned-checkout".into());
    header.details.workspace = Some(Box::new(decision::records::WorkspacePreparation {
        project_cwd: "/project".into(),
        strategy: Some(WorkspaceStrategy::Worktree {
            base_ref: "main".into(),
            start_from_origin: false,
        }),
        generation,
        created_worktree: true,
        setup: Some(setup),
        visible: true,
    }));
    storage.create(&target, &header).unwrap();
    storage
        .decide(&target, submit(5000), context(5000), None)
        .unwrap();
    storage
        .decide(
            &target,
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(5001)),
                command: Command::Interrupt { turn: None },
                receipt: None,
            },
            context(5001),
            None,
        )
        .unwrap();
    let cancelled = storage.header(&target).unwrap().details.workspace.unwrap();
    assert_eq!(
        cancelled.setup.as_ref().unwrap().phase,
        SetupPhase::Cancelled
    );
    assert!(cancelled.created_worktree);
    assert_eq!(
        cancelled.setup.as_ref().unwrap().worktree_path.as_deref(),
        Some("/owned-checkout")
    );
    drop(storage); // The cancellation effect was never executed.
    let storage = Storage::open(&path);
    let recovered = storage
        .decide(
            &target,
            Event::Restart {
                updating: false,
                continue_after_restart: false,
            },
            context(5002),
            None,
        )
        .unwrap();
    assert!(
        matches!(&recovered.step.actions[..], [decision::Action::CancelPreparation { discard: Some((saved, checkout)), .. }] if *saved == generation && checkout.path == "/owned-checkout" && checkout.branch == "owned-branch")
    );
    storage
        .setup(&target, generation, workspace::SetupChange::Discarded, 103)
        .unwrap();
    let cleared = storage.header(&target).unwrap().details.workspace.unwrap();
    assert!(!cleared.created_worktree);
    assert!(cleared.setup.as_ref().unwrap().worktree_path.is_none());
    assert!(cleared.setup.as_ref().unwrap().branch.is_none());
    assert_eq!(
        storage.header(&target).unwrap().state.workspace.cwd(),
        Some("/project")
    );
}

#[test]
fn external_reimport_finishes_text_that_was_first_seen_while_running() {
    use agent_protocol::models::{
        ItemBody, ItemStatus, Thread as NativeThread, ThreadResponse, Turn as NativeTurn,
        TurnStatus,
    };
    for updating in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let mut storage = Storage::open(&path);
        let target = address("external-continuation");
        let response = |turn: &str, status, text: &str| ThreadResponse {
            thread: NativeThread {
                id: Some(target.clone()),
                turns: Some(vec![Arc::new(NativeTurn {
                    id: turn.into(),
                    status,
                    items: Some(vec![Arc::new(agent_protocol::items::Item::new(
                        format!("{turn}-answer").into(),
                        if status == TurnStatus::Running {
                            ItemStatus::Running
                        } else {
                            ItemStatus::Completed
                        },
                        ItemBody::AssistantText {
                            text: text.into(),
                            phase: Default::default(),
                        },
                    ))]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        };
        storage
            .import(
                &target,
                &response("old", TurnStatus::Completed, "earlier answer"),
                Some(&target),
                1,
            )
            .unwrap();
        for (at, status, text) in [
            (2, TurnStatus::Running, "partial"),
            (3, TurnStatus::Completed, "final answer"),
        ] {
            storage.queue_import(&target, &target, at).unwrap();
            let claim = storage
                .claim(at, 16)
                .unwrap()
                .into_iter()
                .find(|claim| matches!(claim.work, jobs::Work::Import { .. }))
                .unwrap();
            storage
                .reimport(
                    &target,
                    &target,
                    claim.id,
                    claim.attempt,
                    &response("new", status, text),
                    at,
                )
                .unwrap();
            if status == TurnStatus::Running {
                drop(storage);
                storage = Storage::open(&path);
                let restarted = storage
                    .decide(
                        &target,
                        Event::Restart {
                            updating,
                            continue_after_restart: true,
                        },
                        context(20),
                        None,
                    )
                    .unwrap();
                assert!(restarted.step.actions.is_empty());
                assert!(restarted.step.continuations.is_empty());
                assert!(matches!(
                    storage
                        .state(&target)
                        .unwrap()
                        .active_turn()
                        .map(|turn| &turn.state),
                    Some(TurnState::Running { .. })
                ));
                assert!(
                    storage
                        .claim(20, 16)
                        .unwrap()
                        .iter()
                        .all(|claim| !matches!(claim.work, jobs::Work::Continue { .. }))
                );
                assert!(
                    storage
                        .decide(&target, submit(99), context(99), None)
                        .unwrap()
                        .step
                        .actions
                        .is_empty(),
                    "external execution still owns the session"
                );
            }
        }
        let history = storage.history(&target, 0).unwrap();
        let turn = history.thread.turns.as_ref().unwrap().last().unwrap();
        assert_eq!(turn.status, TurnStatus::Completed);
        let item = &turn.items.as_ref().unwrap()[0];
        assert_eq!(item.status, ItemStatus::Completed);
        assert!(matches!(item.body(),ItemBody::AssistantText {text,..} if text=="final answer"));
    }
}

#[test]
fn a_failed_import_finishes_and_a_new_source_supersedes_an_old_claim() {
    use agent_protocol::models::{Thread as NativeThread, ThreadResponse};
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let target = address("import-lane");
    let response = ThreadResponse {
        thread: NativeThread {
            id: Some(target.clone()),
            turns: Some(vec![]),
            ..Default::default()
        },
        ..Default::default()
    };
    storage
        .import(&target, &response, Some(&target), 1)
        .unwrap();
    storage.queue_import(&target, &target, 2).unwrap();
    let first = storage
        .claim(2, 16)
        .unwrap()
        .into_iter()
        .find(|claim| matches!(claim.work, jobs::Work::Import { .. }))
        .unwrap();
    let failed = storage
        .import_failed(
            &target,
            first.id,
            first.attempt,
            "native session was deleted".into(),
        )
        .unwrap();
    assert!(matches!(failed.step.outcome, Outcome::Rejected(_)));
    assert!(
        !storage
            .retry(first.id, first.attempt, 3, "must not repeat")
            .unwrap()
    );
    assert!(storage.claim(100_000, 16).unwrap().is_empty());
    storage.queue_import(&target, &target, 3).unwrap();
    let stale = storage
        .claim(3, 16)
        .unwrap()
        .into_iter()
        .find(|claim| matches!(claim.work, jobs::Work::Import { .. }))
        .unwrap();
    let replacement = SessionRef::new(ProviderKind::Codex, "replacement-source".into()).unwrap();
    storage.queue_import(&target, &replacement, 4).unwrap();
    let current = storage.claim(4, 16).unwrap();
    assert!(
        matches!(&current[..],[jobs::Claim {work:jobs::Work::Import {source,..},..}] if source==&replacement)
    );
    let cursor = storage.with(|connection| cursor(connection)).unwrap();
    let ignored = storage
        .import_failed(&target, stale.id, stale.attempt, "late old failure".into())
        .unwrap();
    assert_eq!(ignored.cursor, cursor);
    assert!(ignored.rows.is_empty());
    let replay = storage
        .reimport(&target, &target, stale.id, stale.attempt, &response, 5)
        .unwrap();
    assert!(replay.rows.is_empty());
    assert!(
        !storage
            .retry(stale.id, stale.attempt, 6, "must not block replacement")
            .unwrap()
    );
}

#[test]
fn projects_keep_registration_order_in_storage_and_synced_positions() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    for id in ["z", "a", "m"] {
        storage
            .register_project(&agent_protocol::models::Project {
                id: id.into(),
                name: id.into(),
                roots: vec![agent_protocol::models::ProjectRoot {
                    path: format!("/{id}"),
                }],
                ..Default::default()
            })
            .unwrap();
    }
    assert_eq!(
        storage
            .projects()
            .unwrap()
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["z", "a", "m"]
    );
    let rows = storage
        .with(|connection| projects::read(connection, 0))
        .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| row.position.clone())
            .collect::<Vec<_>>(),
        [
            Position::Project { position: 1 },
            Position::Project { position: 2 },
            Position::Project { position: 3 },
        ]
    );
}

#[test]
fn provider_switches_publish_the_live_steering_admission_in_both_directions() {
    for (from, to, steers) in [
        (ProviderKind::Codex, ProviderKind::DeepSeek, false),
        (ProviderKind::DeepSeek, ProviderKind::Codex, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let mut initial = header();
        initial.state.provider = from;
        initial.details.capabilities.steer = !steers;
        storage.create(&target, &initial).unwrap();
        ready(&storage, &target);
        let mut command = submit(701);
        let Event::Command {
            command: Command::Submit { content, .. },
            ..
        } = &mut command
        else {
            panic!("submit");
        };
        content.model = Some(agent_protocol::models::ModelRef {
            provider: to,
            id: "model".into(),
        });
        let switching = storage
            .decide(
                &target,
                command,
                Context {
                    steers: !steers,
                    ..context(701)
                },
                None,
            )
            .unwrap();
        let [decision::Action::Prepare { id, action, .. }] = &switching.step.actions[..] else {
            panic!("switch must prepare the target provider");
        };
        let prepared = storage
            .decide(
                &target,
                Event::Prepared {
                    id: id.clone(),
                    action: *action,
                    cwd: Some("/project".into()),
                    source: Some("target-native".into()),
                },
                Context {
                    steers,
                    ..context(702)
                },
                None,
            )
            .unwrap();
        let [decision::Action::Deliver { id, action, .. }] = &prepared.step.actions[..] else {
            panic!("prepared switch must deliver");
        };
        storage
            .decide(
                &target,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: Some("target-turn".into()),
                },
                Context {
                    steers,
                    ..context(703)
                },
                None,
            )
            .unwrap();
        let thread = storage.history(&target, 0).unwrap().thread;
        assert_eq!(thread.active_provider, Some(to));
        assert_eq!(thread.capabilities.unwrap().steer, steers);
        assert_eq!(thread.steerable, steers);
        assert!(!thread.skips_compaction);
        let mut follow_up = submit(704);
        let Event::Command {
            command: Command::Submit { content, .. },
            ..
        } = &mut follow_up
        else {
            panic!("submit");
        };
        content.follow_up = FollowUp::Steer;
        let sent = storage
            .decide(
                &target,
                follow_up,
                Context {
                    steers,
                    ..context(704)
                },
                None,
            )
            .unwrap();
        assert_eq!(
            sent.step
                .actions
                .iter()
                .any(|action| matches!(action, decision::Action::Deliver { steer: true, .. })),
            steers
        );
        if !steers {
            assert!(matches!(
                storage.state(&target).unwrap().inputs[Uuid::from_u128(704).to_string().as_str()]
                    .state,
                InputState::Queued { steer: None }
            ));
        }
    }
}

#[test]
fn projected_steerability_tracks_compaction_stop_and_capacity_waits() {
    use agent_protocol::{conversation::Observation, execution::ExecutionError};
    for (text, ending) in [("hello", "stop"), ("hello", "retry"), ("/compact", "stop")] {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("state.sqlite"));
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let mut initial = header();
        initial.state.provider = ProviderKind::Codex;
        initial.details.capabilities.steer = text == "hello";
        storage.create(&target, &initial).unwrap();
        ready(&storage, &target);
        let mut event = submit(601);
        let Event::Command {
            command: Command::Submit { content, .. },
            ..
        } = &mut event
        else {
            panic!("submit")
        };
        content.parts = vec![Input::Text { text: text.into() }];
        let started = storage.decide(&target, event, context(601), None).unwrap();
        let [
            decision::Action::Deliver {
                id,
                action,
                attempt,
                ..
            },
        ] = &started.step.actions[..]
        else {
            panic!("delivery")
        };
        let thread = storage.history(&target, 0).unwrap().thread;
        assert!(!thread.steerable, "unacknowledged {text}");
        assert_eq!(thread.skips_compaction, text == "/compact");
        storage
            .decide(
                &target,
                Event::Delivered {
                    id: id.clone(),
                    action: *action,
                    native_turn: Some("native-turn".into()),
                },
                context(602),
                None,
            )
            .unwrap();
        let thread = storage.history(&target, 0).unwrap().thread;
        assert_eq!(thread.steerable, text != "/compact", "acknowledged {text}");
        assert_eq!(thread.skips_compaction, text == "/compact");
        if text == "/compact" {
            let mut queued = submit(604);
            let Event::Command {
                command: Command::Submit { content, .. },
                ..
            } = &mut queued
            else {
                panic!("submit");
            };
            content.follow_up = FollowUp::Steer;
            let queued = storage.decide(&target, queued, context(604), None).unwrap();
            assert_eq!(queued.step.outcome, Outcome::Accepted);
            assert!(queued.step.actions.is_empty());
            assert!(matches!(
                storage.state(&target).unwrap().inputs[Uuid::from_u128(604).to_string().as_str()]
                    .state,
                InputState::Queued { steer: None }
            ));
        }
        let terminal = if ending == "stop" {
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(603)),
                command: Command::Interrupt { turn: None },
                receipt: None,
            }
        } else {
            Event::Provider {
                attempt: attempt.id,
                native_turn: None,
                native_items: vec![],
                observation: Observation::Failed(Some(ExecutionError {
                    provider_code: Some("serverOverloaded".into()),
                    category: agent_protocol::execution::ErrorCategory::Overloaded,
                    ..Default::default()
                })),
            }
        };
        storage
            .decide(&target, terminal, context(603), None)
            .unwrap();
        let thread = storage.history(&target, 0).unwrap().thread;
        assert!(!thread.steerable, "{text} / {ending}");
        assert_eq!(thread.skips_compaction, text == "/compact");
    }
}
