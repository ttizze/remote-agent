//! Provider-neutral conversation history. Providers retain their own inference
//! checkpoints; this projection describes the work visible to the user.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub content: Content,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessagePart {
    Text {
        text: String,
        annotations: Option<Value>,
    },
    Image {
        path: String,
    },
    File {
        path: String,
        name: String,
    },
    /// Unknown input and optional provider metadata survive legacy import.
    ProviderContent {
        payload: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    UserMessage {
        input: Vec<MessagePart>,
        client_id: Option<String>,
    },
    AssistantText {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        name: String,
        arguments: Value,
        result: Option<Value>,
        outcome: Option<Outcome>,
    },
    /// Preserve imported content that this version cannot interpret.
    ProviderContent {
        provider: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    /// None for imported executions whose original model was not recorded.
    pub target: Option<Target>,
    pub started_at: u64,
    pub completed_at: Option<u64>,
    pub outcome: Option<Outcome>,
    pub error: Option<Value>,
    pub entries: Vec<Arc<Entry>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// A preference for the next execution, not the owner of the conversation.
    pub selection: Target,
    pub executions: Vec<Arc<Execution>>,
    /// Opaque adapter checkpoints. Core never interprets these values.
    pub provider_state: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Created {
        id: String,
        cwd: String,
        title: Option<String>,
        selection: Target,
    },
    ExecutionStarted {
        id: String,
        target: Option<Target>,
        input: Entry,
    },
    EntrySet {
        execution_id: String,
        entry: Entry,
    },
    TextAppended {
        execution_id: String,
        entry_id: String,
        text: String,
    },
    ToolFinished {
        execution_id: String,
        entry_id: String,
        result: Value,
        outcome: Outcome,
    },
    ExecutionFinished {
        execution_id: String,
        outcome: Outcome,
        error: Option<Value>,
    },
    ProviderState {
        provider: String,
        state: Value,
    },
}

/// Sequence numbers are explicit, independent of physical JSONL line lengths.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub seq: u64,
    pub at: u64,
    pub event: Event,
}

/// Validate and project without modifying the caller's state. The log owner
/// publishes this result only after accepting the corresponding record.
pub fn project(previous: Option<&Session>, record: &Record) -> Result<Session, String> {
    if record.version != 1 {
        return Err(format!(
            "unsupported session log version {}",
            record.version
        ));
    }
    if let Event::Created {
        id,
        cwd,
        title,
        selection,
    } = &record.event
    {
        if previous.is_some() || id.is_empty() || cwd.is_empty() {
            return Err("invalid session creation".into());
        }
        return Ok(Session {
            id: id.clone(),
            cwd: cwd.clone(),
            title: title.clone(),
            created_at: record.at,
            updated_at: record.at,
            selection: selection.clone(),
            executions: Vec::new(),
            provider_state: BTreeMap::new(),
        });
    }
    let mut next = previous.ok_or("session creation is missing")?.clone();
    next.updated_at = next.updated_at.max(record.at);
    match &record.event {
        Event::Created { .. } => unreachable!(),
        Event::ExecutionStarted { id, target, input } => {
            if id.is_empty()
                || input.id.is_empty()
                || !matches!(input.content, Content::UserMessage { .. })
                || next
                    .executions
                    .iter()
                    .any(|execution| execution.id == *id || execution.outcome.is_none())
            {
                return Err("invalid or overlapping execution".into());
            }
            if let Some(target) = target {
                next.selection = target.clone();
            }
            next.executions.push(Arc::new(Execution {
                id: id.clone(),
                target: target.clone(),
                started_at: record.at,
                completed_at: None,
                outcome: None,
                error: None,
                entries: vec![Arc::new(input.clone())],
            }));
        }
        Event::ProviderState { provider, state } => {
            next.provider_state.insert(provider.clone(), state.clone());
        }
        event => {
            let id = match event {
                Event::EntrySet { execution_id, .. }
                | Event::TextAppended { execution_id, .. }
                | Event::ToolFinished { execution_id, .. }
                | Event::ExecutionFinished { execution_id, .. } => execution_id,
                _ => unreachable!(),
            };
            let execution = next
                .executions
                .iter_mut()
                .find(|execution| execution.id == *id)
                .ok_or("execution is missing")?;
            if execution.outcome.is_some() {
                return Err("execution has already finished".into());
            }
            let execution = Arc::make_mut(execution);
            match event {
                Event::EntrySet { entry, .. } => {
                    if entry.id.is_empty() {
                        return Err("entry ID is missing".into());
                    }
                    if let Some(old) = execution.entries.iter_mut().find(|old| old.id == entry.id) {
                        if std::mem::discriminant(&old.content)
                            != std::mem::discriminant(&entry.content)
                        {
                            return Err("entry content kind changed".into());
                        }
                        *old = Arc::new(entry.clone());
                    } else {
                        execution.entries.push(Arc::new(entry.clone()));
                    }
                }
                Event::TextAppended { entry_id, text, .. } => {
                    let entry = execution
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == *entry_id)
                        .ok_or("text entry is missing")?;
                    match &mut Arc::make_mut(entry).content {
                        Content::AssistantText { text: body }
                        | Content::Thinking { text: body } => body.push_str(text),
                        _ => return Err("entry does not accept text".into()),
                    }
                }
                Event::ToolFinished {
                    entry_id,
                    result,
                    outcome,
                    ..
                } => {
                    let entry = execution
                        .entries
                        .iter_mut()
                        .find(|entry| entry.id == *entry_id)
                        .ok_or("tool call is missing")?;
                    match &mut Arc::make_mut(entry).content {
                        Content::ToolCall {
                            result: old_result,
                            outcome: old_outcome,
                            ..
                        } if old_outcome.is_none() => {
                            *old_result = Some(result.clone());
                            *old_outcome = Some(*outcome);
                        }
                        _ => return Err("tool call is not pending".into()),
                    }
                }
                Event::ExecutionFinished { outcome, error, .. } => {
                    execution.outcome = Some(*outcome);
                    execution.error = error.clone();
                    execution.completed_at = Some(record.at);
                    for entry in &mut execution.entries {
                        if matches!(entry.content, Content::ToolCall { outcome: None, .. })
                            && let Content::ToolCall {
                                outcome: tool_outcome,
                                ..
                            } = &mut Arc::make_mut(entry).content
                        {
                            *tool_outcome = Some(if *outcome == Outcome::Interrupted {
                                Outcome::Interrupted
                            } else {
                                Outcome::Failed
                            });
                        }
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(next)
}
