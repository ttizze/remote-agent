use super::test_support::{NOW, thread};
use super::*;
use agent_protocol::orchestration as model;

#[test]
fn unsupported_destinations_are_rejected_without_rewriting_their_bytes() {
    for version in [0, 2, 3, 4, 5, 6] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE future(data TEXT); INSERT INTO future VALUES ('preserve');",
            )
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        let storage = Storage::open(&path);
        assert!(storage.check_ready().is_err());
        drop(storage);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn storage_uses_durable_wal_and_reports_an_invalid_path_to_every_consumer() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    storage
        .with(|connection| {
            let mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            let synchronous: i64 = connection
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            assert_eq!(mode, "wal");
            assert_eq!(synchronous, 2);
            Ok(())
        })
        .unwrap();
    let invalid = Storage::open(directory.path());
    assert!(invalid.check_ready().is_err());
    assert!(invalid.shell_snapshot().is_err());
    assert!(invalid.thread_state("invalid", &[]).is_err());
    assert!(invalid.projects().is_err());
}

#[test]
fn reopening_preserves_canonical_receipts_and_never_creates_legacy_tables() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let storage = Storage::open(&path);
    let command = || commands::Command {
        id: "create",
        thread_id: "thread",
        kind: "thread.create",
        now: NOW,
        item_ids: &[],
    };
    let receipt = storage
        .thread_command(command(), Some(&thread("thread")), |_| {
            Ok(model::Decision::default())
        })
        .unwrap();
    let before = storage.shell_snapshot().unwrap();
    drop(storage);
    let storage = Storage::open(&path);
    assert_eq!(storage.shell_snapshot().unwrap(), before);
    assert_eq!(
        storage
            .thread_command(command(), None, |_| panic!(
                "a saved command must not execute again"
            ))
            .unwrap()
            .receipt,
        receipt.receipt
    );
    storage
        .with(|db| {
            let mut query = db
                .prepare("SELECT name FROM sqlite_schema WHERE type='table'")
                .unwrap();
            let names = query
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            for name in [
                "threads",
                "items",
                "inputs",
                "turns",
                "requests",
                "bindings",
                "attempts",
                "jobs",
                "events",
                "rows",
                "catalogs",
                "provider_catalogs",
            ] {
                assert!(
                    !names.iter().any(|saved| saved == name),
                    "obsolete table: {name}"
                );
            }
            assert!(names.iter().any(|name| name == "orchestration_events"));
            Ok(())
        })
        .unwrap();
}

#[test]
fn only_successful_committed_writes_wake_streams() {
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let mut changed = storage.changes();
    storage.shell_snapshot().unwrap();
    assert!(!changed.has_changed().unwrap());
    storage
        .thread_command(
            commands::Command {
                id: "create",
                thread_id: "thread",
                kind: "thread.create",
                now: NOW,
                item_ids: &[],
            },
            Some(&thread("thread")),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
    assert!(changed.has_changed().unwrap());
    changed.borrow_and_update();
    let before = storage.shell_snapshot().unwrap();
    let failed = storage.observe_thread("thread", &[], NOW, |state| {
        Ok(model::Decision {
            changes: vec![model::Change::Thread(model::Thread {
                title: "must roll back".into(),
                active_provider_thread_id: Some("missing".into()),
                ..state.thread.clone()
            })],
            effects: vec![],
        })
    });
    assert!(failed.is_err());
    assert!(!changed.has_changed().unwrap());
    assert_eq!(storage.shell_snapshot().unwrap(), before);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn committed_database_streams_converge_through_reopen_overlap_and_snapshot_fallback(
        actions in proptest::collection::vec((proptest::bool::ANY, "[a-zあ界é]{1,24}", 0u8..4), 1..24)
    ) {
        use agent_protocol::orchestration::projection::{ThreadSnapshot, ThreadStreamItem};
        fn catch_up(storage: &Storage, id: &SessionRef, cached: &mut Option<ThreadSnapshot>) {
            let after=cached.as_ref().map(|snapshot|snapshot.snapshot_sequence);
            for frame in storage.thread_stream(id,after).unwrap() {
                *cached=agent_core::orchestration::receive(&id.application_id(),cached.as_ref(),frame.clone()).unwrap();
                let once=cached.clone();
                *cached=agent_core::orchestration::receive(&id.application_id(),cached.as_ref(),frame).unwrap();
                assert_eq!(*cached,once,"overlapping delivery is idempotent");
            }
        }
        let directory=tempfile::tempdir().unwrap();
        let path=directory.path().join("state.sqlite");
        let mut storage=Storage::open(&path);
        for id in ["thread","foreign"] {
            storage.thread_command(commands::Command {id,thread_id:id,kind:"thread.create",now:NOW,item_ids:&[]},Some(&thread(id)),|_|Ok(model::Decision::default())).unwrap();
        }
        let id=SessionRef::new(None,"thread".into()).unwrap();
        let mut cached=None;
        catch_up(&storage,&id,&mut cached);
        let mut expected="Example".to_string();
        for (foreign,title,mode) in actions {
            let target=if foreign {"foreign"} else {"thread"};
            storage.observe_thread(target,&[],NOW,|state| {
                let mut thread=state.thread.clone();thread.title=title.clone();
                Ok(model::Decision {changes:vec![model::Change::Thread(thread)],effects:vec![]})
            }).unwrap();
            if !foreign {expected=title;}
            if mode==1 {
                drop(storage);
                storage=Storage::open(&path);
            } else if mode==2 {
                storage.with(|db| {db.execute("INSERT INTO orchestration_v2_projection_metadata VALUES ('event-log-pruned-through',2,(SELECT max(sequence) FROM orchestration_events),?1) ON CONFLICT(projection_name) DO UPDATE SET last_sequence=excluded.last_sequence",[NOW]).map_err(failure)?;Ok(())}).unwrap();
            }
            if mode!=3 {catch_up(&storage,&id,&mut cached);}
        }
        catch_up(&storage,&id,&mut cached);
        let current=storage.thread_stream(&id,None).unwrap().into_iter().find_map(|frame|match frame {ThreadStreamItem::Snapshot(value)=>Some(value),_=>None}).unwrap();
        proptest::prop_assert_eq!(&cached.as_ref().unwrap().projection.thread.title,&expected);
        proptest::prop_assert_eq!(cached.as_ref().unwrap(),&current);
    }
}

#[tokio::test]
async fn search_finds_an_old_saved_message_beyond_twenty_thousand_streaming_rows_after_reopen() {
    // T3 defaults to the finished-message trigram index. The 20k scan is only
    // its fallback; newer unfinished rows cannot hide an old indexed message.
    // Use the real projection writer, SQL boundary and public Host RPC. The
    // native producer boundary is separately exercised by the service matrix.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(CONVERSATION_DATABASE_FILE);
    let old = uuid::Uuid::from_u128(4000).to_string();
    let noise = uuid::Uuid::from_u128(4001).to_string();
    let storage = Storage::open(&path);
    storage.with(|db| {
        let tx=db.transaction().map_err(failure)?;
        for id in [&old,&noise] {projections::save(&tx,Some(id),&thread(id))?;}
        for i in 0..20_002 {
            let target=if i==0 {&old} else {&noise};
            let item:model::TurnItem=serde_json::from_value(serde_json::json!({
                "id":format!("search-message-{i}"),"threadId":target,"ordinal":i,"status":if i==0 {"completed"} else {"running"},"updatedAt":NOW,
                "type":"assistant_message","messageId":format!("message-{i}"),"text":"needle","attachments":[],"streaming":i!=0
            })).map_err(failure)?;
            projections::save(&tx,Some(target),&item)?;
        }
        tx.commit().map_err(failure)
    }).unwrap();
    drop(storage);
    for _ in 0..2 {
        let host = crate::HostRpcService::new([], directory.path().join("projects.json"));
        let client = host.open_session();
        let reply = host
            .dispatch(
                client.id(),
                &agent_protocol::protocol::Call::SearchList(
                    agent_protocol::conversation::records::SearchList {
                        term: "needle".into(),
                        limit: Some(50),
                    },
                ),
            )
            .await
            .unwrap();
        let agent_protocol::protocol::Response::Success { result } =
            agent_protocol::protocol::decode::<
                agent_protocol::protocol::Response<agent_protocol::models::ThreadList>,
            >(&reply.initial)
            .unwrap()
        else {
            panic!("canonical search failed")
        };
        assert_eq!(
            result
                .data
                .iter()
                .map(|thread| thread.id.as_ref().unwrap().id.as_str())
                .collect::<Vec<_>>(),
            vec![old.as_str()],
            "querying the real saved index must find the old completed message and exclude newer streaming noise"
        );
    }
}

#[test]
fn large_tool_histories_page_settled_nodes_and_retain_live_ancestry() {
    use model::projection::ThreadStreamItem;
    let directory = tempfile::tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("state.sqlite"));
    let node_id = |index| format!("tool-{index}:{}", "native-reference".repeat(20));
    storage
        .with(|db| {
            let tx = db.transaction().map_err(failure)?;
            projections::save(&tx, Some("thread"), &thread("thread"))?;
            let root: model::ExecutionNode = serde_json::from_value(serde_json::json!({
                "id":"root","threadId":"thread","rootNodeId":"root","kind":"root_turn",
                "status":"running","countsForRun":false
            }))
            .map_err(failure)?;
            projections::save(&tx, Some("thread"), &root)?;
            for index in 0..4000 {
                let node = model::ExecutionNode {
                    id: node_id(index),
                    parent_node_id: Some("root".into()),
                    kind: model::NodeKind::ToolCall,
                    status: if index == 0 {
                        model::NodeStatus::Running
                    } else {
                        model::NodeStatus::Completed
                    },
                    ..root.clone()
                };
                projections::save(&tx, Some("thread"), &node)?;
                let item: model::TurnItem = serde_json::from_value(serde_json::json!({
                "id":format!("item-{index}"),"threadId":"thread","ordinal":index,"nodeId":node.id,
                "status":"completed","updatedAt":NOW,"type":"command_execution",
                "input":"inspect","output":"done","streaming":false
            })).map_err(failure)?;
                projections::save(&tx, Some("thread"), &item)?;
            }
            let mut live = projections::read::<model::ExecutionNode>(&tx, &node_id(0))?.unwrap();
            live.parent_node_id = Some(node_id(1));
            projections::save(&tx, Some("thread"), &live)?;
            tx.commit().map_err(failure)
        })
        .unwrap();
    let target = SessionRef::new(None, "thread".into()).unwrap();
    let snapshot = storage
        .thread_stream(&target, None)
        .unwrap()
        .into_iter()
        .find_map(|item| {
            if let ThreadStreamItem::Snapshot(snapshot) = item {
                Some(snapshot)
            } else {
                None
            }
        })
        .unwrap();
    assert!(
        serde_json::to_vec(&snapshot).unwrap().len() < 1_048_576,
        "the initial window keeps the existing one-MiB budget"
    );
    for id in ["root".to_owned(), node_id(0), node_id(1)] {
        assert!(snapshot.projection.nodes.iter().any(|node| node.id == id));
    }
    assert!(
        !snapshot
            .projection
            .nodes
            .iter()
            .any(|node| node.id == node_id(2))
    );
    assert!(storage.thread_item(&target, "item-2").unwrap().is_some());
    let page = storage
        .thread_history(&target, snapshot.history_cursor.as_ref().unwrap())
        .unwrap();
    assert!(!page.items.is_empty());
    for row in &page.items {
        let id = row.item.node_id.as_ref().unwrap();
        assert!(page.nodes.iter().any(|node| &node.id == id));
    }
    assert!(page.nodes.iter().any(|node| node.id == "root"));
}
