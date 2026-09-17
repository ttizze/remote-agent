use agent_core::{
    models::{Item, Thread, ThreadResponse, Turn},
    state::{
        Effect, Event, Snapshot,
        operations::{self as op, Operation},
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

// Recorded provider events are adapted at the Host boundary before reaching
// the Store. The separate session tests exercise wire subscription identities.
#[path = "support/provider_fixture.rs"]
mod provider_fixture;
fn reduce(previous: &Snapshot, event: Event) -> (Snapshot, Vec<Effect>) {
    if let Event::Notification(agent_core::protocol::Notification::Provider { method, params }) =
        &event
        && let Ok(Some((id, change))) =
            provider_fixture::notification_change(method, params.clone())
    {
        if !previous.conversations.contains_key(&id) {
            let active = match &change {
                agent_core::session::SessionChange::Status { status } => {
                    status.kind == agent_core::models::ThreadStatusKind::Active
                }
                agent_core::session::SessionChange::Turn { completed, .. } => !completed,
                _ => return (previous.clone(), Vec::new()),
            };
            return agent_core::state::reduce(
                previous,
                Event::Notification(agent_core::protocol::Notification::Activity {
                    session: agent_core::session::SessionRef::from_thread_id(&id).unwrap(),
                    active,
                    finished: matches!(&change, agent_core::session::SessionChange::Turn {completed:true, turn} if turn.status.as_deref() == Some("completed")),
                }),
            );
        }
        let mut source = previous.clone();
        let subscription = uuid::Uuid::nil();
        Arc::make_mut(&mut source.subscriptions).insert(id.clone(), subscription);
        let (mut next, effects) = agent_core::state::reduce(
            &source,
            Event::SessionUpdate(Box::new(agent_core::session::SessionUpdate {
                subscription_id: subscription,
                change,
            })),
        );
        next.subscriptions = previous.subscriptions.clone();
        return (next, effects);
    }
    agent_core::state::reduce(previous, event)
}

fn initial(thread: Thread) -> Snapshot {
    Snapshot {
        conversations: Arc::new(BTreeMap::from([(
            thread.id.clone().unwrap(),
            Arc::new(thread),
        )])),
        ..Default::default()
    }
}

fn applied<O: Operation>(
    previous: &Snapshot,
    operation: O,
    output: O::Output,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    let effects = operation.apply(&mut next, output);
    (next, effects)
}
fn reply(thread: Thread) -> agent_core::session::OpenedSession {
    agent_core::session::OpenedSession {
        session: agent_core::session::SessionRef::from_thread_id(thread.id.as_deref().unwrap())
            .unwrap(),
        subscription_id: uuid::Uuid::nil(),
        response: ThreadResponse {
            thread,
            model: None,
        },
    }
}

#[test]
fn snapshot_preserves_history_drafts_and_navigation() {
    let mut snapshot = initial(
        serde_json::from_value(json!({
            "id":"thread", "historyCursor":null,
            "turns":[{"id":"oldest"},{"id":"latest"}]
        }))
        .unwrap(),
    );
    snapshot.drafts = Arc::new(BTreeMap::from([(
        "thread".into(),
        Arc::new(agent_core::state::Draft {
            text: "unsent input".into(),
            ..Default::default()
        }),
    )]));
    snapshot.navigation = Arc::new(agent_core::state::Navigation {
        thread_id: Some("thread".into()),
        cwd: "/fixture".into(),
        draft_key: "thread".into(),
    });
    snapshot.pending_submissions = Arc::new(BTreeMap::from([(
        "sent".into(),
        Arc::new(agent_core::state::PendingSubmission {
            sequence: 0,
            draft_key: "thread".into(),
            draft: snapshot.drafts["thread"].clone(),
            turn_id: Some("latest".into()),
            after_item_id: None,
            accepted: true,
            delivery_unknown: false,
        }),
    )]));
    let restored: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert_eq!(
        restored, snapshot,
        "persisted snapshots must retain loaded history after reopening"
    );
}

#[test]
fn selected_folder_preserves_explicit_scope_without_losing_the_execution_directory() {
    for (project_id, selected) in [
        (None, "/workspace"),
        (Some(Value::Null), ""),
        (Some(json!("project")), "/workspace"),
    ] {
        let mut thread = json!({"id":"thread","cwd":"/workspace"});
        if let Some(project_id) = project_id {
            thread["projectId"] = project_id;
        }
        let thread: Thread = serde_json::from_value(thread).unwrap();
        let (snapshot, _) = applied(
            &Snapshot {
                connected: true,
                ..Default::default()
            },
            op::ReadThread::open("thread".into()),
            reply(thread),
        );
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        for snapshot in [&snapshot, &restored] {
            assert_eq!(snapshot.navigation.cwd, "/workspace");
            assert_eq!(snapshot.selected_directory(), selected);
            assert_eq!(
                snapshot.workspace.review_cwd.as_deref(),
                (!selected.is_empty()).then_some("/workspace")
            );
            for cwd in ["", "/another-project"] {
                let (next, _) = reduce(
                    snapshot,
                    Event::Intent(agent_core::state::Intent::NewChat { cwd: cwd.into() }),
                );
                assert_eq!(next.selected_directory(), cwd);
            }
        }
    }
}

#[test]
fn pending_submission_reconciles_both_reply_and_echo_orders() {
    use agent_core::state::Intent;
    for echo_first in [false, true] {
        let previous = initial(
            serde_json::from_value(json!({"id":"thread","turns":[{"id":"turn","items":[]}]}))
                .unwrap(),
        );
        let (pending, _) = reduce(
            &previous,
            Event::Intent(Intent::Submit {
                thread_id: Some("thread".into()),
                client_user_message_id: "client".into(),
            }),
        );
        let mut finished = pending.clone();
        for (index, echo) in [echo_first, !echo_first].into_iter().enumerate() {
            if echo {
                finished = reduce(&finished, Event::Notification(agent_core::protocol::Notification::Provider {
                    method: "item/completed".into(),
                    params: json!({"threadId":"thread","turnId":"turn","item":{"id":"native","type":"userMessage","clientId":"client","content":[]}}),
                })).0;
            } else {
                op::SendSubmission {
                    thread_id: "thread".into(),
                    client_user_message_id: "client".into(),
                    draft: Arc::default(),
                }
                .apply(
                    &mut finished,
                    op::SubmissionProgress::Sent(Some("turn".into())),
                );
            }
            if index == 0 {
                assert_eq!(finished.pending_submissions.len(), 1);
            }
        }
        assert!(finished.pending_submissions.is_empty());
        assert_eq!(pending.pending_submissions.len(), 1);
    }
}

#[test]
fn submission_acknowledgement_moves_to_the_returned_turn() {
    use agent_core::state::Intent;
    let previous = initial(
        serde_json::from_value(json!({"id":"thread","turns":[{
            "id":"old","status":"completed","items":[{"id":"answer","type":"agentMessage"}]
        }]}))
        .unwrap(),
    );
    let (mut pending, _) = reduce(
        &previous,
        Event::Intent(Intent::Submit {
            thread_id: Some("thread".into()),
            client_user_message_id: "client".into(),
        }),
    );
    assert_eq!(
        pending.pending_submissions["client"].turn_id.as_deref(),
        Some("old")
    );
    assert_eq!(
        pending.pending_submissions["client"]
            .after_item_id
            .as_deref(),
        Some("answer")
    );
    op::SendSubmission {
        thread_id: "thread".into(),
        client_user_message_id: "client".into(),
        draft: Arc::default(),
    }
    .apply(
        &mut pending,
        op::SubmissionProgress::Sent(Some("new".into())),
    );
    assert_eq!(
        pending.pending_submissions["client"].turn_id.as_deref(),
        Some("new")
    );
    assert!(
        pending.pending_submissions["client"]
            .after_item_id
            .is_none()
    );
}

#[test]
fn model_settings_corpus() {
    use agent_core::state::{Draft, Intent};
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/model-settings.json")).unwrap();
    for case in cases {
        let draft: Draft = serde_json::from_value(case["draft"].clone()).unwrap();
        let models = serde_json::from_value(case["models"].clone()).unwrap();
        let mut previous = Snapshot {
            drafts: Arc::new(BTreeMap::from([("thread".into(), Arc::new(draft))])),
            ..Default::default()
        };
        let (next, effects) = if let Some(model) = case["select"].as_str() {
            previous.models = Arc::new(models);
            reduce(
                &previous,
                Event::Intent(Intent::SelectModel {
                    thread_id: "thread".into(),
                    model: model.into(),
                }),
            )
        } else {
            applied(
                &previous,
                op::LoadModels {},
                serde_json::from_value::<agent_core::client::ModelPage>(json!({"data":models}))
                    .unwrap(),
            )
        };
        assert!(effects.is_empty());
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(*next.drafts["thread"], expected, "{}", case["name"]);
    }
}
#[test]
fn event_corpus() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/events.json")).unwrap();
    assert_eq!(cases.len(), 7);
    for case in cases {
        let previous: Thread = serde_json::from_value(case["previous"].clone()).unwrap();
        let id = previous.id.clone().unwrap();
        let mut state = initial(previous);
        for event in case["events"].as_array().unwrap() {
            let (next, effects) = reduce(
                &state,
                Event::Notification(agent_core::protocol::Notification::Provider {
                    method: event["method"].as_str().unwrap().into(),
                    params: event["params"].clone(),
                }),
            );
            assert!(effects.is_empty());
            assert_eq!(next.error, None, "{}", case["name"]);
            state = next;
        }
        assert_eq!(
            serde_json::to_value(&state.conversations[&id]).unwrap(),
            serde_json::to_value(
                serde_json::from_value::<Thread>(case["expected"].clone()).unwrap()
            )
            .unwrap(),
            "{}",
            case["name"]
        );
    }
}
#[test]
fn delta_copies_only_the_changed_path_and_snapshot_round_trips() {
    let item = |id: &str| {
        Arc::new(Item {
            id: id.into(),
            kind: Some("agentMessage".into()),
            text: Some("before".into()),
            ..Default::default()
        })
    };
    let thread = Thread {
        id: Some("thread".into()),
        turns: Some(vec![
            Arc::new(Turn {
                id: "old".into(),
                items: Some(vec![item("old-item")]),
                ..Default::default()
            }),
            Arc::new(Turn {
                id: "live".into(),
                status: Some("inProgress".into()),
                items: Some(vec![item("unchanged"), item("changed")]),
                ..Default::default()
            }),
        ]),
        ..Default::default()
    };
    let mut state = initial(thread);
    Arc::make_mut(&mut state.conversations).insert(
        "other".into(),
        Arc::new(Thread {
            id: Some("other".into()),
            ..Default::default()
        }),
    );
    let (next, _) = reduce(
        &state,
        Event::Notification(agent_core::protocol::Notification::Provider {
            method: "item/agentMessage/delta".into(),
            params: json!({"threadId":"thread","turnId":"live","itemId":"changed","delta":" after"}),
        }),
    );
    assert!(Arc::ptr_eq(
        &state.conversations["other"],
        &next.conversations["other"]
    ));
    let old = state.conversations["thread"].turns.as_ref().unwrap();
    let new = next.conversations["thread"].turns.as_ref().unwrap();
    assert!(Arc::ptr_eq(&old[0], &new[0]));
    assert!(Arc::ptr_eq(
        &old[1].items.as_ref().unwrap()[0],
        &new[1].items.as_ref().unwrap()[0]
    ));
    assert_eq!(
        old[1].items.as_ref().unwrap()[1].text.as_deref(),
        Some("before")
    );
    assert_eq!(
        new[1].items.as_ref().unwrap()[1].text.as_deref(),
        Some("before after")
    );
    let restored: Snapshot = serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
    assert_eq!(restored, next);
}

#[test]
fn activity_corpus_applies_even_without_a_loaded_conversation() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/activity.json")).unwrap();
    for case in cases {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id =
            case["visible"].as_str().map(str::to_owned);
        for event in case["events"].as_array().unwrap() {
            snapshot = reduce(
                &snapshot,
                Event::Notification(agent_core::protocol::Notification::Provider {
                    method: event["method"].as_str().unwrap().into(),
                    params: event["params"].clone(),
                }),
            )
            .0;
        }
        assert_eq!(
            snapshot.activity.active["a"],
            case["active"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            snapshot.activity.unread.contains("a"),
            case["unread"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        assert!(snapshot.conversations.is_empty());
    }
}

#[test]
fn new_chat_selects_catalog_defaults_in_either_load_order() {
    use agent_core::state::Intent;
    let models = serde_json::from_value::<Vec<agent_core::models::Model>>(json!([{
        "id":"model", "model":"model", "displayName":"Model",
        "defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"high"}],
        "defaultServiceTier":"priority", "serviceTiers":[{"id":"priority"}], "isDefault":true
    }]))
    .unwrap();
    for catalog_first in [false, true] {
        let mut current = Snapshot::default();
        for load_catalog in [catalog_first, !catalog_first] {
            if load_catalog {
                op::LoadModels {}.apply(
                    &mut current,
                    serde_json::from_value(json!({"data":models})).unwrap(),
                );
            } else {
                current = reduce(
                    &current,
                    Event::Intent(Intent::NewChat {
                        cwd: "/fixture".into(),
                    }),
                )
                .0;
            }
        }
        let draft = current.drafts.get("new:/fixture").expect("new chat draft");
        assert_eq!(draft.model.as_deref(), Some("model"));
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("priority"));
        let (edited, _) = reduce(
            &current,
            Event::Intent(Intent::SetDraftText {
                thread_id: "new:/fixture".into(),
                text: "keep".into(),
            }),
        );
        let (returned, _) = reduce(
            &edited,
            Event::Intent(Intent::NewChat {
                cwd: "/fixture".into(),
            }),
        );
        assert!(Arc::ptr_eq(&edited.drafts, &returned.drafts));
        assert_eq!(returned.drafts["new:/fixture"].text, "keep");
    }
}

#[test]
fn changing_workspace_clears_content_and_preserves_file_drafts() {
    use agent_core::state::{FileDraft, Intent, Navigation, Workspace};
    let file: Arc<agent_core::models::FileContent> = Arc::new(
        serde_json::from_value(json!({
            "path":"/old/file", "revision":"r1", "text":"saved", "bom":false,
            "lineEnding":"lf", "size":5
        }))
        .unwrap(),
    );
    let directory: Arc<agent_core::models::FileList> = Arc::new(
        serde_json::from_value(json!({
            "path":"/old", "entries":[], "truncated":false
        }))
        .unwrap(),
    );
    let review: Arc<agent_core::models::WorkspaceReview> = Arc::new(
        serde_json::from_value(json!({
            "branch":"main", "additions":1, "deletions":0, "files":[], "diff":"old"
        }))
        .unwrap(),
    );
    let previous = Snapshot {
        navigation: Arc::new(Navigation {
            cwd: "/old".into(),
            ..Default::default()
        }),
        workspace: Arc::new(Workspace {
            file: Some(file.clone()),
            directory: Some(directory.clone()),
            review: Some(review.clone()),
            review_cwd: Some("/old".into()),
            settings: Some(Arc::default()),
            worktrees: None,
        }),
        file_drafts: Arc::new(BTreeMap::from([(
            "/old/file".into(),
            FileDraft {
                revision: "r1".into(),
                text: "unsaved".into(),
            },
        )])),
        ..Default::default()
    };
    for cwd in ["/old", "/new"] {
        for open_thread in [false, true] {
            let next = if open_thread {
                let mut next = previous.clone();
                op::ReadThread::open("thread".into()).apply(
                    &mut next,
                    reply(serde_json::from_value(json!({"id":"thread", "cwd":cwd})).unwrap()),
                );
                next
            } else {
                reduce(
                    &previous,
                    Event::Intent(Intent::NewChat { cwd: cwd.into() }),
                )
                .0
            };
            assert!(Arc::ptr_eq(&previous.file_drafts, &next.file_drafts));
            assert!(next.workspace.settings.is_some());
            if cwd == "/old" {
                assert!(Arc::ptr_eq(&previous.workspace, &next.workspace));
                continue;
            }
            assert!(
                next.workspace.file.is_none(),
                "old file remains after navigation"
            );
            assert!(next.workspace.directory.is_none());
            assert!(next.workspace.review.is_none());
            assert!(next.workspace.review_cwd.is_none());
        }
    }
    let mut unassigned = previous.clone();
    let chat: ThreadResponse =
        serde_json::from_value(json!({"thread":{"id":"chat","cwd":"/old","projectId":null}}))
            .unwrap();
    Arc::make_mut(&mut unassigned.navigation).thread_id = Some("chat".into());
    Arc::make_mut(&mut unassigned.conversations)
        .insert("chat".into(), Arc::new(chat.thread.clone()));
    let mut unassigned: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&unassigned).unwrap()).unwrap();
    op::ReadThread::open("chat".into()).apply(&mut unassigned, reply(chat.thread));
    assert!(unassigned.workspace.review.is_none());
    assert!(unassigned.workspace.review_cwd.is_none());
    assert_eq!(previous.file_drafts, unassigned.file_drafts);
}

#[test]
fn file_change_delta_appends_to_known_changes() {
    for changes in [json!([]), json!([{}]), json!([{"diff":"prefix"}])] {
        let expected = if changes[0]["diff"].is_string() {
            "prefixtail"
        } else {
            "tail"
        };
        let previous = initial(
            serde_json::from_value(json!({
                "id":"thread", "turns":[{"id":"turn", "items":[{
                    "id":"file", "type":"fileChange", "changes":changes
                }]}]
            }))
            .unwrap(),
        );
        let (next, _) = reduce(
            &previous,
            Event::Notification(agent_core::protocol::Notification::Provider {
                method: "item/fileChange/outputDelta".into(),
                params: json!({"threadId":"thread","turnId":"turn","itemId":"file","delta":"tail"}),
            }),
        );
        assert_eq!(next.error, None);
        assert_eq!(
            serde_json::to_value(
                &next.conversations["thread"].turns.as_ref().unwrap()[0]
                    .items
                    .as_ref()
                    .unwrap()[0]
                    .changes
            )
            .unwrap()[0]["diff"],
            expected
        );
    }
}

#[test]
fn leaving_conversation_retains_draft_and_marks_later_completion_unread() {
    use agent_core::state::{Draft, Intent, Navigation};
    let previous = Snapshot {
        drafts: Arc::new(BTreeMap::from([(
            "thread".into(),
            Arc::new(Draft {
                text: "下書き".into(),
                ..Default::default()
            }),
        )])),
        navigation: Arc::new(Navigation {
            thread_id: Some("thread".into()),
            draft_key: "thread".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (listed, effects) = reduce(&previous, Event::Intent(Intent::ShowThreadList));
    assert!(listed.navigation.thread_id.is_none());
    assert_eq!(listed.epoch, previous.epoch + 1);
    assert!(Arc::ptr_eq(&listed.drafts, &previous.drafts));
    assert!(listed.subscriptions.is_empty());
    assert!(effects.is_empty());
    let (completed, _) = reduce(
        &listed,
        Event::Notification(agent_core::protocol::Notification::Provider {
            method: "turn/completed".into(),
            params: json!({"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}),
        }),
    );
    assert!(completed.activity.unread.contains("thread"));
    assert_eq!(completed.drafts["thread"].text, "下書き");
}

#[test]
fn serialized_events_preserve_operation_inputs_and_replay_state() {
    let events = vec![
        Event::Connected,
        Event::Intent(op::Intent::NewChat {
            cwd: "/fixture".into(),
        }),
        Event::Intent(op::Intent::SetDraftText {
            thread_id: "new:/fixture".into(),
            text: "再生する下書き".into(),
        }),
        Event::Intent(op::Intent::ReadFile(op::ReadFile {
            path: "/fixture/file".into(),
            discard_draft: true,
        })),
        Event::Intent(op::Intent::Respond(op::Respond {
            request_id: json!("request"),
            answer: agent_core::client::Answer::Raw {
                value: json!({"decision":"accept","futureField":true}),
            },
        })),
        Event::Disconnected("fixture disconnect".into()),
    ];
    let encoded = serde_json::to_vec(&events).unwrap();
    let decoded: Vec<Event> = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::to_value(&events).unwrap()
    );
    let replay = |events: Vec<Event>| {
        events
            .into_iter()
            .fold(Snapshot::default(), |snapshot, event| {
                reduce(&snapshot, event).0
            })
    };
    assert_eq!(replay(events.clone()), replay(decoded));
    assert_eq!(replay(events).drafts["new:/fixture"].text, "再生する下書き");
}

#[test]
fn durable_upload_and_pairing_results_survive_navigation() {
    let mut snapshot = reduce(
        &Snapshot::default(),
        Event::Intent(agent_core::state::Intent::NewChat { cwd: "/new".into() }),
    )
    .0;
    op::UploadAttachment {
        draft_key: "old".into(),
        attachment: agent_core::state::Attachment {
            path: "/local".into(),
            name: "image.png".into(),
            is_image: true,
        },
        directory: "/old".into(),
    }
    .stale(&mut snapshot, "/uploaded".into());
    assert_eq!(snapshot.drafts["old"].attachments[0].path, "/uploaded");
    for name in ["first", "updated"] {
        op::PairRemoteHost {
            invitation: serde_json::from_value(json!({
                "endpoint":"unused", "invitation":uuid::Uuid::nil(), "expiresAt":0
            }))
            .unwrap(),
            name: name.into(),
        }
        .stale(
            &mut snapshot,
            serde_json::from_value(json!({
                "id":"remote", "name":name, "ticket":"unused"
            }))
            .unwrap(),
        );
        assert_eq!(snapshot.management.remotes.len(), 1);
        assert_eq!(snapshot.management.remotes[0].name, name);
    }
    assert_eq!(snapshot.navigation.cwd, "/new");
    assert!(snapshot.error.is_none());
}

#[test]
fn incomplete_model_catalog_preserves_restored_choices_and_defaults_only_new_drafts() {
    use agent_core::state::{Draft, Intent};
    for restored in [false, true] {
        let draft = Draft {
            model: Some("codex-model".into()),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            text: "keep this input".into(),
            ..Default::default()
        };
        let mut state = Snapshot {
            drafts: Arc::new(BTreeMap::from([("saved".into(), Arc::new(draft.clone()))])),
            ..Default::default()
        };
        if restored {
            state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        }
        op::LoadModels {}.apply(
            &mut state,
            serde_json::from_value(json!({"data":[{
            "id":"claude:default","model":"claude:default","displayName":"Claude",
            "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]
        }],"providerErrors":{"codex":{"message":"offline"}}}))
            .unwrap(),
        );
        assert_eq!(*state.drafts["saved"], draft);
        assert_eq!(state.model_error_messages(), ["codex: offline"]);
        state = reduce(
            &state,
            Event::Intent(Intent::NewChat {
                cwd: "/fresh".into(),
            }),
        )
        .0;
        assert_eq!(
            state.drafts["new:/fresh"].model.as_deref(),
            Some("claude:default")
        );
        state = reduce(
            &state,
            Event::Intent(Intent::SelectEffort {
                thread_id: "saved".into(),
                effort: "high".into(),
            }),
        )
        .0;
        assert_eq!(*state.drafts["saved"], draft);
        let catalog = serde_json::from_value(json!({"data":[{
            "id":"codex-model","model":"codex-model","displayName":"Codex",
            "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}],
            "serviceTiers":[{"id":"priority"}]
        }]})).unwrap();
        op::LoadModels {}.apply(&mut state, catalog);
        assert_eq!(*state.drafts["saved"], draft);
        assert!(state.model_errors.is_empty());
    }
}

#[test]
fn completed_commands_refresh_session_metadata_without_waiting_for_the_turn() {
    for connected in [false, true] {
        let mut snapshot = initial(
            serde_json::from_value(json!({
                "id":"task", "status":{"type":"active"},
                "turns":[{"id":"turn","status":"inProgress","items":[]}]
            }))
            .unwrap(),
        );
        snapshot.connected = connected;
        let (next, effects) = reduce(
            &snapshot,
            Event::Notification(agent_core::protocol::Notification::Provider {
                method: "item/completed".into(),
                params: json!({"threadId":"task","turnId":"turn","item":{
                    "id":"merge","type":"commandExecution","command":"git merge task","status":"completed","exitCode":0
                }}),
            }),
        );
        assert_eq!(effects.len(), usize::from(connected));
        assert_eq!(
            next.conversations["task"].turns.as_ref().unwrap()[0]
                .status
                .as_deref(),
            Some("inProgress")
        );
        assert_eq!(next.error, None);
    }
}

#[test]
fn terminal_disconnect_keeps_resumption_and_host_switch_drops_old_handles() {
    use agent_core::state::{Event, Intent, Snapshot, TerminalPhase, operations as op, reduce};
    let snapshot = Snapshot {
        connected: true,
        storage_scope: "first-host".into(),
        ..Default::default()
    };
    let (starting, _) = reduce(
        &snapshot,
        Event::Intent(Intent::StartTerminal(op::StartTerminal {
            handle: "test".into(),
            cwd: "/fixture".into(),
            size: agent_core::client::TerminalSize { cols: 80, rows: 24 },
        })),
    );
    assert!(starting.terminal_view("test".into()).unwrap().accepts_input);
    let (disconnected, _) = reduce(&starting, Event::Disconnected("offline".into()));
    assert!(
        !disconnected
            .terminal_view("test".into())
            .unwrap()
            .accepts_input
    );
    let (late_failure, _) = reduce(
        &disconnected,
        Event::TerminalFailed {
            handle: "test".into(),
            reason: "cancelled".into(),
        },
    );
    assert_eq!(
        late_failure.terminals["test"].phase,
        TerminalPhase::Suspended
    );
    let (other_host, _) = reduce(&late_failure, Event::StorageScope("other-host".into()));
    assert!(other_host.terminals.is_empty());
}

#[test]
fn project_registration_navigates_only_while_current() {
    use agent_core::state::Intent;
    let previous = Snapshot {
        connected: true,
        ..Default::default()
    };
    let operation = op::AddProject {
        cwd: "/new-project".into(),
    };
    let (pending, _) = reduce(
        &previous,
        Event::Intent(Intent::AddProject(operation.clone())),
    );
    assert_eq!(pending.navigation, previous.navigation);
    let (opened, effects) = applied(&pending, operation.clone(), "/resolved-project".into());
    assert_eq!(opened.navigation.cwd, "/resolved-project");
    assert_eq!(opened.navigation.draft_key, "new:/resolved-project");
    assert_eq!(effects.len(), 2); // Workspace review and project-list refresh.
    let (mut elsewhere, _) = reduce(
        &pending,
        Event::Intent(Intent::NewChat {
            cwd: "/elsewhere".into(),
        }),
    );
    let navigation = elsewhere.navigation.clone();
    assert_eq!(
        operation
            .stale(&mut elsewhere, "/resolved-project".into())
            .len(),
        1
    );
    assert_eq!(elsewhere.navigation, navigation);
}

#[test]
fn failed_submission_restores_text_and_attachments_without_losing_new_input() {
    use agent_core::state::{Attachment, Draft, Intent};
    let attachment = |path: &str| Attachment {
        path: path.into(),
        name: path.into(),
        is_image: false,
    };
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.drafts).insert(
        "thread".into(),
        Arc::new(Draft {
            text: "sent".into(),
            attachments: vec![attachment("/sent")],
            ..Default::default()
        }),
    );
    let (mut snapshot, _) = reduce(
        &snapshot,
        Event::Intent(Intent::Submit {
            thread_id: Some("thread".into()),
            client_user_message_id: "pending".into(),
        }),
    );
    assert!(snapshot.drafts["thread"].text.is_empty());
    assert!(snapshot.drafts["thread"].attachments.is_empty());
    Arc::make_mut(&mut snapshot.drafts).insert(
        "thread".into(),
        Arc::new(Draft {
            text: "next".into(),
            attachments: vec![attachment("/next")],
            model: Some("new-model".into()),
            ..Default::default()
        }),
    );
    let (snapshot, _) = reduce(&snapshot, Event::SubmissionFailed("pending".into()));
    assert!(snapshot.pending_submissions.is_empty());
    let restored = &snapshot.drafts["thread"];
    assert_eq!(restored.text, "sent\nnext");
    assert_eq!(
        restored.attachments,
        vec![attachment("/next"), attachment("/sent")]
    );
    assert_eq!(restored.model.as_deref(), Some("new-model"));
}

#[test]
fn file_navigation_rejects_empty_and_relative_paths_before_rpc() {
    for path in ["", "nested", "../", "~/project"] {
        let (snapshot, effects) = reduce(
            &Snapshot::default(),
            Event::Intent(agent_core::state::Intent::ListFiles(op::ListFiles {
                path: path.into(),
            })),
        );
        assert!(effects.is_empty(), "sent an invalid path: {path:?}");
        assert_eq!(
            snapshot.error.as_deref(),
            Some("絶対パスを入力してください。")
        );
    }
    let (snapshot, effects) = reduce(
        &Snapshot::default(),
        Event::Intent(agent_core::state::Intent::ListFiles(op::ListFiles {
            path: "/".into(),
        })),
    );
    assert!(snapshot.error.is_none());
    assert_eq!(effects.len(), 1);
}
