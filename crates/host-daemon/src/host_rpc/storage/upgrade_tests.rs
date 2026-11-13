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
    assert_eq!(turns[1].items.as_ref().unwrap()[0].id.as_str(), user);
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
    assert_eq!(storage.projects().unwrap(), [project.clone()]);
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
    assert_eq!(recovered.phase, SetupPhase::Cancelled);
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
