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
