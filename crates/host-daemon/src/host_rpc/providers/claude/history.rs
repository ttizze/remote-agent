//! Compatibility with the existing client protocol and one-time legacy import.
//! Mutable conversation state lives only in the neutral session projection.
use agent_core::{
    models::{Item, Thread, ThreadResponse, ThreadStatus, Turn},
    session::{Content, Entry, Event, Execution, MessagePart, Outcome, Session, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub(super) fn outcome(value: Option<Outcome>) -> &'static str {
    match value {
        None => "inProgress",
        Some(Outcome::Completed) => "completed",
        Some(Outcome::Interrupted) => "interrupted",
        Some(Outcome::Failed) => "failed",
    }
}

pub(super) fn input(value: &Value) -> Result<Vec<MessagePart>, String> {
    value
        .as_array()
        .ok_or_else(|| "conversation input is not an array".into())
        .map(|parts| {
            parts
                .iter()
                .map(|part| {
                    let allowed: &[&str] = match part["type"].as_str() {
                        Some("text") => &["type", "text", "text_elements"],
                        Some("localImage") => &["type", "path"],
                        Some("mention") => &["type", "path", "name"],
                        _ => &[],
                    };
                    if part.as_object().is_some_and(|object| {
                        object.keys().all(|key| allowed.contains(&key.as_str()))
                    }) {
                        match part["type"].as_str() {
                            Some("text") if part["text"].is_string() => {
                                return MessagePart::Text {
                                    text: part["text"].as_str().unwrap().into(),
                                    annotations: part.get("text_elements").cloned(),
                                };
                            }
                            Some("localImage") if part["path"].is_string() => {
                                return MessagePart::Image {
                                    path: part["path"].as_str().unwrap().into(),
                                };
                            }
                            Some("mention")
                                if part["path"].is_string() && part["name"].is_string() =>
                            {
                                return MessagePart::File {
                                    path: part["path"].as_str().unwrap().into(),
                                    name: part["name"].as_str().unwrap().into(),
                                };
                            }
                            _ => {}
                        }
                    }
                    MessagePart::ProviderContent {
                        payload: part.clone(),
                    }
                })
                .collect()
        })
}

fn wire_input(parts: &[MessagePart]) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|part| match part {
                MessagePart::Text { text, annotations } => {
                    let mut value = json!({"type":"text","text":text});
                    if let Some(annotations) = annotations {
                        value["text_elements"] = annotations.clone();
                    }
                    value
                }
                MessagePart::Image { path } => json!({"type":"localImage","path":path}),
                MessagePart::File { path, name } => {
                    json!({"type":"mention","path":path,"name":name})
                }
                MessagePart::ProviderContent { payload } => payload.clone(),
            })
            .collect(),
    )
}

pub(super) fn item(entry: &Entry) -> Item {
    let value = match &entry.content {
        Content::UserMessage { input, client_id } => {
            json!({"id":entry.id,"type":"userMessage","content":wire_input(input),"clientId":client_id})
        }
        Content::AssistantText { text } => json!({"id":entry.id,"type":"agentMessage","text":text}),
        Content::Thinking { text } => json!({"id":entry.id,"type":"reasoning","text":text}),
        Content::ToolCall {
            name,
            arguments,
            result,
            outcome: state,
        } => {
            json!({"id":entry.id,"type":"mcpToolCall","server":"Claude Code","tool":name,"arguments":arguments,"result":result,"status":outcome(*state)})
        }
        Content::ProviderContent { provider, payload } => {
            return serde_json::from_value(payload.clone()).unwrap_or_else(|_| Item {
                id: entry.id.clone(),
                kind: Some("providerContent".into()),
                extra: serde_json::Map::from_iter([
                    ("provider".into(), json!(provider)),
                    ("payload".into(), payload.clone()),
                ]),
                ..Default::default()
            });
        }
    };
    serde_json::from_value(value).expect("validated legacy or constructed conversation item")
}

pub(super) fn execution(execution: &Execution) -> Turn {
    Turn {
        id: execution.id.clone(),
        status: Some(outcome(execution.outcome).into()),
        items: Some(
            execution
                .entries
                .iter()
                .map(|entry| Arc::new(item(entry)))
                .collect(),
        ),
        started_at: Some(Some(execution.started_at.into())),
        completed_at: execution.completed_at.map(|at| Some(at.into())),
        error: execution.error.clone(),
        ..Default::default()
    }
}

pub(super) fn response(session: &Session, include_turns: bool) -> ThreadResponse {
    let preview = session
        .executions
        .iter()
        .flat_map(|execution| &execution.entries)
        .find_map(|entry| {
            let Content::UserMessage { input, .. } = &entry.content else {
                return None;
            };
            Some(
                input
                    .iter()
                    .filter_map(|part| match part {
                        MessagePart::Text { text, .. } => Some(text.as_str()),
                        MessagePart::ProviderContent { payload } => payload["text"].as_str(),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .chars()
                    .take(120)
                    .collect(),
            )
        });
    ThreadResponse {
        thread: Thread {
            id: Some(format!("claude:{}", session.id)),
            cwd: Some(session.cwd.clone()),
            name: session.title.clone(),
            status: Some(ThreadStatus {
                kind: if session
                    .executions
                    .last()
                    .is_some_and(|execution| execution.outcome.is_none())
                {
                    "active"
                } else {
                    "idle"
                }
                .into(),
                extra: Default::default(),
            }),
            turns: include_turns.then(|| {
                session
                    .executions
                    .iter()
                    .map(|turn| Arc::new(execution(turn)))
                    .collect()
            }),
            preview,
            created_at: Some(session.created_at.into()),
            updated_at: Some(session.updated_at.into()),
            history_cursor: Some(None),
            ..Default::default()
        },
        model: Some(format!("claude:{}", session.selection.model)),
        extra: Default::default(),
    }
}

#[derive(Deserialize)]
pub(super) struct Legacy {
    pub response: ThreadResponse,
    pub session_id: Uuid,
    pub resumable: bool,
}

impl Legacy {
    pub(super) fn events(self) -> Result<Vec<(u64, Event)>, String> {
        let thread = self.response.thread;
        let target = Target {
            provider: "claude".into(),
            model: self
                .response
                .model
                .as_deref()
                .and_then(|model| model.strip_prefix("claude:"))
                .ok_or("legacy Claude model is missing")?
                .into(),
        };
        let created = thread
            .created_at
            .as_ref()
            .and_then(serde_json::Number::as_u64)
            .unwrap_or_default();
        let updated = thread
            .updated_at
            .as_ref()
            .and_then(serde_json::Number::as_u64)
            .unwrap_or(created);
        let mut events = vec![
            (
                created,
                Event::Created {
                    id: self.session_id.to_string(),
                    cwd: thread.cwd.ok_or("legacy Claude cwd is missing")?,
                    title: thread.name,
                    selection: target.clone(),
                },
            ),
            (
                created,
                Event::ProviderState {
                    provider: "claude".into(),
                    state: json!({"session_id":self.session_id,"resumable":self.resumable}),
                },
            ),
        ];
        for turn in thread.turns.into_iter().flatten() {
            let started = turn
                .started_at
                .as_ref()
                .and_then(|at| at.as_ref())
                .and_then(serde_json::Number::as_u64)
                .unwrap_or(created);
            let completed = turn
                .completed_at
                .as_ref()
                .and_then(|at| at.as_ref())
                .and_then(serde_json::Number::as_u64)
                .unwrap_or(updated);
            let items = turn.items.as_deref().unwrap_or_default();
            let first = items
                .first()
                .filter(|item| item.kind.as_deref() == Some("userMessage"))
                .ok_or("legacy Claude execution has no user input")?;
            events.push((
                started,
                Event::ExecutionStarted {
                    id: turn.id.clone(),
                    target: None,
                    input: Entry {
                        id: first.id.clone(),
                        content: Content::UserMessage {
                            input: input(
                                first
                                    .extra
                                    .get("content")
                                    .ok_or("legacy user input is missing")?,
                            )?,
                            client_id: first.client_id.clone(),
                        },
                    },
                },
            ));
            for item in items.iter().skip(1) {
                events.push((
                    started,
                    Event::EntrySet {
                        execution_id: turn.id.clone(),
                        entry: Entry {
                            id: item.id.clone(),
                            content: Content::ProviderContent {
                                provider: "claude".into(),
                                payload: serde_json::to_value(item).map_err(|e| e.to_string())?,
                            },
                        },
                    },
                ));
            }
            let interrupted = turn.status.as_deref() == Some("inProgress");
            events.push((completed, Event::ExecutionFinished {
                execution_id: turn.id.clone(), outcome: match turn.status.as_deref() {
                    Some("completed") => Outcome::Completed,
                    Some("interrupted" | "inProgress") => Outcome::Interrupted,
                    _ => Outcome::Failed,
                },
                error: if interrupted { Some(json!({"message":"Hostが終了したためClaudeの実行が中断されました。もう一度送信してください。"})) }
                    else { turn.error.clone() },
            }));
        }
        Ok(events)
    }
}
