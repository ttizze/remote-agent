use super::*;
use proptest::prelude::*;

#[path = "stream_tests.rs"]
mod stream;

fn cursor(sequence: u64) -> Cursor {
    Cursor {
        store: uuid::Uuid::from_u128(1),
        sequence,
    }
}

fn scope() -> Scope<String> {
    Scope::Conversation("a".into())
}
fn empty() -> Replica<String, String> {
    Replica::new(scope(), 1)
}
fn window(item: u64, turn: u64) -> Option<Window> {
    Some(Window { item, turn })
}
fn input(id: &str, version: u64, value: &str) -> Row<String, String> {
    Row {
        key: Key::Input {
            thread: "a".into(),
            id: id.into(),
        },
        version,
        position: Position::Unordered,
        content: Content::Full(value.into()),
    }
}
fn turn(id: &str, version: u64, ordinal: u64) -> Row<String, String> {
    Row {
        key: Key::Turn {
            thread: "a".into(),
            id: id.into(),
        },
        version,
        position: Position::Turn { ordinal },
        content: Content::Full(id.into()),
    }
}
fn item(id: &str, version: u64, position: u64, parent: &str, ordinal: u64) -> Row<String, String> {
    Row {
        key: Key::Item {
            thread: "a".into(),
            id: id.into(),
        },
        version,
        position: Position::Item {
            turn: parent.into(),
            ordinal,
            position,
        },
        content: Content::Full(id.into()),
    }
}
fn snapshot(
    at: u64,
    floor: Option<Window>,
    rows: Vec<Row<String, String>>,
) -> Message<String, String> {
    Message::Snapshot {
        cursor: cursor(at),
        window: floor,
        rows,
    }
}
fn changes(
    from: u64,
    through: u64,
    rows: Vec<Row<String, String>>,
    removed: Vec<Removal<String>>,
) -> Message<String, String> {
    Message::Changes {
        from: cursor(from),
        through: cursor(through),
        rows,
        removed,
    }
}

#[test]
fn two_changed_targets_survive_catchup_and_duplicate_delivery() {
    let cached = empty().apply(1, snapshot(9, window(0, 0), vec![])).unwrap();
    let frame = changes(
        9,
        11,
        vec![input("a", 10, "A"), input("b", 11, "B")],
        vec![],
    );
    let next = cached.apply(1, frame.clone()).unwrap();
    assert_eq!(next.rows.len(), 2);
    assert_eq!(next.cursor, Some(cursor(11)));
    assert_eq!(next.apply(1, frame).unwrap(), next);
    assert!(cached.rows.is_empty());
}

#[test]
fn snapshot_replaces_cache_including_after_database_recreation() {
    let cached = empty()
        .apply(1, snapshot(30, window(0, 0), vec![input("old", 30, "old")]))
        .unwrap();
    let mut frame = snapshot(1, window(0, 0), vec![input("new", 1, "new")]);
    if let Message::Snapshot { cursor, .. } = &mut frame {
        cursor.store = uuid::Uuid::from_u128(2);
    }
    let clean = empty().apply(1, frame.clone()).unwrap();
    assert_eq!(cached.apply(1, frame).unwrap(), clean);
    let renewed = clean.resubscribe(2).unwrap();
    assert_eq!(
        renewed
            .apply(1, snapshot(100, window(0, 0), vec![]))
            .unwrap(),
        renewed
    );
}

#[test]
fn a_new_generation_receives_current_frames_and_rejects_old_ones() {
    let cached = empty().apply(1, snapshot(0, window(0, 0), vec![])).unwrap();
    let renewed = cached.resubscribe(2).unwrap();
    let current = renewed
        .apply(
            2,
            snapshot(4, window(0, 0), vec![input("new", 4, "current")]),
        )
        .unwrap();
    assert_eq!(current.cursor, Some(cursor(4)));
    assert_eq!(current.rows.len(), 1);
    assert_eq!(
        current
            .apply(1, snapshot(100, window(0, 0), vec![]))
            .unwrap(),
        current
    );
}

#[test]
fn missing_predecessor_or_wrong_store_cannot_advance_the_cursor() {
    let cached = empty().apply(1, snapshot(9, window(0, 0), vec![])).unwrap();
    assert_eq!(
        cached.apply(1, changes(10, 11, vec![input("b", 11, "B")], vec![])),
        Err(ApplyError::Discontinuity)
    );
    let mut frame = changes(9, 11, vec![], vec![]);
    if let Message::Changes { through, .. } = &mut frame {
        through.store = uuid::Uuid::from_u128(2);
    }
    assert_eq!(cached.apply(1, frame), Err(ApplyError::Store));
    assert_eq!(cached.cursor, Some(cursor(9)));
}

#[test]
fn extension_changes_membership_before_subsequent_live_changes() {
    let cached = empty()
        .apply(
            1,
            snapshot(
                4,
                window(100, 2),
                vec![turn("recent", 4, 2), item("recent", 4, 100, "recent", 2)],
            ),
        )
        .unwrap();
    let expanded = cached
        .apply(
            1,
            Message::Extend {
                cursor: cursor(4),
                window: Window { item: 50, turn: 1 },
                rows: vec![turn("old", 1, 1), item("old", 1, 75, "old", 1)],
            },
        )
        .unwrap();
    let next = expanded
        .apply(1, changes(4, 5, vec![item("old", 5, 75, "old", 1)], vec![]))
        .unwrap();
    assert_eq!(next.rows[&item("old", 5, 75, "old", 1).key].version, 5);
    assert_eq!(cached.window, window(100, 2));
    assert!(!cached.rows.contains_key(&item("old", 1, 75, "old", 1).key));
}

#[test]
fn removing_a_turn_cascades_items_and_keeps_the_loaded_window() {
    let cached = empty()
        .apply(
            1,
            snapshot(
                2,
                window(100, 2),
                vec![turn("t", 1, 2), item("i", 2, 100, "t", 2)],
            ),
        )
        .unwrap();
    let next = cached
        .apply(
            1,
            changes(
                2,
                3,
                vec![],
                vec![Removal {
                    key: turn("t", 1, 2).key,
                    version: 3,
                }],
            ),
        )
        .unwrap();
    assert!(next.rows.is_empty());
    assert_eq!(next.cursor, Some(cursor(3)));
    assert_eq!(next.window, cached.window);
}

#[test]
fn an_empty_running_turn_is_in_scope_and_an_old_item_is_not() {
    let next = empty()
        .apply(
            1,
            snapshot(
                10,
                window(100, 3),
                vec![
                    turn("running", 10, 3),
                    turn("earlier", 1, 1),
                    item("outside", 1, 50, "earlier", 1),
                ],
            ),
        )
        .unwrap();
    assert_eq!(next.rows.len(), 1);
    assert!(next.rows.contains_key(&turn("running", 10, 3).key));
}

#[test]
fn late_items_in_old_turns_join_the_range_when_the_turn_is_loaded() {
    let cached = empty()
        .apply(
            1,
            snapshot(
                10,
                window(100, 3),
                vec![
                    turn("running", 10, 3),
                    turn("earlier", 1, 1),
                    item("late", 10, 101, "earlier", 1),
                ],
            ),
        )
        .unwrap();
    assert_eq!(cached.rows.len(), 1);
    let expanded = cached
        .apply(
            1,
            Message::Extend {
                cursor: cursor(10),
                window: Window { item: 50, turn: 1 },
                rows: vec![turn("earlier", 1, 1), item("late", 10, 101, "earlier", 1)],
            },
        )
        .unwrap();
    assert_eq!(expanded.rows.len(), 3);
}

#[test]
fn turn_order_precedes_item_creation_order_when_opening_and_extending() {
    let rows = vec![
        turn("older", 1, 1),
        item("late", 10, 101, "older", 1),
        turn("boundary", 2, 2),
        item("boundary", 2, 100, "boundary", 2),
        turn("newer", 3, 3),
        item("newer", 3, 50, "newer", 3),
    ];
    let cached = empty()
        .apply(1, snapshot(10, window(100, 2), rows))
        .unwrap();
    assert!(
        cached
            .rows
            .contains_key(&item("newer", 3, 50, "newer", 3).key)
    );
    assert!(
        !cached
            .rows
            .contains_key(&item("late", 10, 101, "older", 1).key)
    );
    let extended = cached
        .apply(
            1,
            Message::Extend {
                cursor: cursor(10),
                window: Window { item: 101, turn: 1 },
                rows: vec![turn("older", 1, 1), item("late", 10, 101, "older", 1)],
            },
        )
        .unwrap();
    assert!(
        extended
            .rows
            .contains_key(&item("late", 10, 101, "older", 1).key)
    );
    assert!(
        extended
            .rows
            .contains_key(&item("newer", 3, 50, "newer", 3).key)
    );
}

#[test]
fn hydration_cannot_advance_a_stale_rows_version_without_its_live_change() {
    let mut deferred = item("i", 2, 1, "t", 1);
    deferred.content = Content::Summary("summary".into());
    let cached = empty()
        .apply(
            1,
            snapshot(4, window(0, 0), vec![turn("t", 1, 1), deferred]),
        )
        .unwrap();
    assert_eq!(
        cached
            .apply(
                1,
                Message::Hydrate {
                    cursor: cursor(4),
                    row: item("i", 3, 2, "t", 1)
                }
            )
            .unwrap(),
        cached
    );
}

#[test]
fn unrelated_streaming_does_not_starve_a_valid_hydration() {
    let mut deferred = item("i", 2, 1, "t", 1);
    deferred.content = Content::Summary("summary".into());
    let cached = empty()
        .apply(
            1,
            snapshot(2, window(0, 0), vec![turn("t", 1, 1), deferred]),
        )
        .unwrap();
    let streaming = cached
        .apply(1, changes(2, 10, vec![input("another", 10, "new")], vec![]))
        .unwrap();
    let full = item("i", 2, 1, "t", 1);
    let hydrated = streaming
        .apply(
            1,
            Message::Hydrate {
                cursor: cursor(2),
                row: full.clone(),
            },
        )
        .unwrap();
    assert_eq!(hydrated.rows()[&full.key], full);
    assert_eq!(hydrated.cursor(), Some(cursor(10)));
    let removed = streaming
        .apply(
            1,
            changes(
                10,
                11,
                vec![],
                vec![Removal {
                    key: full.key.clone(),
                    version: 11,
                }],
            ),
        )
        .unwrap();
    assert_eq!(
        removed
            .apply(
                1,
                Message::Hydrate {
                    cursor: cursor(2),
                    row: full
                }
            )
            .unwrap(),
        removed
    );
}

#[test]
fn hydration_fills_only_missing_content_at_the_same_version() {
    let mut deferred = item("i", 2, 1, "t", 1);
    deferred.content = Content::Summary("summary".into());
    let cached = empty()
        .apply(
            1,
            snapshot(2, window(0, 0), vec![turn("t", 1, 1), deferred]),
        )
        .unwrap();
    let full = item("i", 2, 1, "t", 1);
    let next = cached
        .apply(
            1,
            Message::Hydrate {
                cursor: cursor(2),
                row: full.clone(),
            },
        )
        .unwrap();
    assert_eq!(next.rows[&full.key], full);
    assert_eq!(next.cursor, cached.cursor);
    let older = item("i", 1, 1, "t", 1);
    assert_eq!(
        next.apply(
            1,
            Message::Hydrate {
                cursor: cursor(2),
                row: older
            }
        )
        .unwrap(),
        next
    );
}

#[test]
fn malformed_frames_are_atomic_and_cannot_create_orphans() {
    let cached = empty()
        .apply(
            1,
            snapshot(2, window(0, 0), vec![input("a", 1, "original")]),
        )
        .unwrap();
    assert_eq!(
        cached.apply(
            1,
            changes(
                2,
                3,
                vec![input("a", 3, "new"), input("bad", 4, "future")],
                vec![]
            )
        ),
        Err(ApplyError::Version)
    );
    assert_eq!(
        cached.apply(
            1,
            changes(2, 3, vec![item("orphan", 3, 1, "missing", 1)], vec![])
        ),
        Err(ApplyError::Parent)
    );
    assert_eq!(
        cached.rows[&input("a", 1, "original").key].content,
        Content::Full("original".into())
    );
}

#[test]
fn list_and_conversation_cursors_and_id_namespaces_are_independent() {
    let list = Replica::<String, String>::new(Scope::List, 1);
    let a = Row {
        key: Key::Thread("a".into()),
        version: 3,
        position: Position::Unordered,
        content: Content::Full("A".into()),
    };
    let b = Row {
        key: Key::Project("a".into()),
        version: 3,
        position: Position::Project { position: 1 },
        content: Content::Full("project".into()),
    };
    let next = list
        .apply(
            1,
            snapshot(3, None, vec![a, b, input("out-of-scope", 3, "ignored")]),
        )
        .unwrap();
    assert_eq!(next.rows.len(), 2);
    let a = empty()
        .apply(1, snapshot(1, window(0, 0), vec![turn("same-id", 1, 1)]))
        .unwrap();
    let mut foreign = vec![
        input("same-id", 2, "other"),
        turn("same-id", 2, 1),
        item("same-id", 2, 1, "same-id", 1),
    ];
    for row in &mut foreign {
        match &mut row.key {
            Key::Input { thread, .. } | Key::Turn { thread, .. } | Key::Item { thread, .. } => {
                *thread = "b".into()
            }
            _ => unreachable!(),
        }
    }
    let next = a.apply(1, changes(1, 2, foreign, vec![])).unwrap();
    assert_eq!(next.rows(), a.rows());
}

#[test]
fn resume_boundaries_include_equal_history_and_pruning_sequences() {
    for c in 0..=12 {
        assert_eq!(can_resume(Some(cursor(c)), cursor(12), 5, 7), c >= 7);
    }
    assert!(can_resume(Some(cursor(5)), cursor(12), 5, 0));
    assert!(!can_resume(Some(cursor(4)), cursor(12), 5, 0));
    assert!(!can_resume(None, cursor(12), 0, 0));
    assert!(!can_resume(Some(cursor(13)), cursor(12), 0, 0));
    let other = Cursor {
        store: uuid::Uuid::from_u128(2),
        sequence: 12,
    };
    assert!(!can_resume(Some(other), cursor(12), 0, 0));
}

#[test]
fn only_applying_the_replacement_snapshot_makes_its_cursor_resumable() {
    let cached = empty()
        .apply(
            1,
            snapshot(
                1,
                window(100, 1),
                vec![turn("t", 1, 1), item("i", 1, 100, "t", 1)],
            ),
        )
        .unwrap();
    assert!(!can_resume(cached.cursor, cursor(2), 0, 2));
    // History replacement occupies one stream position with a snapshot, never
    // a preceding Changes frame that could advance the cursor before the reset.
    let replaced = cached
        .apply(
            1,
            snapshot(
                2,
                window(50, 1),
                vec![turn("t", 2, 1), item("i", 2, 50, "t", 1)],
            ),
        )
        .unwrap();
    assert_eq!(
        replaced.rows[&item("i", 2, 50, "t", 1).key].position,
        item("i", 2, 50, "t", 1).position
    );
    assert!(can_resume(replaced.cursor, cursor(2), 0, 2));
}

#[test]
fn wire_roundtrip_preserves_existing_conversation_identity_and_scope() {
    let id = crate::session::SessionRef::new(
        crate::session::ProviderKind::Claude,
        "bex-task:existing".into(),
    )
    .unwrap();
    let frame = Message::Snapshot {
        cursor: cursor(4),
        window: Some(Window { item: 0, turn: 0 }),
        rows: vec![Row {
            key: Key::Thread(id.clone()),
            version: 4,
            position: Position::Unordered,
            content: Content::Full("existing".to_owned()),
        }],
    };
    let bytes = crate::protocol::encode(&frame).unwrap();
    let decoded: Message<crate::session::SessionRef, String> =
        crate::protocol::decode(&bytes).unwrap();
    assert_eq!(decoded, frame);
    let state = Replica::new(Scope::Conversation(id.clone()), 1)
        .apply(1, decoded)
        .unwrap();
    assert!(state.rows.contains_key(&Key::Thread(id)));
}

#[test]
fn stale_extension_invalid_window_and_same_version_reparenting_are_rejected() {
    let cached = empty()
        .apply(
            1,
            snapshot(
                4,
                window(10, 2),
                vec![turn("t", 4, 2), item("i", 4, 10, "t", 2)],
            ),
        )
        .unwrap();
    assert_eq!(
        cached.apply(
            1,
            Message::Extend {
                cursor: cursor(3),
                window: Window { item: 0, turn: 0 },
                rows: vec![]
            }
        ),
        Err(ApplyError::Discontinuity)
    );
    assert_eq!(
        cached.apply(
            1,
            Message::Extend {
                cursor: cursor(4),
                window: Window { item: 11, turn: 2 },
                rows: vec![]
            }
        ),
        Err(ApplyError::Window)
    );
    assert_eq!(
        cached.apply(
            1,
            Message::Hydrate {
                cursor: cursor(4),
                row: item("i", 4, 11, "t", 2)
            }
        ),
        Err(ApplyError::Position)
    );
    let missing = cached.apply(1, Message::Missing).unwrap();
    assert!(missing.rows.is_empty());
    assert!(missing.cursor.is_none());
    let restored = cached.apply(1, snapshot(3, window(0, 0), vec![])).unwrap();
    assert_eq!(restored.cursor, Some(cursor(3)));
    assert!(restored.rows.is_empty());
    assert_eq!(cached.resubscribe(1), Err(ApplyError::Generation));
}

proptest! {
    #[test]
    fn arbitrary_live_and_catchup_histories_converge(
        actions in prop::collection::vec((any::<bool>(), 0usize..16, any::<u16>()), 0..80),
        disconnect_at in 0usize..80,
    ) {
        // Oracle: the Host's current input values, independent of replica application.
        // A removed ID is never reused. A later creation gets a fresh ID.
        let mut host = std::collections::BTreeMap::<String, (u64, String)>::new();
        let mut deletions = Vec::new();
        let mut client = empty().apply(1, snapshot(0, window(0, 0), vec![])).unwrap();
        let mut seq = 0;
        let mut unique = 0;
        let mut frozen = None;
        for (index, (delete, target, value)) in actions.into_iter().enumerate() {
            if index == disconnect_at { frozen = Some(client.clone()); }
            let previous = seq;
            seq += 1;
            let existing = host.keys().nth(target % host.len().max(1)).cloned();
            let (rows, removed) = if delete && existing.is_some() {
                let id = existing.unwrap(); host.remove(&id);
                let deletion = Removal { key: input(&id, seq, "").key, version: seq };
                deletions.push(deletion.clone()); (vec![], vec![deletion])
            } else {
                let id = existing.unwrap_or_else(|| { unique += 1; format!("input-{unique}") });
                let value = value.to_string(); host.insert(id.clone(), (seq, value.clone()));
                (vec![input(&id, seq, &value)], vec![])
            };
            client = client.apply(1, changes(previous, seq, rows, removed)).unwrap();
            let expected: std::collections::BTreeMap<_, _> = host.iter().map(|(id,(v,text))| {
                let row = input(id,*v,text); (row.key.clone(),row)
            }).collect();
            prop_assert_eq!(&client.rows, &expected);
        }
        let frozen = frozen.unwrap_or_else(|| empty().apply(1, snapshot(0, window(0,0), vec![])).unwrap());
        let from = frozen.cursor.unwrap().sequence;
        let rows = host.iter().filter(|(_, (v,_))| *v > from).map(|(id,(v,text))| input(id,*v,text)).collect();
        let removed = deletions.into_iter().filter(|d| d.version > from).collect();
        let caught_up = frozen.apply(1, changes(from,seq,rows,removed)).unwrap();
        prop_assert_eq!(&caught_up.rows, &client.rows);
        let snap = snapshot(seq,window(0,0),host.iter().map(|(id,(v,text))| input(id,*v,text)).collect());
        prop_assert_eq!(caught_up, empty().apply(1,snap).unwrap());
    }
}
