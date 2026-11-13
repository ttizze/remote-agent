use super::*;
use crate::session::{ProviderKind, SessionRef};
use crate::state::operations::Operation;
use serde_json::json;

fn canonical_update(
    previous: &Snapshot,
    subscription: uuid::Uuid,
    change: crate::session::SessionChange,
) -> (Snapshot, Vec<Effect>) {
    let Some((snapshot, message)) =
        crate::test_conversations::changed(previous, subscription, change)
    else {
        return (previous.clone(), Vec::new());
    };
    super::super::conversations::received(&snapshot, subscription, message)
}

fn snapshot(provider: ProviderKind) -> Snapshot {
    let parent = SessionRef::new(provider, "parent".into()).unwrap();
    let subscription = uuid::Uuid::new_v4();
    Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            lists: crate::state::Lists {
                threads: Some(Arc::new(serde_json::from_value(json!({
                    "data":[{"id":parent,"name":"Task","cwd":"/fixture","status":"idle","hasChanges":true}],"projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,})).unwrap())),
                ..Default::default()
            },
            sessions: crate::state::Sessions {
                subscriptions: Arc::new([(parent.clone(), subscription)].into()),
                conversations: Arc::new([(parent.clone(), Arc::new(crate::models::Thread {
                    id: Some(parent.clone()),
                    turns: Some(vec![Arc::new(crate::models::Turn { id:"turn".into(), status:crate::models::TurnStatus::Running, items:Some(vec![]), ..Default::default() })]),
                    ..Default::default()
                }))].into()),
                ..Default::default()
            },
            ..Default::default()
        }
}

#[rstest::rstest]
fn prepared_workspace_updates_only_its_open_panel_without_losing_new_draft_edits(
    #[values(true, false)] selected: bool,
    #[values("", "side")] pane_id: &str,
) {
    let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
    let subscription = uuid::Uuid::new_v4();
    let draft_key = if selected {
        DraftKey::from(id.clone())
    } else {
        DraftKey::from("another")
    };
    let mut snapshot = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            ..Default::default()
        },
        sessions: crate::state::Sessions {
            conversations: Arc::new(
                [(
                    id.clone(),
                    Arc::new(Thread {
                        id: Some(id.clone()),
                        cwd: Some("/project".into()),
                        ..Default::default()
                    }),
                )]
                .into(),
            ),
            subscriptions: Arc::new([(id.clone(), subscription)].into()),
            ..Default::default()
        },
        input: crate::state::Input {
            drafts: Arc::new(
                [(
                    draft_key.clone(),
                    Arc::new(Draft {
                        text: "typed while preparing".into(),
                        ..Default::default()
                    }),
                )]
                .into(),
            ),
            ..Default::default()
        },
        ..Default::default()
    };
    snapshot.open_pane(pane_id.into(), "/project".into());
    Arc::make_mut(snapshot.panes.get_mut(pane_id).unwrap()).navigation = Arc::new(Navigation {
        thread_id: selected.then_some(id.clone()),
        draft_key: draft_key.clone(),
        cwd: "/project".into(),
    });
    let file = op::OperationKey::File {
        pane: pane_id.into(),
    };
    Arc::make_mut(&mut snapshot.operations).insert(
        file.clone(),
        op::OperationState {
            generation: 1,
            phase: op::OperationPhase::Failed {
                message: "old workspace".into(),
            },
        },
    );
    let (next, effects) = canonical_update(
        &snapshot,
        subscription,
        crate::session::SessionChange::Workspace {
            cwd: "/worktree".into(),
        },
    );
    assert_eq!(
        next.sessions.conversations[&id].cwd.as_deref(),
        Some("/worktree")
    );
    assert_eq!(
        next.panes.get(pane_id).unwrap().navigation.cwd,
        if selected { "/worktree" } else { "/project" }
    );
    assert_eq!(
        next.operations.contains_key(&file),
        !selected,
        "only a pane whose workspace moved loses its old read error"
    );
    assert_eq!(next.input.drafts[&draft_key].text, "typed while preparing");
    assert_eq!(effects.len(), usize::from(selected));
}

#[test]
fn a_preparing_task_reviews_no_changes_until_its_turn_starts() {
    use agent_protocol::session::{PreparationStatus, SessionPreparation};
    let id = SessionRef::new(ProviderKind::Codex, "admitted".into()).unwrap();
    let subscription = uuid::Uuid::new_v4();
    let snapshot = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            ..Default::default()
        },
        sessions: crate::state::Sessions {
            conversations: Arc::new(
                [(
                    id.clone(),
                    Arc::new(Thread {
                        id: Some(id.clone()),
                        cwd: Some("/project".into()),
                        preparations: vec![SessionPreparation {
                            submission: agent_protocol::operations::Submission {
                                thread_id: id.clone(),
                                client_user_message_id: "input".into(),
                                input: Vec::new(),
                                model: None,
                                effort: None,
                                service_tier: None,
                                follow_up: Default::default(),
                            },
                            status: PreparationStatus::Preparing,
                            stage: agent_protocol::session::PreparationStage::Workspace,
                        }],
                        ..Default::default()
                    }),
                )]
                .into(),
            ),
            subscriptions: Arc::new([(id.clone(), subscription)].into()),
            ..Default::default()
        },
        panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(
            crate::state::PaneState {
                navigation: Arc::new(Navigation {
                    thread_id: Some(id.clone()),
                    draft_key: id.clone().into(),
                    cwd: "/project".into(),
                }),
                ..Default::default()
            },
        )),
        ..Default::default()
    };
    let update = |snapshot: &Snapshot, change| canonical_update(snapshot, subscription, change);
    let mut reviewing = snapshot.clone();
    assert!(
        op::review_workspace(&mut reviewing, "").is_none(),
        "the source checkout's changes are not the preparing task's"
    );
    let (moved, effects) = update(
        &snapshot,
        crate::session::SessionChange::Workspace {
            cwd: "/worktree".into(),
        },
    );
    assert!(effects.is_empty());
    assert!(moved.panes.primary().workspace.review_cwd.is_none());
    let finish_preparation = |previous: &Snapshot| {
        let current = &previous.sessions.replicas[&id];
        let mut thread = previous.sessions.conversations[&id].as_ref().clone();
        thread.preparations.clear();
        let replica = crate::test_conversations::replica_at(
            crate::models::ThreadResponse {
                thread,
                ..Default::default()
            },
            current.subscription(),
            current.cursor().unwrap().sequence + 1,
        );
        super::super::conversations::received(previous, subscription, replica.snapshot().unwrap())
    };
    let (started, effects) = finish_preparation(&moved);
    assert_eq!(
        effects.len(),
        1,
        "completion reviews even if the provider already sent its items"
    );
    let (_, repeated) = finish_preparation(&started);
    assert!(
        repeated.is_empty(),
        "replayed completion does not refresh changes again"
    );
    let item = serde_json::from_value(json!({
        "id":"user","status":"completed","clientInputId":"input",
        "body":{"inline":{"body":{"userMessage":{"text":"start","content":[]}}}}
    }))
    .unwrap();
    let (_, effects) = update(
        &started,
        crate::session::SessionChange::Turn {
            turn: crate::models::Turn {
                id: "turn".into(),
                status: crate::models::TurnStatus::Running,
                items: Some(vec![Arc::new(item)]),
                ..Default::default()
            },
            completed: false,
        },
    );
    assert!(
        effects.is_empty(),
        "the preparation completion already refreshed the worktree"
    );
}

#[test]
fn replacing_history_drops_removed_turns_pages_and_late_reads_without_losing_the_draft() {
    {
        let mut state = snapshot(ProviderKind::Codex);
        let id = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&id)
                .unwrap(),
        );
        thread.history_cursor = Some("10".into());
        thread.history_has_more = Some(true);
        let before = thread.clone();
        let mut replacement = before.clone();
        replacement.turns = Some(Vec::new());
        replacement.history_cursor = None;
        replacement.history_has_more = Some(false);
        Arc::make_mut(&mut state.input.drafts).insert(
            id.clone().into(),
            Arc::new(crate::state::Draft {
                text: "keep this draft".into(),
                ..Default::default()
            }),
        );
        let replica = crate::test_conversations::replica_at(
            crate::models::ThreadResponse {
                thread: before.clone(),
                ..Default::default()
            },
            1,
            1,
        );
        Arc::make_mut(&mut state.sessions.replicas).insert(id.clone(), Arc::new(replica));
        let replacement = crate::test_conversations::replica_at(
            crate::models::ThreadResponse {
                thread: replacement,
                ..Default::default()
            },
            1,
            2,
        )
        .snapshot()
        .unwrap();
        let (mut next, effects) = crate::state::reduce(
            &state,
            "",
            Event::ConversationRows {
                subscription_id: state.sessions.subscriptions[&id],
                message: Box::new(replacement),
            },
        );
        assert!(
            effects.is_empty(),
            "the authoritative stream already supplied its replacement"
        );
        assert_eq!(next.sessions.subscriptions, state.sessions.subscriptions);
        assert_eq!(next.input.drafts, state.input.drafts);
        let late = crate::test_conversations::Opened {
            replica: crate::test_conversations::replica_at(
                crate::models::ThreadResponse {
                    thread: before,
                    ..Default::default()
                },
                0,
                1,
            ),
            subscription: uuid::Uuid::new_v4(),
        };
        op::ReadThread::new(id.clone()).stale(&mut next, "", late);
        let held = &next.sessions.conversations[&id];
        assert!(held.turns.as_ref().unwrap().is_empty());
        assert_eq!(held.history_cursor, None);
        assert_eq!(held.history_has_more, Some(false));
        assert_eq!(next.sessions.replicas[&id].cursor().unwrap().sequence, 2);
    }
}

#[test]
fn unrelated_fleets_do_not_trigger_reads_of_the_visible_fleet() {
    let mut state = snapshot(ProviderKind::Codex);
    let parent = state.lists.threads.as_ref().unwrap().data[0]
        .id
        .clone()
        .unwrap();
    Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(parent.clone());
    Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).thread_id =
        Some(parent);
    let session = |id: &str| SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
    for (id, parent) in [
        ("child", "parent"),
        ("grandchild", "child"),
        ("unrelated", "other"),
        ("cycle-a", "cycle-b"),
        ("cycle-b", "cycle-a"),
    ] {
        let id = session(id);
        Arc::make_mut(&mut state.sessions.subscriptions).insert(id.clone(), uuid::Uuid::new_v4());
        Arc::make_mut(&mut state.sessions.conversations).insert(
            id.clone(),
            Arc::new(Thread {
                id: Some(id),
                parent_id: Some(session(parent)),
                turns: Some(vec![Arc::new(crate::models::Turn {
                    id: "turn".into(),
                    status: crate::models::TurnStatus::Running,
                    items: Some(vec![]),
                    ..Default::default()
                })]),
                ..Default::default()
            }),
        );
    }
    let cross_provider = SessionRef::new(ProviderKind::Claude, "cross-provider".into()).unwrap();
    Arc::make_mut(&mut state.sessions.subscriptions)
        .insert(cross_provider.clone(), uuid::Uuid::new_v4());
    let mut cross_thread = state.sessions.conversations[&session("child")]
        .as_ref()
        .clone();
    cross_thread.id = Some(cross_provider.clone());
    Arc::make_mut(&mut state.sessions.conversations)
        .insert(cross_provider.clone(), Arc::new(cross_thread));
    for (id, expected) in [
        ("child", 1),
        ("grandchild", 1),
        ("unrelated", 0),
        ("cycle-a", 0),
    ]
    .into_iter()
    .map(|(id, expected)| (session(id), expected))
    // After a handoff the public parent retains its original provider,
    // while a native child belongs to the active provider.
    .chain([(cross_provider, 1)])
    {
        let item = serde_json::from_value(json!({
                "id":"spawn","status":"running","clientInputId":null,
                "body":{"inline":{"body":{"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":id,"receivers":[session("new-agent")],"states":[],"agentId":null,"result":null}}}}
            })).unwrap();
        let (next, effects) = canonical_update(
            &state,
            state.sessions.subscriptions[&id],
            crate::session::SessionChange::Item {
                turn_id: "turn".into(),
                item: Arc::new(item),
            },
        );
        assert_eq!(effects.len(), expected, "spawn {id}");
        assert_eq!(
            next.panes.primary().navigation,
            state.panes.primary().navigation
        );
    }
}

#[rstest::rstest]
#[case::codex(ProviderKind::Codex)]
#[case::claude(ProviderKind::Claude)]
fn streamed_commands_and_known_agents_do_not_reload_titles(#[case] provider: ProviderKind) {
    let mut state = snapshot(provider);
    Arc::make_mut(state.lists.threads.as_mut().unwrap()).data[0].cwd = Some("/fixture".into());
    let id = state.lists.threads.as_ref().unwrap().data[0]
        .id
        .clone()
        .unwrap();
    let subscription = state.sessions.subscriptions[&id];
    let child = SessionRef::new(provider, "child".into()).unwrap();
    Arc::make_mut(state.panes.primary_mut()).observed_agents = Some(id.clone());
    Arc::make_mut(&mut state.sessions.conversations).insert(
        child.clone(),
        Arc::new(crate::models::Thread {
            id: Some(child.clone()),
            parent_id: Some(id.clone()),
            ..Default::default()
        }),
    );
    for body in [
        json!({"commandExecution":{"command":"git status","cwd":null,"output":"","exitCode":0}}),
        json!({"subagent":{"tool":"wait","prompt":null,"model":null,"effort":null,"sender":id,"receivers":[child],"states":[],"agentId":null,"result":null}}),
    ] {
        for status in ["running", "completed"] {
            let item = serde_json::from_value(json!({
                "id":"work","status":status,"clientInputId":null,
                "body":{"inline":{"body":body}}
            }))
            .unwrap();
            let (next, effects) = canonical_update(
                &state,
                subscription,
                crate::session::SessionChange::Item {
                    turn_id: "turn".into(),
                    item: Arc::new(item),
                },
            );
            assert!(effects.is_empty());
            state = next;
        }
    }
    let (ended, effects) = canonical_update(
        &state,
        subscription,
        crate::session::SessionChange::Turn {
            turn: crate::models::Turn {
                id: "turn".into(),
                status: crate::models::TurnStatus::Completed,
                ..Default::default()
            },
            completed: true,
        },
    );
    assert!(
        effects.is_empty(),
        "an unselected conversation does not schedule a panel review"
    );
    assert!(!ended.sessions.subscriptions.contains_key(&id));
}

#[test]
fn user_messages_do_not_read_lists() {
    let state = snapshot(ProviderKind::Codex);
    let id = state.lists.threads.as_ref().unwrap().data[0]
        .id
        .clone()
        .unwrap();
    let item = serde_json::from_value(json!({
        "id":"user","status":"completed","clientInputId":null,
        "body":{"inline":{"body":{"userMessage":{"text":"New task title","content":[]}}}}
    }))
    .unwrap();
    let (_, effects) = canonical_update(
        &state,
        state.sessions.subscriptions[&id],
        crate::session::SessionChange::Item {
            turn_id: "turn".into(),
            item: Arc::new(item),
        },
    );
    assert!(effects.is_empty());
}

#[test]
fn running_subagents_refresh_only_the_observed_fleet_without_waiting_for_completion() {
    let mut snapshot = snapshot(ProviderKind::Codex);
    let parent = snapshot.lists.threads.as_ref().unwrap().data[0]
        .id
        .clone()
        .unwrap();
    let child = SessionRef::new(ProviderKind::Codex, "child".into()).unwrap();
    let subscription = snapshot.sessions.subscriptions[&parent];
    let item = serde_json::from_value(serde_json::json!({
            "id":"spawn","status":"running","clientInputId":null,
            "body":{"inline":{"body":{"subagent":{"tool":"spawnAgent","prompt":null,"model":null,"effort":null,"sender":parent,"receivers":[child],"states":[],"agentId":null,"result":null}}}}
        })).unwrap();
    Arc::make_mut(snapshot.panes.primary_mut()).observed_agents = Some(parent);
    let (next, effects) = canonical_update(
        &snapshot,
        subscription,
        crate::session::SessionChange::Item {
            turn_id: "turn".into(),
            item: Arc::new(item),
        },
    );
    assert_eq!(
        effects.len(),
        1,
        "a spawn refreshes while the parent is still running"
    );
    assert_eq!(
        next.panes.primary().navigation,
        snapshot.panes.primary().navigation
    );
}

#[test]
fn follow_up_delivery_in_an_established_checkout_keeps_the_review_available() {
    use agent_protocol::session::{PreparationStage, PreparationStatus, SessionPreparation};
    for status in [
        PreparationStatus::Sending,
        PreparationStatus::Unknown,
        PreparationStatus::Interrupted,
        PreparationStatus::Failed {
            error: Default::default(),
        },
    ] {
        let mut state = snapshot(ProviderKind::Codex);
        let id = SessionRef::new(ProviderKind::Codex, "parent".into()).unwrap();
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&id)
                .unwrap(),
        );
        thread.cwd = Some("/fixture".into());
        thread.preparations.push(SessionPreparation {
            submission: agent_protocol::operations::Submission {
                thread_id: id.clone(),
                client_user_message_id: "follow-up".into(),
                input: Vec::new(),
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            stage: PreparationStage::Provider,
            status,
        });
        let pane = Arc::make_mut(state.panes.primary_mut());
        pane.navigation = Arc::new(Navigation {
            thread_id: Some(id.clone()),
            draft_key: id.into(),
            cwd: "/fixture".into(),
        });
        Arc::make_mut(&mut pane.workspace).review_cwd = Some("/fixture".into());
        assert!(op::review_workspace(&mut state, "").is_some());
        assert_eq!(
            state.panes.primary().workspace.review_cwd.as_deref(),
            Some("/fixture")
        );
    }
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn failed_worktree_preparation_hides_the_source_checkout_until_a_checkout_exists(
    #[case] created: bool,
) {
    let preparation = agent_protocol::session::SessionPreparation {
        submission: agent_protocol::operations::Submission {
            thread_id: SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap(),
            client_user_message_id: "failed".into(),
            input: Vec::new(),
            model: None,
            effort: None,
            service_tier: None,
            follow_up: Default::default(),
        },
        status: agent_protocol::session::PreparationStatus::Failed {
            error: Default::default(),
        },
        stage: agent_protocol::session::PreparationStage::Provider,
    };
    let mut setup = agent_protocol::workspace::WorkspaceSetup::new(Some("main".into()), 0);
    setup.finish(
        agent_protocol::workspace::SetupPhase::Failed,
        Some("checkout failed".into()),
        1,
    );
    setup.worktree_path = created.then(|| "/worktree".into());
    assert_eq!(
        op::preparing_workspace(&[preparation], Some(&setup)),
        !created
    );
}
