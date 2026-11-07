#[cfg(test)]
mod tests {
    use crate::host_rpc::storage::*;
    use agent_protocol::{
        execution::TurnStatus,
        models::{Item, SessionStatus, ThreadResponse, Turn},
        session::SessionRef,
    };
    const WINDOW_BYTES: usize = 1 << 20;
    use agent_protocol::{
        items::{AssistantPhase, ItemBody},
        models::Thread,
        session::ProviderKind,
    };
    use std::sync::Arc;

    fn conversation() -> SessionRef {
        SessionRef::new(ProviderKind::Codex, "conversation".into()).unwrap()
    }

    fn turn(id: &str, items: &[&str], status: TurnStatus) -> Turn {
        Turn {
            id: id.into(),
            status,
            items: Some(
                items
                    .iter()
                    .map(|item| {
                        Arc::new(Item::new(
                            (*item).into(),
                            Default::default(),
                            ItemBody::AssistantText {
                                text: format!("{id}/{item}"),
                                phase: AssistantPhase::Final,
                            },
                        ))
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn history(turns: Vec<Turn>) -> ThreadResponse {
        ThreadResponse {
            thread: Thread {
                id: Some(conversation()),
                name: Some("native".into()),
                status: SessionStatus::Running,
                turns: Some(turns.into_iter().map(Arc::new).collect()),
                ..Default::default()
            },
            model: None,
            effort: None,
            service_tier: None,
        }
    }

    fn store() -> (tempfile::TempDir, Database) {
        let root = tempfile::tempdir().unwrap();
        let store = Database::open(&root.path().join("state/transcripts.sqlite"));
        (root, store)
    }

    fn texts(window: &Window) -> Vec<String> {
        window
            .response
            .thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .map(|item| match item.body() {
                ItemBody::AssistantText { text, .. } => text.clone(),
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn bound_native_aliases_are_neither_list_roots_nor_reimport_targets() {
        let (_root, database) = store();
        database
            .import(
                &conversation(),
                history(vec![turn("native", &["message"], TurnStatus::Completed)]),
            )
            .unwrap();
        let mut row = crate::host_rpc::conversations::Conversation::new(&Thread {
            id: Some(conversation()),
            cwd: Some("/workspace".into()),
            ..Default::default()
        })
        .unwrap();
        let public = SessionRef::new(ProviderKind::Codex, "public".into()).unwrap();
        row.id = public.clone();
        database.save(row).unwrap();
        assert_eq!(
            database
                .titles("")
                .unwrap()
                .into_iter()
                .map(|title| title.thread.id.unwrap())
                .collect::<Vec<_>>()
                .as_slice(),
            std::slice::from_ref(&public)
        );
        assert!(database.imported().unwrap().is_empty());
        database
            .import(
                &public,
                history(vec![turn("app", &["app-message"], TurnStatus::Completed)]),
            )
            .unwrap();
        assert_eq!(
            database
                .imported()
                .unwrap()
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>(),
            [public]
        );
    }

    #[test]
    fn repeated_empty_native_occurrences_keep_distinct_runs_and_stable_identity() {
        let (_root, database) = store();
        let empty = turn("same", &[], TurnStatus::Completed);
        database
            .import(&conversation(), history(vec![empty.clone(), empty.clone()]))
            .unwrap();
        let ids = || {
            database
                .read(|connection| {
                    let mut query = connection
                        .prepare("SELECT run_id FROM runs ORDER BY ordinal")
                        .map_err(failure)?;
                    query
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(failure)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(failure)
                })
                .unwrap()
        };
        let original = ids();
        assert_ne!(original[0], original[1]);
        database
            .import(
                &conversation(),
                history(vec![
                    turn("older", &["older-item"], TurnStatus::Completed),
                    empty.clone(),
                    empty,
                ]),
            )
            .unwrap();
        let imported = ids();
        assert_eq!(&imported[1..], original.as_slice());
        assert_eq!(
            database
                .history(&conversation())
                .unwrap()
                .unwrap()
                .thread
                .turns
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn streaming_updates_replace_one_body_and_keep_deletion_revisions() {
        let (root, database) = store();
        database.import(&conversation(), history(vec![])).unwrap();
        let mut previous = 0;
        for length in 1..=100 {
            let mut turn = turn("stream", &["message"], TurnStatus::Running);
            turn.items = Some(vec![Arc::new(Item::new(
                "message".into(),
                Default::default(),
                ItemBody::AssistantText {
                    text: "x".repeat(length),
                    phase: AssistantPhase::Final,
                },
            ))]);
            database.record(&conversation(), &turn, None).unwrap();
            let (revision, changes, items): (i64, i64, i64) = database.read(|connection| {
                connection.query_row("SELECT max(sequence), count(*), (SELECT count(*) FROM turn_items) FROM changes", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(failure)
            }).unwrap();
            assert!(revision > previous);
            previous = revision;
            assert_eq!((changes, items), (3, 1));
        }
        drop(database);
        let database = Database::open(&root.path().join("state/transcripts.sqlite"));
        database.interrupt_running().unwrap();
        let restored = database
            .history(&conversation())
            .unwrap()
            .unwrap()
            .thread
            .turns
            .unwrap();
        assert_eq!(restored[0].status, TurnStatus::Interrupted);
        assert!(
            matches!(restored[0].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText { text, .. } if text == &"x".repeat(100))
        );
        database
            .record(
                &conversation(),
                &turn("stream", &[], TurnStatus::Interrupted),
                Some(&"message".into()),
            )
            .unwrap();
        let deleted: bool = database
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT deleted FROM changes WHERE entity_type = 'item'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(failure)
            })
            .unwrap();
        assert!(deleted);
        assert!(
            database
                .history(&conversation())
                .unwrap()
                .unwrap()
                .thread
                .turns
                .unwrap()[0]
                .items
                .as_ref()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn imports_match_repeated_turn_ids_by_items_and_keep_unmatched_occurrences() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                history(vec![
                    turn("reused", &["a"], TurnStatus::Completed),
                    turn("reused", &["b"], TurnStatus::Failed),
                    turn("other", &["tail"], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        let mut shared = turn("reused", &["b", "native-only"], TurnStatus::Running);
        shared.provider = Some(ProviderKind::Codex);
        shared.supports_fork = Some(true);
        store
            .import(
                &target,
                history(vec![
                    shared,
                    turn("reused", &["new"], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        let saved = store
            .history(&target)
            .unwrap()
            .unwrap()
            .thread
            .turns
            .unwrap();
        assert_eq!(
            saved
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["reused", "reused", "reused", "other"]
        );
        assert_eq!(saved[0].status, TurnStatus::Failed);
        assert_eq!(saved[0].provider, Some(ProviderKind::Codex));
        assert_eq!(saved[0].supports_fork, Some(true));
        assert_eq!(
            saved
                .iter()
                .flat_map(|turn| turn.items.iter().flatten())
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "new", "a", "tail"]
        );

        // An itemless native observation matches the first stored occurrence
        // and suppresses all stored occurrences covered by that observation.
        store
            .import(
                &target,
                history(vec![turn("reused", &[], TurnStatus::Running)]),
            )
            .unwrap();
        let saved = store
            .history(&target)
            .unwrap()
            .unwrap()
            .thread
            .turns
            .unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].items.as_ref().unwrap()[0].id.as_str(), "b");
        assert_eq!(saved[1].id.as_str(), "other");
    }

    #[test]
    fn recorded_turns_stay_hidden_until_import_and_keep_their_items() {
        let (_root, store) = store();
        let target = conversation();
        store
            .record(
                &target,
                &turn("shared", &["live"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("later", &["after"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        assert!(store.window(&target, None, 10).unwrap().is_none());
        assert!(store.turn(&target, &"later".into()).unwrap().is_none());
        assert!(
            store
                .item(&target, &"later".into(), &"after".into(), true)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .item(&target, &"later".into(), &"after".into(), false)
                .unwrap()
                .is_some()
        );

        let mut native = turn("shared", &["live"], TurnStatus::Completed);
        Arc::make_mut(&mut native.items.as_mut().unwrap()[0]).body =
            agent_protocol::items::ItemContent::Inline {
                body: Box::new(ItemBody::AssistantText {
                    text: "native".into(),
                    phase: AssistantPhase::Final,
                }),
            };
        store
            .import(
                &target,
                history(vec![turn("first", &["a"], TurnStatus::Completed), native]),
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["first/a", "shared/live", "later/after"]);

        // A turn that finished while its native history was read keeps the
        // recorded result instead of the unfinished read.
        let running = SessionRef::new(ProviderKind::Codex, "running".into()).unwrap();
        store
            .record(
                &running,
                &turn("racing", &["start", "end"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        store
            .import(
                &running,
                history(vec![turn("racing", &["start"], TurnStatus::Running)]),
            )
            .unwrap();
        let window = store.window(&running, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["racing/start", "racing/end"]);
        assert_eq!(
            window.response.thread.turns.unwrap()[0].status,
            TurnStatus::Completed
        );
        assert_eq!(window.start, 0);
        assert_eq!(window.response.thread.name.as_deref(), Some("native"));
        assert_eq!(window.response.thread.status, SessionStatus::Unknown);
    }

    #[test]
    fn failed_cached_writes_roll_back_and_recover_without_gaps_after_reopening() {
        let (root, store) = store();
        let target = conversation();
        store.import(&target, history(vec![])).unwrap();
        assert_eq!(store.window(&target, None, 10).unwrap().unwrap().start, 0);
        store
            .record(&target, &turn("first", &["a"], TurnStatus::Completed), None)
            .unwrap();
        let version = store
            .window(&target, None, 10)
            .unwrap()
            .unwrap()
            .response
            .thread
            .transcript_version;
        store
            .with(|connection| {
                connection
                    .execute_batch(
                        "CREATE TRIGGER reject_turn BEFORE INSERT ON runs
                         WHEN NEW.native_turn_id = 'rejected' OR NEW.status = 'running'
                         BEGIN SELECT RAISE(ABORT, 'rejected by test'); END;",
                    )
                    .map_err(failure)
            })
            .unwrap();
        assert!(
            store
                .record(
                    &target,
                    &turn("rejected", &["b"], TurnStatus::Completed),
                    None
                )
                .is_err()
        );
        assert!(
            store
                .record(&target, &turn("first", &["a"], TurnStatus::Running), None)
                .is_err()
        );
        assert!(
            store
                .update_turn(&target, &"first".into(), None, |mut turn| {
                    turn.status = TurnStatus::Running;
                    Some(turn)
                })
                .is_err()
        );
        let unchanged = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&unchanged), ["first/a"]);
        assert_eq!(
            unchanged.response.thread.turns.as_ref().unwrap()[0].status,
            TurnStatus::Completed
        );
        assert_eq!(unchanged.response.thread.transcript_version, version);
        store
            .record(
                &target,
                &turn("second", &["b"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        drop(store);
        let store = Database::open(&root.path().join("state/transcripts.sqlite"));
        let latest = store.window(&target, None, 1).unwrap().unwrap();
        assert_eq!(latest.start, 1);
        assert_eq!(texts(&latest), ["second/b"]);
        // Replacing an earlier occurrence must not move the next append position.
        store
            .record(&target, &turn("first", &["a"], TurnStatus::Failed), None)
            .unwrap();
        store
            .record(&target, &turn("third", &["c"], TurnStatus::Completed), None)
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.start, 0);
        assert_eq!(texts(&window), ["first/a", "second/b", "third/c"]);
        store
            .with(|connection| {
                let counted: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM runs WHERE thread_id = ?1",
                        [key(&target)],
                        |row| row.get(0),
                    )
                    .map_err(failure)?;
                assert_eq!(
                    connection
                        .query_row(
                            "SELECT coalesce(max(ordinal) + 1, 0) FROM runs WHERE thread_id = ?1",
                            [key(&target)],
                            |row| row.get::<_, i64>(0)
                        )
                        .map_err(failure)?,
                    counted
                );
                Ok::<(), String>(())
            })
            .unwrap();
    }

    #[test]
    fn windows_page_backwards_and_stop_at_the_byte_budget() {
        let (_root, store) = store();
        let target = conversation();
        let turns = (0..12)
            .map(|index| turn(&index.to_string(), &["x"], TurnStatus::Completed))
            .collect();
        store.import(&target, history(turns)).unwrap();
        let newest = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(
            (
                newest.start,
                newest.response.thread.turns.as_ref().unwrap().len()
            ),
            (2, 10)
        );
        let older = store
            .window(&target, Some(newest.start), 10)
            .unwrap()
            .unwrap();
        assert_eq!(texts(&older), ["0/x", "1/x"]);
        assert!(store.window(&target, Some(13), 10).is_err());

        let large = |id: &str| {
            let mut turn = turn(id, &["x"], TurnStatus::Completed);
            Arc::make_mut(&mut turn.items.as_mut().unwrap()[0]).body =
                agent_protocol::items::ItemContent::Inline {
                    body: Box::new(ItemBody::AssistantText {
                        text: "x".repeat(WINDOW_BYTES / 2 + 1),
                        phase: AssistantPhase::Final,
                    }),
                };
            turn
        };
        let target = SessionRef::new(ProviderKind::Codex, "large".into()).unwrap();
        store
            .import(&target, history(vec![large("a"), large("b")]))
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(
            window.start, 1,
            "a window keeps one turn past the budget, never two"
        );
    }

    #[test]
    fn recording_replaces_the_same_occurrence_and_keeps_known_items() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                history(vec![turn("repeated", &["old"], TurnStatus::Completed)]),
            )
            .unwrap();
        // A reused turn ID with new items is a new occurrence.
        store
            .record(
                &target,
                &turn("repeated", &["new"], TurnStatus::Running),
                None,
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("repeated", &["new", "more"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let mut bare = turn("repeated", &[], TurnStatus::Failed);
        bare.items = None;
        store.record(&target, &bare, None).unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        let turns = window.response.thread.turns.as_ref().unwrap();
        assert_eq!(
            texts(&window),
            ["repeated/old", "repeated/new", "repeated/more"]
        );
        assert_eq!(turns[1].status, TurnStatus::Failed);
        assert_eq!(
            store
                .turn(&target, &"repeated".into())
                .unwrap()
                .unwrap()
                .status,
            TurnStatus::Failed
        );
        store
            .record(
                &target,
                &turn("repeated", &["old", "late"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.response.thread.turns.as_ref().unwrap().len(), 2);
        assert_eq!(
            texts(&window),
            [
                "repeated/old",
                "repeated/late",
                "repeated/new",
                "repeated/more"
            ]
        );
        assert_eq!(
            window.response.thread.turns.as_ref().unwrap()[1].status,
            TurnStatus::Failed
        );
    }

    #[test]
    fn known_items_identify_older_occurrences_before_empty_later_turns() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                history(vec![
                    turn("repeated", &["old"], TurnStatus::Completed),
                    turn("repeated", &[], TurnStatus::Completed),
                ]),
            )
            .unwrap();
        store
            .record(
                &target,
                &turn("repeated", &["old", "late"], TurnStatus::Completed),
                None,
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["repeated/old", "repeated/late"]);
        let turns = window.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].items.as_ref().unwrap().len(), 2);
        assert!(turns[1].items.as_ref().unwrap().is_empty());
    }

    #[test]
    fn removing_an_unrecorded_occurrence_does_not_replace_an_older_one() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                history(vec![turn("repeat", &["old"], TurnStatus::Completed)]),
            )
            .unwrap();
        // A completion can fail to persist before its last item is removed.
        store
            .record(
                &target,
                &turn("repeat", &[], TurnStatus::Completed),
                Some(&"unrecorded".into()),
            )
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["repeat/old"]);
        let turns = window.response.thread.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].items.as_ref().unwrap()[0].id.as_str(), "old");
        assert!(turns[1].items.as_ref().unwrap().is_empty());
    }

    #[test]
    fn restarts_interrupt_running_turns_and_renames_follow_imports() {
        let (_root, store) = store();
        let target = conversation();
        store
            .rename(&target, Some("ignored before import"))
            .unwrap();
        store
            .import(
                &target,
                history(vec![turn("running", &["partial"], TurnStatus::Running)]),
            )
            .unwrap();
        store.rename(&target, Some("renamed")).unwrap();
        store.interrupt_running().unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(window.response.thread.name.as_deref(), Some("renamed"));
        assert_eq!(
            window.response.thread.turns.as_ref().unwrap()[0].status,
            TurnStatus::Interrupted
        );
    }

    #[test]
    fn late_updates_change_the_stored_turn_and_its_version() {
        let (_root, store) = store();
        let target = conversation();
        store
            .import(
                &target,
                history(vec![turn("done", &["answer"], TurnStatus::Completed)]),
            )
            .unwrap();
        let before = store
            .window(&target, None, 10)
            .unwrap()
            .unwrap()
            .response
            .thread
            .transcript_version;
        store
            .update_turn(&target, &"done".into(), None, |mut turn| {
                turn.items.as_mut()?.push(Arc::new(Item::new(
                    "late".into(),
                    Default::default(),
                    ItemBody::AssistantText {
                        text: "done/late".into(),
                        phase: AssistantPhase::Final,
                    },
                )));
                Some(turn)
            })
            .unwrap();
        store
            .update_turn(&target, &"missing".into(), None, |_| unreachable!())
            .unwrap();
        let window = store.window(&target, None, 10).unwrap().unwrap();
        assert_eq!(texts(&window), ["done/answer", "done/late"]);
        assert!(window.response.thread.transcript_version > before);
    }

    #[test]
    fn every_change_gives_its_conversation_a_newer_version() {
        let (_root, store) = store();
        let target = conversation();
        let other = SessionRef::new(ProviderKind::Codex, "other".into()).unwrap();
        let version = |target: &SessionRef| {
            store
                .window(target, None, 10)
                .unwrap()
                .unwrap()
                .response
                .thread
                .transcript_version
                .unwrap()
        };
        store.import(&target, history(vec![])).unwrap();
        store.import(&other, history(vec![])).unwrap();
        let imported = version(&target);
        assert!(version(&other) > imported);
        store
            .record(&target, &turn("t", &["i"], TurnStatus::Running), None)
            .unwrap();
        let recorded = version(&target);
        assert!(recorded > version(&other));
        store.rename(&target, Some("renamed")).unwrap();
        let renamed = version(&target);
        assert!(renamed > recorded);
        store.interrupt_running().unwrap();
        assert!(version(&target) > renamed);
        assert_eq!(version(&target), version(&target), "reads keep the version");
    }

    #[test]
    fn complete_history_requires_an_import_and_is_not_limited_to_a_display_window() {
        let root = tempfile::tempdir().unwrap();
        let store = Database::open(&root.path().join("transcripts.sqlite"));
        let target = conversation();
        let mut first = turn("first", &["large"], TurnStatus::Completed);
        Arc::make_mut(&mut first.items.as_mut().unwrap()[0]).body =
            agent_protocol::items::ItemContent::Inline {
                body: Box::new(ItemBody::AssistantText {
                    text: "x".repeat(WINDOW_BYTES),
                    phase: AssistantPhase::Final,
                }),
            };
        store.record(&target, &first, None).unwrap();
        assert!(store.history(&target).unwrap().is_none());
        let second = turn("second", &["last"], TurnStatus::Completed);
        store.import(&target, history(vec![first, second])).unwrap();
        assert!(store.window(&target, None, 10).unwrap().unwrap().start > 0);
        let complete = store.history(&target).unwrap().unwrap();
        assert_eq!(
            complete
                .thread
                .turns
                .unwrap()
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }

    #[test]
    fn an_unopened_store_fails_instead_of_reporting_empty_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let store = Database::open(root.path());
        assert!(store.window(&conversation(), None, 10).is_err());
        assert!(
            store
                .record(
                    &conversation(),
                    &turn("t", &["i"], TurnStatus::Completed),
                    None
                )
                .is_err()
        );
    }
}
