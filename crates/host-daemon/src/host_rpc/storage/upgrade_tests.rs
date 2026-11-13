use super::*;
use agent_protocol::{
    conversation::{InputState, UnknownState},
    models::{Item, ItemBody, Thread, ThreadResponse, Turn, TurnStatus},
    operations::{FollowUp, Input, InputQueue, Submission},
    session::{PreparationStage, PreparationStatus, SessionPreparation},
};

fn address() -> SessionRef {
    SessionRef::new(ProviderKind::Codex, "existing-native-identity".into()).unwrap()
}

#[test]
fn committed_previous_storage_ignores_a_stale_rewind_file() {
    let directory = tempfile::tempdir().unwrap();
    previous(&directory.path().join("bex-transcripts.sqlite"), false);
    std::fs::write(
        directory.path().join("bex-conversations.rewind"),
        b"stale invalid JSON",
    )
    .unwrap();
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    assert_eq!(
        storage
            .history(&address(), 0)
            .unwrap()
            .thread
            .turns
            .unwrap()[0]
            .id
            .as_str(),
        "original-turn"
    );
}

#[test]
fn json_only_rewind_restores_qualified_addresses_without_changing_public_ids() {
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let root = SessionRef::new(ProviderKind::Claude, "bex-task:kept".into()).unwrap();
    let first = SessionRef::new(ProviderKind::Claude, "first-native".into()).unwrap();
    let second = SessionRef::new(ProviderKind::Codex, "second-native".into()).unwrap();
    let qualified = |id: &str| {
        format!(
            "bex:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&(&second, id)).unwrap())
        )
    };
    let turn = qualified("native-turn");
    let item = qualified("native-item");
    let user = qualified("native-user");
    let input = Uuid::new_v4();
    let row = serde_json::json!({"id":root,"active":second,"cwd":"/workspace","name":null,"preview":null,
        "bindings":[{"session":first,"covered":1},{"session":second,"covered":2}],"handoffs":[],"startup":null});
    let mut deferred = Item::new(
        item.clone().into(),
        agent_protocol::models::ItemStatus::Completed,
        ItemBody::AssistantText {
            text: "deferred answer".into(),
            phase: Default::default(),
        },
    );
    deferred.defer();
    let mut sent = Item::new(
        user.clone().into(),
        agent_protocol::models::ItemStatus::Completed,
        ItemBody::UserMessage {
            text: Some("kept user input".into()),
            content: vec![],
        },
    );
    sent.client_input_id = Some(input.to_string().into());
    let response = ThreadResponse {
        thread: Thread {
            id: Some(root.clone()),
            active_provider: Some(ProviderKind::Codex),
            cwd: Some("/workspace".into()),
            turns: Some(vec![
                Arc::new(Turn {
                    id: "first-turn".into(),
                    provider: Some(ProviderKind::Claude),
                    status: TurnStatus::Completed,
                    items: Some(vec![]),
                    ..Default::default()
                }),
                Arc::new(Turn {
                    id: turn.clone().into(),
                    provider: Some(ProviderKind::Codex),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(sent), Arc::new(deferred)]),
                    ..Default::default()
                }),
            ]),
            ..Default::default()
        },
        ..Default::default()
    };
    std::fs::write(
        directory.path().join("bex-conversations.json"),
        serde_json::to_vec(&vec![row.clone()]).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("bex-conversations.rewind"),
        serde_json::to_vec(&serde_json::json!({"previous":row,"history":response})).unwrap(),
    )
    .unwrap();
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    let address = storage
        .native_body(&root, &item.clone().into())
        .unwrap()
        .unwrap();
    assert_eq!(address.request.thread_id, second);
    assert_eq!(address.request.turn_id.as_str(), "native-turn");
    assert_eq!(address.request.item_id.as_str(), "native-item");
    assert_eq!(
        storage.state(&root).unwrap().bindings[&ProviderKind::Claude],
        decision::Binding::Ready {
            source: first.id.clone(),
            covered: 1,
        },
        "the inactive native source keeps its actual covered prefix for later incremental handoff"
    );
    let fork = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    storage
        .fork(&root, &fork, &turn.clone().into(), 200)
        .unwrap();
    assert_eq!(
        storage.state(&fork).unwrap().bindings[&ProviderKind::Codex],
        decision::Binding::Fork {
            source: second.id.clone(),
            through: "native-turn".into()
        }
    );
    let restored = storage.history(&root, 0).unwrap();
    let restored = &restored.thread.turns.as_ref().unwrap()[1];
    assert_eq!(restored.id.as_str(), turn);
    assert_eq!(restored.items.as_ref().unwrap()[0].id.as_str(), user);
    assert_eq!(
        restored.items.as_ref().unwrap()[0]
            .client_input_id
            .as_ref()
            .unwrap()
            .as_str(),
        input.to_string()
    );
    assert_eq!(
        storage.receipts(&root, &[input]).unwrap()[0].1.outcome,
        Outcome::Accepted
    );
    let mut imported = response.clone();
    imported.thread.id = Some(second.clone());
    let mut native_turn = imported.thread.turns.as_ref().unwrap()[1].as_ref().clone();
    native_turn.id = "native-turn".into();
    for (item, id) in native_turn
        .items
        .as_mut()
        .unwrap()
        .iter_mut()
        .zip(["native-user", "native-item"])
    {
        Arc::make_mut(item).id = id.into();
    }
    let echoed = Arc::make_mut(&mut native_turn.items.as_mut().unwrap()[0]);
    echoed.client_input_id = None;
    echoed.body = agent_protocol::items::ItemContent::Inline {
        body: Box::new(ItemBody::UserMessage {
            text: Some("Provider context handoff. private context packet".into()),
            content: vec![],
        }),
    };
    imported.thread.turns = Some(vec![Arc::new(native_turn)]);
    storage.queue_import(&root, &second, 300).unwrap();
    let claim = storage
        .claim(300, 10)
        .unwrap()
        .into_iter()
        .find(|claim| matches!(&claim.work,jobs::Work::Import {target,..} if target==&root))
        .unwrap();
    storage
        .reimport(&root, &second, claim.id, claim.attempt, &imported, 301)
        .unwrap();
    let reimported = storage.history(&root, 0).unwrap();
    let turns = reimported.thread.turns.as_ref().unwrap();
    assert_eq!(
        turns.len(),
        2,
        "re-import must match original qualified native turn addresses"
    );
    assert_eq!(turns[1].id.as_str(), turn);
    assert_eq!(turns[1].items.as_ref().unwrap().len(), 2);
    let kept = &turns[1].items.as_ref().unwrap()[0];
    assert_eq!(kept.id.as_str(), user);
    assert_eq!(
        kept.client_input_id.as_ref().unwrap().as_str(),
        input.to_string()
    );
    assert!(
        matches!(kept.body(), ItemBody::UserMessage { text: Some(text), .. } if text == "kept user input")
    );
}
fn input(id: Uuid) -> Submission {
    Submission {
        thread_id: address(),
        client_user_message_id: id.to_string().into(),
        input: vec![Input::Text {
            text: "preserved input".into(),
        }],
        model: None,
        effort: Some("high".into()),
        service_tier: Some("priority".into()),
        follow_up: FollowUp::Queue,
    }
}

fn previous(path: &Path, corrupt: bool) -> (Uuid, Uuid) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch("CREATE TABLE conversations(provider TEXT,id TEXT,cwd TEXT,metadata BLOB); CREATE TABLE turns(provider TEXT,id TEXT,position INTEGER,body BLOB); CREATE TABLE host_state(name TEXT PRIMARY KEY,body BLOB); CREATE TABLE queues(provider TEXT,id TEXT,body BLOB); PRAGMA user_version=1;").unwrap();
    let response = ThreadResponse {
        thread: Thread {
            id: Some(address()),
            cwd: Some("/kept/workspace".into()),
            name: Some("Saved conversation".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    connection
        .execute(
            "INSERT INTO conversations VALUES ('codex',?1,?2,?3)",
            params![
                address().id,
                response.thread.cwd,
                protocol::encode(&response).unwrap()
            ],
        )
        .unwrap();
    let turn = Turn {
        id: "original-turn".into(),
        status: TurnStatus::Completed,
        items: Some(vec![Arc::new(Item::new(
            "original-item".into(),
            agent_protocol::execution::ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "kept answer".into(),
                phase: Default::default(),
            },
        ))]),
        ..Default::default()
    };
    let body = if corrupt {
        vec![0xc1]
    } else {
        protocol::encode(turn).unwrap()
    };
    connection
        .execute(
            "INSERT INTO turns VALUES ('codex',?1,0,?2)",
            params![address().id, body],
        )
        .unwrap();
    let sending = Uuid::new_v4();
    let queued = Uuid::new_v4();
    let preparation = SessionPreparation {
        submission: input(sending),
        stage: PreparationStage::Provider,
        status: PreparationStatus::Sending,
    };
    let binding = serde_json::json!([{"id":address(),"active":address(),"cwd":"/kept/workspace","name":"Saved conversation","preview":"kept preview","bindings":[{"session":address()}],"handoffs":[],"startup":{"preparation":preparation}}]);
    let reads = serde_json::json!([{"session":address(),"turn_id":"original-turn","unread":false}]);
    let models = serde_json::json!([{"session":address(),"model":null,"effort":"high","service_tier":"priority"}]);
    for (name, value) in [("bindings", binding), ("reads", reads), ("models", models)] {
        connection
            .execute(
                "INSERT INTO host_state VALUES (?1,?2)",
                params![name, serde_json::to_vec(&value).unwrap()],
            )
            .unwrap();
    }
    connection
        .execute(
            "INSERT INTO queues VALUES ('codex',?1,?2)",
            params![
                address().id,
                protocol::encode(InputQueue {
                    inputs: vec![input(queued)],
                    held: false
                })
                .unwrap()
            ],
        )
        .unwrap();
    (sending, queued)
}

#[test]
fn first_open_backs_up_and_converts_identity_history_options_and_uncertain_input_once() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("bex-transcripts.sqlite");
    let destination = directory.path().join("bex-conversations.sqlite");
    let (sending, queued) = previous(&old, false);
    let before = std::fs::read(&old).unwrap();
    let storage = Storage::open(&destination);
    storage.check_ready().unwrap();
    assert_eq!(storage.identities().unwrap(), [address()]);
    let state = storage.state(&address()).unwrap();
    assert!(matches!(
        &state.inputs[sending.to_string().as_str()].state,
        InputState::Unknown {
            status: UnknownState::Fenced,
            ..
        }
    ));
    assert!(matches!(
        &state.inputs[queued.to_string().as_str()].state,
        InputState::Queued { .. }
    ));
    assert!(state.thread.queue_held);
    let header = storage.header(&address()).unwrap();
    assert_eq!(
        header.details.read_through.as_ref().unwrap().as_str(),
        "original-turn"
    );
    assert_eq!(header.details.selection.effort.as_deref(), Some("high"));
    let history = storage.history(&address(), 0).unwrap();
    let turns = history.thread.turns.unwrap();
    assert_eq!(turns[0].id.as_str(), "original-turn");
    assert_eq!(
        turns[0].items.as_ref().unwrap()[0].id.as_str(),
        "original-item"
    );
    assert_eq!(std::fs::read(&old).unwrap(), before);
    let backups: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("before-conversation-rows-")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path().join("bex-transcripts.sqlite").exists());
    drop(storage);
    let reopened = Storage::open(&destination);
    assert_eq!(reopened.state(&address()).unwrap().counters, state.counters);
    assert_eq!(
        std::fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("before-conversation-rows-"))
            .count(),
        1
    );
}

#[test]
fn a_corrupt_previous_transcript_fails_without_creating_an_empty_successful_destination() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("bex-transcripts.sqlite");
    let destination = directory.path().join("bex-conversations.sqlite");
    previous(&old, true);
    assert!(Storage::open(&destination).check_ready().is_err());
    let connection = Connection::open(&destination).unwrap();
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    let count: u32 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    drop(connection);
    let repaired = protocol::encode(&Turn {
        id: "original-turn".into(),
        status: TurnStatus::Completed,
        ..Default::default()
    })
    .unwrap();
    Connection::open(&old)
        .unwrap()
        .execute("UPDATE turns SET body=?1", [repaired])
        .unwrap();
    Storage::open(&destination).check_ready().unwrap();
}

#[test]
fn first_open_preserves_project_ids_roots_and_original_bytes_once() {
    let directory = tempfile::tempdir().unwrap();
    let legacy = directory.path().join("bex-projects.json");
    let destination = directory.path().join("bex-conversations.sqlite");
    let root = directory.path().join("project");
    std::fs::create_dir(&root).unwrap();
    let project = agent_protocol::models::Project {
        id: "existing-project-id".into(),
        name: "Existing project".into(),
        roots: vec![agent_protocol::models::ProjectRoot {
            path: root.to_string_lossy().into_owned(),
        }],
        ..Default::default()
    };
    let original = serde_json::to_vec(&vec![project.clone()]).unwrap();
    std::fs::write(&legacy, &original).unwrap();
    let storage = Storage::open(&destination);
    assert_eq!(
        storage.projects().unwrap().as_slice(),
        std::slice::from_ref(&project)
    );
    let backups = || {
        std::fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("before-conversation-rows-")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(backups().len(), 1);
    assert_eq!(
        std::fs::read(backups()[0].path().join("bex-projects.json")).unwrap(),
        original
    );
    assert_eq!(std::fs::read(&legacy).unwrap(), original);
    drop(storage);
    // An obsolete file no longer controls the project registry after upgrade.
    std::fs::write(&legacy, b"[]").unwrap();
    let reopened = Storage::open(&destination);
    assert_eq!(reopened.projects().unwrap(), [project]);
    assert_eq!(backups().len(), 1);
}

#[test]
fn json_only_upgrade_resolves_native_parent_links_after_restoring_all_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let parent = SessionRef::new(ProviderKind::Claude, "bex-task:parent".into()).unwrap();
    let native_parent = SessionRef::new(ProviderKind::Claude, "native-parent".into()).unwrap();
    let child = SessionRef::new(ProviderKind::Claude, "bex-task:child".into()).unwrap();
    let native_child = SessionRef::new(ProviderKind::Claude, "native-child".into()).unwrap();
    let row = |id: &SessionRef, native: &SessionRef| {
        serde_json::json!({
            "id":id,"active":native,"cwd":"","name":null,"preview":null,
            "bindings":[{"session":native,"covered":0}],"handoffs":[],"startup":null
        })
    };
    std::fs::write(
        directory.path().join("bex-conversations.json"),
        serde_json::to_vec(&vec![
            row(&child, &native_child),
            row(&parent, &native_parent),
        ])
        .unwrap(),
    )
    .unwrap();
    std::fs::write(directory.path().join("bex-conversations.rewind"),serde_json::to_vec(&serde_json::json!({
        "previous":row(&child,&native_child),"history":ThreadResponse {thread:Thread {id:Some(child.clone()),parent_id:Some(native_parent),turns:Some(vec![]),..Default::default()},..Default::default()}
    })).unwrap()).unwrap();
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    assert_eq!(storage.header(&child).unwrap().details.parent, Some(parent));
    assert_eq!(
        storage.header(&child).unwrap().state.workspace,
        decision::Workspace::Missing
    );
}

#[test]
fn previous_workspace_scripts_and_interrupted_setup_are_retained_together() {
    use agent_protocol::workspace::{SetupPhase, SetupScript, WorkspaceSetup, WorkspaceStrategy};
    for phase in [SetupPhase::Running, SetupPhase::Done, SetupPhase::Failed] {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("bex-transcripts.sqlite");
        previous(&old, false);
        let connection = Connection::open(&old).unwrap();
        let body: Vec<u8> = connection
            .query_row(
                "SELECT body FROM host_state WHERE name='bindings'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut rows: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let strategy = WorkspaceStrategy::Worktree {
            base_ref: "origin/main".into(),
            start_from_origin: true,
        };
        let mut setup = WorkspaceSetup::new(Some("origin/main".into()), 10);
        setup.script = Some(SetupScript {
            name: "Setup".into(),
            command: "echo preparing".into(),
            asynchronous: false,
        });
        setup.worktree_path = Some("/kept/workspace".into());
        setup.branch = Some("bex/session-kept".into());
        if phase != SetupPhase::Running {
            setup.finish(phase, None, 20);
        }
        rows[0]["startup"]["project_cwd"] = serde_json::json!("/original");
        rows[0]["startup"]["workspace"] = serde_json::json!(strategy);
        rows[0]["workspace_setup"] = serde_json::json!(setup);
        connection
            .execute(
                "UPDATE host_state SET body=?1 WHERE name='bindings'",
                [serde_json::to_vec(&rows).unwrap()],
            )
            .unwrap();
        let scripts = vec![agent_protocol::models::ProjectScript {
            name: "Build".into(),
            command: "echo build".into(),
        }];
        let key = "project-scripts:/original/.git";
        connection
            .execute(
                "INSERT INTO host_state VALUES (?1,?2)",
                params![key, serde_json::to_vec(&scripts).unwrap()],
            )
            .unwrap();
        drop(connection);
        let path = directory.path().join("bex-conversations.sqlite");
        let storage = Storage::open(&path);
        storage.check_ready().unwrap();
        assert_eq!(storage.workspace_scripts(key).unwrap(), scripts);
        let workspace = storage
            .header(&address())
            .unwrap()
            .details
            .workspace
            .unwrap();
        assert_eq!(workspace.project_cwd, "/original");
        assert_eq!(workspace.strategy, Some(strategy));
        let recovered = workspace.setup.as_ref().unwrap();
        assert_eq!(
            recovered.phase,
            if phase == SetupPhase::Running {
                SetupPhase::Cancelled
            } else {
                phase
            }
        );
        assert_eq!(workspace.visible, phase == SetupPhase::Running);
        assert_eq!(recovered.script, setup.script);
        assert_eq!(recovered.worktree_path, setup.worktree_path);
        assert_eq!(recovered.branch, setup.branch);
        drop(storage);
        let storage = Storage::open(&path);
        assert_eq!(storage.workspace_scripts(key).unwrap(), scripts);
        assert_eq!(
            storage.header(&address()).unwrap().details.workspace,
            Some(workspace)
        );
    }
}

#[test]
fn unsupported_destinations_are_rejected_without_rewriting_their_bytes() {
    for version in [0, 3] {
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
        assert!(storage.identity().is_err());
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
    assert!(invalid.identity().is_err());
    assert!(invalid.headers(true).is_err());
    assert!(invalid.list(None).is_err());
    assert!(invalid.state(&address()).is_err());
}

#[test]
fn a_future_previous_store_is_rejected_without_importing_or_rewriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("bex-transcripts.sqlite");
    previous(&source, false);
    let connection = Connection::open(&source).unwrap();
    connection.pragma_update(None, "user_version", 2).unwrap();
    drop(connection);
    let before = std::fs::read(&source).unwrap();
    let destination = directory.path().join("bex-conversations.sqlite");
    let storage = Storage::open(&destination);
    assert!(storage.check_ready().is_err());
    drop(storage);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let connection = Connection::open(&destination).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn recorded_native_fragments_without_metadata_are_imported_after_upgrade() {
    for managed in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("bex-transcripts.sqlite");
        let (_, queued) = previous(&old, false);
        let connection = Connection::open(&old).unwrap();
        connection
            .execute("UPDATE conversations SET metadata=NULL", [])
            .unwrap();
        if managed {
            connection.execute("UPDATE host_state SET body=?1 WHERE name='bindings'", [serde_json::to_vec(&serde_json::json!([{"id":address(),"active":address(),"cwd":"/kept/workspace","name":"Saved conversation","preview":null,"bindings":[{"session":address()}],"handoffs":[],"startup":null}])).unwrap()]).unwrap();
        } else {
            connection
                .execute("DELETE FROM host_state WHERE name='bindings'", [])
                .unwrap();
        }
        drop(connection);
        let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
        storage.check_ready().unwrap();
        assert!(storage.loaded(&address()).unwrap());
        let before = storage.state(&address()).unwrap();
        assert!(before.thread.queue_held);
        assert!(before.inputs.contains_key(queued.to_string().as_str()));
        let turn = |id: &str, text: &str| {
            Arc::new(Turn {
                id: id.into(),
                status: TurnStatus::Completed,
                items: Some(vec![Arc::new(Item::new(
                    format!("{id}-item").into(),
                    agent_protocol::execution::ItemStatus::Completed,
                    ItemBody::AssistantText {
                        text: text.into(),
                        phase: Default::default(),
                    },
                ))]),
                ..Default::default()
            })
        };
        let response = ThreadResponse {
            thread: Thread {
                id: Some(address()),
                name: Some("Recovered conversation".into()),
                cwd: Some("/kept/workspace".into()),
                turns: Some(vec![
                    turn("older-turn", "older answer"),
                    Arc::new(Turn {
                        id: "original-turn".into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![Arc::new(Item::new(
                            "original-item".into(),
                            agent_protocol::execution::ItemStatus::Completed,
                            ItemBody::AssistantText {
                                text: "kept answer".into(),
                                phase: Default::default(),
                            },
                        ))]),
                        ..Default::default()
                    }),
                ]),
                ..Default::default()
            },
            ..Default::default()
        };
        let claim = storage.claim(i64::MAX / 2, 16).unwrap().into_iter().find(|claim| matches!(&claim.work, jobs::Work::Import { target, .. } if target == &address())).unwrap();
        let imported = storage
            .reimport(
                &address(),
                &address(),
                claim.id,
                claim.attempt,
                &response,
                100,
            )
            .unwrap();
        assert_eq!(imported.step.outcome, Outcome::Accepted);
        assert!(storage.loaded(&address()).unwrap());
        let after = storage.state(&address()).unwrap();
        assert!(after.thread.queue_held);
        assert_eq!(after.inputs, before.inputs);
        assert_eq!(after.counters.input, before.counters.input);
        let history = storage.history(&address(), 0).unwrap().thread;
        let turns = history.turns.unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(
            turns
                .iter()
                .filter(|turn| turn.id.as_str() == "original-turn")
                .count(),
            1
        );
        for expected in ["older answer", "kept answer"] {
            assert_eq!(turns.iter().flat_map(|turn| turn.items.iter().flatten()).filter(|item| matches!(item.body(), ItemBody::AssistantText { text, .. } if text == expected)).count(), 1);
        }
        let details = storage.header(&address()).unwrap().details;
        assert_eq!(
            details.read_through.as_ref().map(|id| id.as_str()),
            Some("original-turn")
        );
        assert_eq!(details.selection.effort.as_deref(), Some("high"));
        assert_eq!(details.selection.service_tier.as_deref(), Some("priority"));
        assert!(details.history_read_state.is_none());
        assert!(
            matches!(turns[0].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText { text, .. } if text == "older answer")
        );
        assert_eq!(turns[1].id.as_str(), "original-turn");
        if !managed {
            assert_eq!(history.name.as_deref(), Some("Recovered conversation"));
            assert_eq!(turns[0].id.as_str(), "older-turn");
        }
    }
}

#[test]
fn repeated_qualified_native_turns_keep_each_deferred_item_address() {
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let root = SessionRef::new(ProviderKind::Claude, "bex-task:repeated".into()).unwrap();
    let source = address();
    let qualified = |native: &str| {
        format!(
            "bex:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&(&source, native)).unwrap())
        )
    };
    let turn = qualified("repeated");
    let ids = [qualified("first"), qualified("second")];
    let response = ThreadResponse {
        thread: Thread {
            id: Some(root.clone()),
            turns: Some(
                ids.iter()
                    .map(|id| {
                        let mut item = Item::new(
                            id.clone().into(),
                            agent_protocol::execution::ItemStatus::Completed,
                            ItemBody::AssistantText {
                                text: "deferred".into(),
                                phase: Default::default(),
                            },
                        );
                        item.defer();
                        Arc::new(Turn {
                            id: turn.clone().into(),
                            status: TurnStatus::Completed,
                            items: Some(vec![Arc::new(item)]),
                            ..Default::default()
                        })
                    })
                    .collect(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    let row = serde_json::json!({"id":root,"active":source,"cwd":"/workspace","bindings":[{"session":source}],"handoffs":[]});
    std::fs::write(
        directory.path().join("bex-conversations.rewind"),
        serde_json::to_vec(&serde_json::json!({"previous":row,"history":response})).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("bex-read-state.json"),
        serde_json::to_vec(&serde_json::json!([
            {"session":root,"turn_id":turn,"unread":false}
        ]))
        .unwrap(),
    )
    .unwrap();
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    let latest = storage
        .history(&root, 0)
        .unwrap()
        .thread
        .turns
        .unwrap()
        .last()
        .unwrap()
        .id
        .clone();
    assert_eq!(
        storage.header(&root).unwrap().details.read_through,
        Some(latest)
    );
    storage
        .decide(
            &root,
            Event::Command {
                id: decision::OperationId::Client(Uuid::new_v4()),
                command: decision::Command::Rename("renamed".into()),
                receipt: None,
            },
            Context {
                preparation: None,
                rewind_binding: None,
                imported_turns: &[],
                external_busy: false,
                now: 500,
                seed: Uuid::new_v4(),
                steers: false,
                steer_ready: true,
            },
            None,
        )
        .unwrap();
    assert!(
        decision::records::list_activity([(&root, &storage.header(&root).unwrap())])
            .unread
            .is_empty()
    );
    for (id, native) in ids.into_iter().zip(["first", "second"]) {
        let address = storage.native_body(&root, &id.into()).unwrap().unwrap();
        assert_eq!(address.request.thread_id, source);
        assert_eq!(address.request.turn_id.as_str(), "repeated");
        assert_eq!(address.request.item_id.as_str(), native);
    }
}

#[test]
fn first_upgrade_without_a_recovery_journal_interrupts_the_old_running_turn() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("bex-transcripts.sqlite");
    previous(&old, false);
    let connection = Connection::open(&old).unwrap();
    let mut turn: Turn = protocol::decode(
        &connection
            .query_row("SELECT body FROM turns", [], |row| row.get::<_, Vec<u8>>(0))
            .unwrap(),
    )
    .unwrap();
    turn.status = TurnStatus::Running;
    connection
        .execute(
            "UPDATE turns SET body=?1",
            [protocol::encode(turn).unwrap()],
        )
        .unwrap();
    // No queued/preparing input is involved in this ordinary restart.
    connection.execute("DELETE FROM queues", []).unwrap();
    connection.execute("UPDATE host_state SET body=?1 WHERE name='bindings'", [serde_json::to_vec(&serde_json::json!([{"id":address(),"active":address(),"cwd":"/kept/workspace","bindings":[{"session":address()}],"handoffs":[],"startup":null}])).unwrap()]).unwrap();
    drop(connection);
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    assert!(!storage.updating().unwrap());
    let context = Context {
        now: 1000,
        seed: Uuid::new_v4(),
        steers: true,
        preparation: None,
        rewind_binding: None,
        imported_turns: &[],
        external_busy: false,
        steer_ready: true,
    };
    let restarted = storage
        .decide(
            &address(),
            Event::Restart {
                updating: false,
                continue_after_restart: true,
            },
            context,
            None,
        )
        .unwrap();
    assert!(restarted.step.continuations.is_empty());
    assert!(matches!(
        storage.state(&address()).unwrap().turns["original-turn"].state,
        decision::TurnState::Interrupted
    ));
    let operation = Uuid::new_v4();
    let submitted = storage
        .decide(
            &address(),
            Event::Command {
                id: decision::OperationId::Client(operation),
                receipt: None,
                command: decision::Command::Submit {
                    id: operation.to_string().into(),
                    content: decision::InputContent {
                        parts: input(Uuid::new_v4()).input,
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: FollowUp::Queue,
                    },
                },
            },
            context,
            None,
        )
        .unwrap();
    assert_eq!(submitted.step.outcome, Outcome::Accepted);
    assert!(
        submitted
            .step
            .actions
            .iter()
            .any(|action| matches!(action, decision::Action::Deliver { .. }))
    );
}

#[tokio::test]
async fn the_first_update_keeps_the_detached_workers_journal_and_schedules_recovery_once() {
    for sending in [false, true] {
        use agent_local_host::update::{
            ContinuationDelivery, InterruptedTask, RECOVERY_FILE, Recovery,
        };
        let directory = tempfile::tempdir().unwrap();
        previous(&directory.path().join("bex-transcripts.sqlite"), false);
        let cwd = directory.path().to_str().unwrap().to_owned();
        let connection = Connection::open(directory.path().join("bex-transcripts.sqlite")).unwrap();
        let mut turn: Turn = protocol::decode(
            &connection
                .query_row("SELECT body FROM turns", [], |row| row.get::<_, Vec<u8>>(0))
                .unwrap(),
        )
        .unwrap();
        use base64::Engine;
        let native = SessionRef::new(ProviderKind::Claude, "second-native".into()).unwrap();
        let qualified = |id: &str| {
            format!(
                "bex:{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(serde_json::to_vec(&(&native, id)).unwrap())
            )
        };
        let canonical = qualified("original-turn");
        let mut recorded_native = turn.clone();
        recorded_native.status = TurnStatus::Completed;
        turn.id = canonical.clone().into();
        for item in turn.items.iter_mut().flatten() {
            let item = Arc::make_mut(item);
            item.id = qualified(item.id.as_str()).into();
        }
        let bindings = serde_json::json!([{"id":address(),"active":native,"cwd":cwd,"bindings":[{"session":address(),"covered":0},{"session":native,"covered":1}],"handoffs":[],"startup":null}]);
        connection
            .execute(
                "UPDATE host_state SET body=?1 WHERE name='bindings'",
                [serde_json::to_vec(&bindings).unwrap()],
            )
            .unwrap();
        let mut metadata: ThreadResponse = protocol::decode(
            &connection
                .query_row("SELECT metadata FROM conversations", [], |row| {
                    row.get::<_, Vec<u8>>(0)
                })
                .unwrap(),
        )
        .unwrap();
        metadata.thread.cwd = Some(cwd.clone());
        connection
            .execute(
                "UPDATE conversations SET cwd=?1,metadata=?2",
                params![cwd, protocol::encode(metadata).unwrap()],
            )
            .unwrap();
        turn.status = TurnStatus::Running;
        connection
            .execute(
                "UPDATE turns SET body=?1",
                [protocol::encode(turn).unwrap()],
            )
            .unwrap();
        let external = SessionRef::new(ProviderKind::Codex, "external-running-cli".into()).unwrap();
        connection
            .execute(
                "INSERT INTO conversations VALUES ('codex',?1,?2,?3)",
                params![
                    external.id,
                    cwd,
                    protocol::encode(ThreadResponse {
                        thread: Thread {
                            id: Some(external.clone()),
                            cwd: Some(cwd.clone()),
                            ..Default::default()
                        },
                        ..Default::default()
                    })
                    .unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO turns VALUES ('codex',?1,0,?2)",
                params![
                    external.id,
                    protocol::encode(Turn {
                        id: "external-turn".into(),
                        status: TurnStatus::Running,
                        items: Some(vec![]),
                        ..Default::default()
                    })
                    .unwrap()
                ],
            )
            .unwrap();
        let journal = Recovery {
            tasks: vec![InterruptedTask {
                native_session: native.clone(),
                submission: input(Uuid::new_v4()),
                cwd: Some(cwd.clone()),
                last_turn: Some("original-turn".into()),
                delivery: if sending {
                    ContinuationDelivery::Sending
                } else {
                    ContinuationDelivery::Pending
                },
            }],
        };
        let report = directory.path().join(RECOVERY_FILE);
        agent_local_host::save_private_json(&report, &journal).unwrap();
        let path = directory.path().join("bex-conversations.sqlite");
        let storage = Storage::open(&path);
        storage.check_ready().unwrap();
        assert_eq!(storage.updating().unwrap(), !sending);
        assert!(matches!(
            storage.state(&external).unwrap().turns["external-turn"].state,
            decision::TurnState::Interrupted
        ));
        // The old updater requires these exact fields while reading only delivery.
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        assert_eq!(
            saved["tasks"][0]["native_session"],
            serde_json::json!(native)
        );
        assert!(saved["tasks"][0].get("submission").is_some());
        assert_eq!(saved["tasks"][0]["last_turn"], canonical);
        let context = Context {
            now: 1000,
            seed: Uuid::new_v4(),
            steers: true,
            preparation: None,
            rewind_binding: None,
            imported_turns: &[],
            external_busy: false,
            steer_ready: true,
        };
        let external_restart = storage
            .decide(
                &external,
                Event::Restart {
                    updating: !sending,
                    continue_after_restart: true,
                },
                context,
                None,
            )
            .unwrap();
        assert!(external_restart.step.continuations.is_empty());
        if !sending {
            let restarted = storage
                .decide(
                    &address(),
                    Event::Restart {
                        updating: true,
                        continue_after_restart: true,
                    },
                    context,
                    None,
                )
                .unwrap();
            assert_eq!(restarted.step.continuations.len(), 1);
            let again = storage
                .decide(
                    &address(),
                    Event::Restart {
                        updating: true,
                        continue_after_restart: true,
                    },
                    context,
                    None,
                )
                .unwrap();
            assert!(again.step.continuations.is_empty());
        }
        drop(storage);
        let agent = crate::host_rpc::runtime::native_fixture::NativeAgent::new(
            ProviderKind::Claude,
            directory.path().join("claude"),
        );
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(cwd.clone()),
                    status: agent_protocol::models::SessionStatus::Idle,
                    turns: Some(vec![Arc::new(recorded_native)]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let service = crate::HostRpcService::new(
            [agent.backend()],
            directory.path().join("bex-projects.json"),
        );
        service.set_history_scope(directory.path().to_owned(), true);
        let _connection = service.open_session();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.resume_after_update(directory.path()),
        )
        .await
        .unwrap()
        .unwrap();
        let recovered: Recovery = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        if sending {
            assert!(matches!(
                recovered.tasks[0].delivery,
                ContinuationDelivery::Failed(_)
            ));
            assert!(
                agent.inputs.lock().unwrap().is_empty(),
                "an uncertain continuation must never be resent"
            );
        } else {
            assert!(
                matches!(
                    recovered.tasks[0].delivery,
                    ContinuationDelivery::Accepted(_)
                ),
                "{:?}",
                recovered.tasks[0].delivery
            );
            assert_eq!(agent.inputs.lock().unwrap().len(), 1);
        }

        drop(service);
    }
}

#[test]
fn native_reimport_retains_recorded_turns_absent_from_the_native_transcript() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("bex-transcripts.sqlite");
    previous(&old, false);
    let connection = Connection::open(old).unwrap();
    connection
        .execute("UPDATE conversations SET metadata=NULL", [])
        .unwrap();
    connection
        .execute("DELETE FROM host_state WHERE name='bindings'", [])
        .unwrap();
    let storage = Storage::open(&directory.path().join("bex-conversations.sqlite"));
    storage.check_ready().unwrap();
    let claim = storage.claim(i64::MAX / 2, 16).unwrap().into_iter().find(|claim| matches!(&claim.work, jobs::Work::Import { target, .. } if target == &address())).unwrap();
    storage
        .reimport(
            &address(),
            &address(),
            claim.id,
            claim.attempt,
            &ThreadResponse {
                thread: Thread {
                    id: Some(address()),
                    cwd: Some("/kept/workspace".into()),
                    turns: Some(vec![]),
                    ..Default::default()
                },
                ..Default::default()
            },
            100,
        )
        .unwrap();
    let history = storage.history(&address(), 0).unwrap();
    let turns = history.thread.turns.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id.as_str(), "original-turn");
    assert!(
        matches!(turns[0].items.as_ref().unwrap()[0].body(), ItemBody::AssistantText { text, .. } if text == "kept answer")
    );
}
