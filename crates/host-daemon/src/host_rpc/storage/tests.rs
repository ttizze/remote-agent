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
fn context(seed: u128) -> Context {
    Context {
        external_busy: false,
        now: 100,
        seed: Uuid::from_u128(seed),
        steers: true,
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
        matches!(&row.content,Content::Full(record) if matches!(record.as_ref(),Record::Thread(header) if header.state.name=="Renamed"))
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
                provider: ProviderKind::Claude,
                value: Some(Binding::Ready {
                    covered: state.history_end(),
                    source: format!("native-{}", id.id),
                }),
            };
            state = state.apply(std::slice::from_ref(&binding));
            rows::write(
                &transaction,
                key,
                id,
                &binding,
                &state,
                0,
                0,
                storage.process,
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
    assert_eq!(first.state.as_ref().unwrap().inputs.len(), 1);
    assert!(first.state.as_ref().unwrap().turns.is_empty());
    let keys = first
        .rows
        .iter()
        .map(|row| &row.key)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), first.rows.len());
    let replay = storage.decide(&id, submit(1), context(2), None).unwrap();
    assert_eq!(replay.cursor, first.cursor);
    assert_eq!(replay.state, first.state);
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
    assert_eq!(created.state.as_ref().unwrap().inputs.len(), 1);
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
    assert!(!recovery.state.as_ref().unwrap().permits(&action));
    assert!(matches!(
        recovery
            .state
            .as_ref()
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
    let updated = storage
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
    let position = updated.state.as_ref().unwrap().items["reply"].position;
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
    let late = storage
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
        late.state.as_ref().unwrap().items["reply"].position,
        position
    );
    assert_eq!(
        late.state.as_ref().unwrap().turns[&attempt.turn].state,
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
    assert_eq!(admitted.state.unwrap().inputs.len(), 1);
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
    assert!(replay.state.is_none());
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
                    enabled: true,
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
        for (index, expected) in ["first updated", "second updated"].into_iter().enumerate() {
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
        admitted
            .state
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
    storage.catalog_page(&retry, &[], None, 1001).unwrap();
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
    storage.import_status(&id, None, 100).unwrap();
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
    storage
        .catalog_page(
            &again,
            &[(
                child,
                child_header,
            )],
            None,
            60_100,
        )
        .unwrap();
    let message = storage.list(replica.cursor()).unwrap();
    let replica = replica.apply(1, message).unwrap();
    assert_eq!(replica.rows()[&key], before);
}
