use agent_core::session::{Content, Entry, Event, MessagePart, Outcome, Record, Target, project};
use serde_json::json;

fn target(provider: &str) -> Target {
    Target {
        provider: provider.into(),
        model: format!("{provider}-model"),
    }
}
fn input() -> Entry {
    Entry {
        id: "input".into(),
        content: Content::UserMessage {
            input: vec![MessagePart::Text {
                text: "hello".into(),
                annotations: None,
            }],
            client_id: Some("client-input".into()),
        },
    }
}
fn record(seq: u64, event: Event) -> Record {
    Record {
        version: 1,
        seq,
        at: seq,
        event,
    }
}
fn created() -> Record {
    record(
        1,
        Event::Created {
            id: "conversation".into(),
            cwd: "/workspace".into(),
            title: None,
            selection: target("first"),
        },
    )
}

#[test]
fn replay_keeps_session_identity_and_execution_targets_and_replaces_streamed_content() {
    let records = [
        created(),
        record(
            2,
            Event::ExecutionStarted {
                id: "one".into(),
                target: Some(target("first")),
                input: input(),
            },
        ),
        record(
            3,
            Event::EntrySet {
                execution_id: "one".into(),
                entry: Entry {
                    id: "answer".into(),
                    content: Content::AssistantText { text: "par".into() },
                },
            },
        ),
        record(
            4,
            Event::TextAppended {
                execution_id: "one".into(),
                entry_id: "answer".into(),
                text: "tial".into(),
            },
        ),
        record(
            5,
            Event::EntrySet {
                execution_id: "one".into(),
                entry: Entry {
                    id: "answer".into(),
                    content: Content::AssistantText {
                        text: "final answer".into(),
                    },
                },
            },
        ),
        record(
            6,
            Event::EntrySet {
                execution_id: "one".into(),
                entry: Entry {
                    id: "tool".into(),
                    content: Content::ToolCall {
                        name: "read".into(),
                        arguments: json!({"path":"a.txt"}),
                        result: None,
                        outcome: None,
                    },
                },
            },
        ),
        record(
            7,
            Event::ToolFinished {
                execution_id: "one".into(),
                entry_id: "tool".into(),
                result: json!([{"text":"file body"}]),
                outcome: Outcome::Completed,
            },
        ),
        record(
            8,
            Event::ExecutionFinished {
                execution_id: "one".into(),
                outcome: Outcome::Completed,
                error: None,
            },
        ),
        record(
            9,
            Event::ExecutionStarted {
                id: "two".into(),
                target: Some(target("second")),
                input: input(),
            },
        ),
    ];
    let mut state = None;
    let mut before_append = None;
    for record in &records {
        // Exercise the persisted format, not just in-memory constructors.
        let decoded = serde_json::from_slice(&serde_json::to_vec(record).unwrap()).unwrap();
        state = Some(project(state.as_ref(), &decoded).unwrap());
        if record.seq == 3 {
            before_append = state.clone();
        }
    }
    let state = state.unwrap();
    assert_eq!(state.id, "conversation");
    assert_eq!(state.executions[0].target, Some(target("first")));
    assert_eq!(state.executions[1].target, Some(target("second")));
    assert_eq!(state.executions[0].entries.len(), 3);
    assert_eq!(
        state.executions[0].entries[1].content,
        Content::AssistantText {
            text: "final answer".into()
        }
    );
    assert_eq!(
        before_append.unwrap().executions[0].entries[1].content,
        Content::AssistantText { text: "par".into() }
    );
    assert!(
        matches!(&state.executions[0].entries[2].content, Content::ToolCall { outcome: Some(Outcome::Completed), result: Some(value), .. } if value == &json!([{"text":"file body"}]))
    );
}

#[test]
fn invalid_transitions_do_not_change_the_previous_projection() {
    let state = project(None, &created()).unwrap();
    let start = record(
        2,
        Event::ExecutionStarted {
            id: "one".into(),
            target: Some(target("first")),
            input: input(),
        },
    );
    let running = project(Some(&state), &start).unwrap();
    assert!(project(Some(&running), &start).is_err());
    assert!(
        project(
            Some(&running),
            &record(
                3,
                Event::TextAppended {
                    execution_id: "one".into(),
                    entry_id: "input".into(),
                    text: "wrong".into()
                }
            )
        )
        .is_err()
    );
    assert!(
        project(
            Some(&running),
            &record(
                3,
                Event::ToolFinished {
                    execution_id: "one".into(),
                    entry_id: "missing".into(),
                    result: json!(null),
                    outcome: Outcome::Completed
                }
            )
        )
        .is_err()
    );
    assert_eq!(running.executions[0].entries[0].as_ref(), &input());
    let finished = project(
        Some(&running),
        &record(
            3,
            Event::ExecutionFinished {
                execution_id: "one".into(),
                outcome: Outcome::Interrupted,
                error: None,
            },
        ),
    )
    .unwrap();
    assert!(
        project(
            Some(&finished),
            &record(
                4,
                Event::EntrySet {
                    execution_id: "one".into(),
                    entry: input()
                }
            )
        )
        .is_err()
    );
    assert_eq!(running.executions[0].outcome, None);
}
