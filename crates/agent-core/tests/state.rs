use agent_core::{
    models::{Item, Thread, ThreadResponse, Turn},
    state::{
        Effect, Event, Snapshot,
        operations::{self as op, Operation},
        reduce,
    },
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

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
fn reply(thread: Thread) -> ThreadResponse {
    ThreadResponse {
        thread,
        model: None,
        extra: Default::default(),
    }
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
fn submission_drafts_corpus() {
    use agent_core::state::Draft;
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/submission-drafts.json")).unwrap();
    for case in cases {
        let sent: Draft = serde_json::from_value(case["sent"].clone()).unwrap();
        let current: Draft = serde_json::from_value(case["current"].clone()).unwrap();
        let previous = Snapshot {
            drafts: Arc::new(BTreeMap::from([("thread".into(), Arc::new(current))])),
            ..Default::default()
        };
        let (next, _) = applied(
            &previous,
            op::SendSubmission {
                thread_id: "thread".into(),
                client_user_message_id: "client".into(),
                draft: Arc::new(sent),
            },
            None,
        );
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(*next.drafts["thread"], expected, "{}", case["name"]);
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
                finished = reduce(&finished, Event::Notification {
                    method: "item/completed".into(),
                    params: json!({"threadId":"thread","turnId":"turn","item":{"id":"native","type":"userMessage","clientId":"client","content":[]}}),
                }).0;
            } else {
                op::SendSubmission {
                    thread_id: "thread".into(),
                    client_user_message_id: "client".into(),
                    draft: Arc::default(),
                }
                .apply(&mut finished, Some("turn".into()));
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
            applied(&previous, op::LoadModels {}, models)
        };
        assert!(effects.is_empty());
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(*next.drafts["thread"], expected, "{}", case["name"]);
        if *previous.drafts["thread"] == expected {
            assert!(Arc::ptr_eq(&previous.drafts, &next.drafts));
        }
    }
}
#[test]
fn history_corpus() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/history.json")).unwrap();
    assert_eq!(cases.len(), 8);
    for case in cases {
        let previous: Thread = serde_json::from_value(case["previous"].clone()).unwrap();
        let id = previous.id.clone().unwrap();
        let incoming = serde_json::from_value(case["incoming"].clone()).unwrap();
        let previous = initial(previous);
        let (next, effects) = if case["operation"] == "refresh" {
            applied(&previous, op::ReadThread::new(id.clone()), reply(incoming))
        } else {
            applied(
                &previous,
                op::ReadOlder {
                    thread_id: id.clone(),
                    turn_id: case["turnId"].as_str().map(str::to_owned),
                    cursor: case["cursor"].as_str().map(str::to_owned),
                    defer_item_details: true,
                },
                reply(incoming),
            )
        };
        assert!(effects.is_empty());
        if let Some(expected) = case.get("expected") {
            assert_eq!(next.error, None, "{}", case["name"]);
            assert_eq!(
                serde_json::to_value(&next.conversations[&id]).unwrap(),
                *expected,
                "{}",
                case["name"]
            );
        } else {
            let expected = match case["errorContains"].as_str().unwrap() {
                "会話ID" => "thread ID",
                "カーソル" => "cursor",
                other => panic!("unknown error {other}"),
            };
            assert!(next.error.unwrap().contains(expected), "{}", case["name"]);
            assert_eq!(next.conversations, previous.conversations);
        }
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
                Event::Notification {
                    method: event["method"].as_str().unwrap().into(),
                    params: event["params"].clone(),
                },
            );
            assert!(effects.is_empty());
            assert_eq!(next.error, None, "{}", case["name"]);
            state = next;
        }
        assert_eq!(
            serde_json::to_value(&state.conversations[&id]).unwrap(),
            case["expected"],
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
        Event::Notification {
            method: "item/agentMessage/delta".into(),
            params: json!({"threadId":"thread","turnId":"live","itemId":"changed","delta":" after"}),
        },
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
                Event::Notification {
                    method: event["method"].as_str().unwrap().into(),
                    params: event["params"].clone(),
                },
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
fn only_external_conversations_watch_persisted_history() {
    for status in ["idle", "active", "notLoaded"] {
        let thread: Thread = serde_json::from_value(json!({
            "id":"thread", "path":"/isolated/rollout.jsonl",
            "status":{"type":status}, "turns":[]
        }))
        .unwrap();
        let (snapshot, effects) = applied(
            &Snapshot::default(),
            op::ReadThread::open("thread".into()),
            reply(thread),
        );
        assert_eq!(
            snapshot.navigation.watch_id.is_some(),
            status == "notLoaded",
            "{status}"
        );
        assert_eq!(
            effects.len(),
            usize::from(status == "notLoaded"),
            "{status}"
        );
    }
}

#[test]
fn resuming_an_external_conversation_stops_rollout_refreshes() {
    let thread = serde_json::from_value(json!({
        "id":"thread", "path":"/isolated/rollout.jsonl", "status":{"type":"notLoaded"}
    }))
    .unwrap();
    let (watching, _) = applied(
        &Snapshot::default(),
        op::ReadThread::open("thread".into()),
        reply(thread),
    );
    let watch_id = watching.navigation.watch_id;
    let (loaded, effects) = reduce(
        &watching,
        Event::Notification {
            method: "thread/status/changed".into(),
            params: json!({"threadId":"thread", "status":{"type":"active", "activeFlags":[]}}),
        },
    );
    assert!(loaded.navigation.watch_id.is_none());
    assert!(loaded.navigation.watch_thread_id.is_none());
    assert_eq!(effects.len(), 1);
    assert!(
        reduce(
            &loaded,
            Event::Notification {
                method: "host/thread/changed".into(),
                params: json!({"threadId":"thread", "watchId":watch_id}),
            }
        )
        .1
        .is_empty()
    );
}

#[test]
fn stopped_history_watch_cannot_reload_a_conversation() {
    use agent_core::state::Intent;
    let (watching, _) = reduce(
        &Snapshot::default(),
        Event::Intent(Intent::Watch(op::Watch {
            thread_id: "thread".into(),
            watch_key: 1,
            watch_id: 3,
            path: Some("/rollout".into()),
        })),
    );
    let changed = || Event::Notification {
        method: "host/thread/changed".into(),
        params: json!({"threadId":"thread","watchId":3}),
    };
    assert_eq!(reduce(&watching, changed()).1.len(), 1);
    let (stopped, _) = reduce(
        &watching,
        Event::Intent(Intent::Unwatch(op::Unwatch {
            watch_key: 1,
            watch_id: 3,
        })),
    );
    assert!(reduce(&stopped, changed()).1.is_empty());
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
                op::LoadModels {}.apply(&mut current, models.clone());
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
                    serde_json::from_value(json!({"thread":{"id":"thread", "cwd":cwd}})).unwrap(),
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
    op::ReadThread::open("chat".into()).apply(&mut unassigned, chat);
    assert!(unassigned.workspace.review.is_none());
    assert!(unassigned.workspace.review_cwd.is_none());
    assert_eq!(previous.file_drafts, unassigned.file_drafts);
}

#[test]
fn file_change_delta_rejects_invalid_targets_without_mutating_history() {
    for changes in [
        json!([false]),
        json!([7]),
        json!(["text"]),
        json!([[]]),
        json!([null]),
        json!({}),
        json!(null),
    ] {
        let previous = initial(
            serde_json::from_value(json!({
                "id":"thread", "turns":[{"id":"turn", "items":[{
                    "id":"file", "type":"fileChange", "changes":changes
                }]}]
            }))
            .unwrap(),
        );
        let (next, effects) = reduce(
            &previous,
            Event::Notification {
                method: "item/fileChange/outputDelta".into(),
                params: json!({"threadId":"thread","turnId":"turn","itemId":"file","delta":"tail"}),
            },
        );
        assert_eq!(
            next.error.as_deref(),
            Some("invalid file change delta target")
        );
        assert!(effects.is_empty());
        assert!(Arc::ptr_eq(&previous.conversations, &next.conversations));
    }
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
            Event::Notification {
                method: "item/fileChange/outputDelta".into(),
                params: json!({"threadId":"thread","turnId":"turn","itemId":"file","delta":"tail"}),
            },
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
fn late_fork_preserves_new_navigation_and_stores_the_fork() {
    use agent_core::state::Intent;
    let (forking, _) = reduce(
        &Snapshot::default(),
        Event::Intent(Intent::ForkThread(op::ForkThread::new(
            "old".into(),
            "turn".into(),
        ))),
    );
    let (navigated, _) = reduce(
        &forking,
        Event::Intent(Intent::NewChat { cwd: "/new".into() }),
    );
    let mut finished = navigated;
    op::ForkThread {
        thread_id: "old".into(),
        last_turn_id: "turn".into(),
        exclude_turns: false,
    }
    .stale(
        &mut finished,
        serde_json::from_value(json!({"thread":{"id":"forked","cwd":"/old"}})).unwrap(),
    );
    assert!(finished.navigation.thread_id.is_none());
    assert_eq!(finished.navigation.cwd, "/new");
    assert!(finished.conversations.contains_key("forked"));
}

#[test]
fn account_listing_does_not_invalidate_a_concurrent_login() {
    use agent_core::state::Intent;
    let (starting, _) = reduce(
        &Snapshot::default(),
        Event::Intent(Intent::StartAccountLogin(op::StartAccountLogin {})),
    );
    let (listing, _) = reduce(
        &starting,
        Event::Intent(Intent::ListAccounts(op::ListAccounts {})),
    );
    let mut finished = listing;
    op::StartAccountLogin {}.apply(&mut finished, serde_json::from_value(json!({"loginId":"login","userCode":"fixture-only","verificationUrl":"https://example.invalid"})).unwrap());
    assert_eq!(
        finished.account.login.as_ref().map(|l| l.login_id.as_str()),
        Some("login")
    );
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
            watch_id: Some(7),
            watch_thread_id: Some("thread".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (listed, effects) = reduce(&previous, Event::Intent(Intent::ShowThreadList));
    assert!(listed.navigation.thread_id.is_none());
    assert_eq!(listed.epoch, previous.epoch + 1);
    assert!(Arc::ptr_eq(&listed.drafts, &previous.drafts));
    assert!(listed.navigation.watch_id.is_none());
    assert!(listed.navigation.watch_thread_id.is_none());
    assert_eq!(effects.len(), 1);
    let (completed, _) = reduce(
        &listed,
        Event::Notification {
            method: "turn/completed".into(),
            params: json!({"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}),
        },
    );
    assert!(completed.activity.unread.contains("thread"));
    assert_eq!(completed.drafts["thread"].text, "下書き");
}

#[test]
fn serialized_events_preserve_operation_inputs_and_replay_state() {
    let events = vec![
        Event::Connected,
        Event::Intent(op::Intent::NewChat { cwd: "/fixture".into() }),
        Event::Intent(op::Intent::SetDraftText { thread_id: "new:/fixture".into(), text: "再生する下書き".into() }),
        Event::Intent(op::Intent::ReadFile(op::ReadFile { path: "/fixture/file".into(), discard_draft: true })),
        Event::ServerRequest(serde_json::from_value(json!({"id":"request","method":"item/commandExecution/requestApproval","params":{"futureField":[1,2]},"unknown":true})).unwrap()),
        Event::Intent(op::Intent::Respond(op::Respond { request_id: json!("request"), answer: agent_core::client::Answer::Raw { value: json!({"decision":"accept","futureField":true}) } })),
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
