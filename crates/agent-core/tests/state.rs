use agent_core::{
    models::{Item, Thread, Turn},
    state::{Event, Snapshot, reduce},
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
        let (next, _) = reduce(
            &previous,
            Event::Submitted {
                thread_id: "thread".into(),
                client_user_message_id: "client".into(),
                draft: Arc::new(sent),
                turn_id: None,
            },
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
        let accepted = Event::Submitted {
            thread_id: "thread".into(),
            client_user_message_id: "client".into(),
            draft: Arc::default(),
            turn_id: Some("turn".into()),
        };
        let echoed = Event::Notification {
            method: "item/completed".into(),
            params: json!({"threadId":"thread","turnId":"turn","item":{"id":"native","type":"userMessage","clientId":"client","content":[]}}),
        };
        let (first, second) = if echo_first {
            (echoed, accepted)
        } else {
            (accepted, echoed)
        };
        let (intermediate, _) = reduce(&pending, first);
        assert_eq!(intermediate.pending_submissions.len(), 1);
        let (finished, _) = reduce(&intermediate, second);
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
        let event = if let Some(model) = case["select"].as_str() {
            previous.models = Arc::new(models);
            Event::Intent(Intent::SelectModel {
                thread_id: "thread".into(),
                model: model.into(),
            })
        } else {
            Event::ModelsLoaded(models)
        };
        let (next, effects) = reduce(&previous, event);
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
        let event = if case["operation"] == "refresh" {
            Event::ThreadRefreshed(incoming)
        } else {
            Event::OlderLoaded {
                thread_id: id.clone(),
                thread: incoming,
                turn_id: case["turnId"].as_str().map(str::to_owned),
                cursor: case["cursor"].as_str().map(str::to_owned),
            }
        };
        let (next, effects) = reduce(&previous, event);
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
fn stopped_history_watch_cannot_reload_a_conversation() {
    use agent_core::state::Intent;
    let (watching, _) = reduce(
        &Snapshot::default(),
        Event::Intent(Intent::Watch {
            thread_id: "thread".into(),
            watch_key: 1,
            watch_id: 3,
            path: Some("/rollout".into()),
        }),
    );
    let changed = || Event::Notification {
        method: "host/thread/changed".into(),
        params: json!({"threadId":"thread","watchId":3}),
    };
    assert_eq!(reduce(&watching, changed()).1.len(), 1);
    let (stopped, _) = reduce(
        &watching,
        Event::Intent(Intent::Unwatch {
            watch_key: 1,
            watch_id: 3,
        }),
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
        let catalog = Event::ModelsLoaded(models.clone());
        let navigation = Event::Intent(Intent::NewChat("/fixture".into()));
        let events = if catalog_first {
            [catalog, navigation]
        } else {
            [navigation, catalog]
        };
        let mut current = Snapshot::default();
        for event in events {
            current = reduce(&current, event).0;
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
        let (returned, _) = reduce(&edited, Event::Intent(Intent::NewChat("/fixture".into())));
        assert!(Arc::ptr_eq(&edited.drafts, &returned.drafts));
        assert_eq!(returned.drafts["new:/fixture"].text, "keep");
    }
}

#[test]
fn changing_workspace_rejects_old_reads_and_preserves_file_drafts() {
    use agent_core::state::{FileDraft, Intent, Navigation, Workspace};
    let file: Arc<agent_core::models::FileContent> = Arc::new(serde_json::from_value(json!({
        "path":"/old/file", "revision":"r1", "text":"saved", "bom":false,
        "lineEnding":"lf", "size":5
    })).unwrap());
    let directory: Arc<agent_core::models::FileList> = Arc::new(serde_json::from_value(json!({
        "path":"/old", "entries":[], "truncated":false
    })).unwrap());
    let review: Arc<agent_core::models::WorkspaceReview> = Arc::new(serde_json::from_value(json!({
        "branch":"main", "additions":1, "deletions":0, "files":[], "diff":"old"
    })).unwrap());
    let previous = Snapshot {
        navigation: Arc::new(Navigation { cwd: "/old".into(), ..Default::default() }),
        workspace: Arc::new(Workspace {
            file: Some(file.clone()), directory: Some(directory.clone()),
            review: Some(review.clone()), review_cwd: Some("/old".into()),
            settings: Some(Arc::default()), ..Default::default()
        }),
        file_drafts: Arc::new(BTreeMap::from([("/old/file".into(), FileDraft {
            revision: "r1".into(), text: "unsaved".into()
        })])),
        ..Default::default()
    };
    for cwd in ["/old", "/new"] {
        for open_thread in [false, true] {
            let event = if open_thread {
                Event::ThreadOpened {
                    generation: 0,
                    thread: serde_json::from_value(json!({"id":"thread", "cwd":cwd})).unwrap(),
                    model: None,
                }
            } else { Event::Intent(Intent::NewChat(cwd.into())) };
            let (next, _) = reduce(&previous, event);
            assert!(Arc::ptr_eq(&previous.file_drafts, &next.file_drafts));
            assert!(next.workspace.settings.is_some());
            if cwd == "/old" {
                assert!(Arc::ptr_eq(&previous.workspace, &next.workspace));
                continue;
            }
            assert!(next.workspace.file.is_none(), "old file remains after navigation");
            assert!(next.workspace.directory.is_none());
            assert!(next.workspace.review.is_none());
            assert!(next.workspace.review_cwd.is_none());
            for response in [
                Event::FileLoaded { request: 0, file: (*file).clone() },
                Event::FilesLoaded { request: 0, files: (*directory).clone() },
                Event::ReviewLoaded { request: 0, review: (*review).clone() },
            ] {
                assert_eq!(reduce(&next, response).0.workspace, next.workspace);
            }
        }
    }
}
