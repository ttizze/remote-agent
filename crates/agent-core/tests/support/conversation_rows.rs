//! Test inputs use the same ordered rows as the Host, while display assertions
//! can keep their small, readable conversation fixtures.
use agent_protocol::models::ThreadResponse;
use agent_protocol::{
    conversation::{self as decision, records},
    execution::{SessionStatus, TurnStatus},
    sync::{Content, Cursor, Key, Message, Position, Scope, Window},
};
use std::sync::Arc;

pub fn catalog(list: agent_protocol::models::ThreadList, cursor: Cursor) -> records::Message {
    let mut rows = std::collections::BTreeMap::new();
    for thread in list.data {
        let Some(id) = thread.id.clone() else {
            continue;
        };
        let conversation = replica(
            ThreadResponse {
                thread,
                ..Default::default()
            },
            1,
        );
        let mut row = conversation.rows()[&Key::Thread(id)].clone();
        row.version = cursor.sequence;
        rows.insert(row.key.clone(), row);
    }
    for project in list.projects {
        let key = Key::Project(project.id.clone());
        rows.insert(
            key.clone(),
            records::Row {
                key,
                version: cursor.sequence,
                position: Position::Unordered,
                content: Content::Full(Arc::new(records::Record::Project(project))),
            },
        );
    }
    Message::Snapshot {
        cursor,
        window: None,
        rows: rows.into_values().collect(),
    }
}

pub fn replica(response: ThreadResponse, generation: u64) -> records::Replica {
    replica_at(response, generation, 7)
}

pub fn replica_at(response: ThreadResponse, generation: u64, sequence: u64) -> records::Replica {
    let thread = response.thread;
    let id = thread.id.clone().unwrap();
    let provider = thread
        .active_provider
        .or(id.provider)
        .expect("fixture declares the active provider");
    let cursor = Cursor {
        store: uuid::Uuid::from_u128(1),
        sequence,
    };
    let header = records::Header {
        state: decision::Thread {
            name: thread.name.clone().unwrap_or_default(),
            workspace: thread
                .cwd
                .clone()
                .map(decision::Workspace::Ready)
                .unwrap_or(decision::Workspace::Missing),
            provider,
            queue_held: thread.queue.held,
        },
        details: records::Details {
            parent: thread.parent_id,
            selection: records::Selection {
                model: response.model,
                effort: response.effort,
                service_tier: response.service_tier,
            },
            capabilities: records::capabilities(thread.capabilities.unwrap_or_default()),
            preview: thread.preview,
            updated_at: (thread.updated_at.unwrap_or_default() * 1000.0) as i64,
            project: thread.project_id.clone(),
            branch: thread.git_branch,
            has_changes: thread.has_changes,
            agent: thread.agent_id,
            can_accept_direct_input: thread.can_accept_direct_input,
            history_read_state: thread.history_read_state,
            activity: records::Activity {
                stale: thread.list_stale == Some(true),
                status: if thread
                    .turns
                    .iter()
                    .flatten()
                    .any(|turn| turn.status == TurnStatus::Running)
                {
                    SessionStatus::Running
                } else {
                    thread.status
                },
                background: thread.background,
                latest: thread.turns.iter().flatten().last().map(|turn| turn.status),
                completion: thread
                    .turns
                    .iter()
                    .flatten()
                    .last()
                    .filter(|turn| turn.status == TurnStatus::Completed)
                    .map(|turn| turn.id.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let mut rows = vec![records::Row {
        key: Key::Thread(id.clone()),
        version: cursor.sequence,
        position: Position::Unordered,
        content: Content::Full(Arc::new(records::Record::Thread(header))),
    }];
    let preparing_count = thread.preparations.len() as u64;
    for (index, input) in thread.queue.inputs.into_iter().enumerate() {
        rows.push(input_row(
            &id,
            input,
            preparing_count + index as u64,
            records::Delivery::Queued,
            cursor.sequence,
        ));
    }
    for (position, preparation) in thread.preparations.into_iter().enumerate() {
        use agent_protocol::session::PreparationStatus;
        let state = match preparation.status {
            PreparationStatus::Preparing => records::Delivery::Preparing(preparation.stage),
            PreparationStatus::Sending => records::Delivery::Delivering,
            PreparationStatus::Unknown => records::Delivery::Unknown { resolving: false },
            PreparationStatus::Failed { error } => records::Delivery::Failed(error),
            PreparationStatus::Interrupted => records::Delivery::Failed(Default::default()),
        };
        rows.push(input_row(
            &id,
            preparation.submission,
            position as u64,
            state,
            cursor.sequence,
        ));
    }
    for request in thread.requests.values() {
        let turn = match &request.target {
            agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => turn_id.clone(),
            _ => thread
                .turns
                .iter()
                .flatten()
                .last()
                .map(|turn| turn.id.clone())
                .unwrap_or_else(|| "pending".into()),
        };
        let record = records::Request {
            id: request.id.clone(),
            turn,
            body: request.body.clone(),
            state: match request.delivery {
                agent_protocol::session::RequestDelivery::Awaiting => {
                    records::AnswerState::Awaiting
                }
                agent_protocol::session::RequestDelivery::Sending
                | agent_protocol::session::RequestDelivery::Sent => records::AnswerState::Sending,
                agent_protocol::session::RequestDelivery::Unknown => records::AnswerState::Unknown,
            },
        };
        rows.push(records::Row {
            key: Key::Request {
                thread: id.clone(),
                id: request.id.clone(),
            },
            version: cursor.sequence,
            position: Position::Unordered,
            content: Content::Full(Arc::new(records::Record::Request(record))),
        });
    }
    let floor = thread.history_cursor.as_ref().map(|_| 1).unwrap_or(0);
    let mut position = 0;
    let mut item_ids = std::collections::BTreeSet::new();
    for (index, turn) in thread.turns.into_iter().flatten().enumerate() {
        let ordinal = index as u64 + 1;
        let record = records::Turn {
            id: turn.id.clone(),
            ordinal,
            provider: turn.provider.unwrap_or(provider),
            queued: turn.queued,
            started_at: turn
                .started_at_ms
                .map(|at| at as i64)
                .or_else(|| turn.started_at.map(|at| (at * 1000.0) as i64)),
            completed_at: turn.completed_at_ms.map(|at| at as i64),
            duration_ms: turn.duration_ms,
            error: turn.error.clone(),
            state: match turn.status {
                TurnStatus::Running => records::Execution::Running { stopping: false },
                TurnStatus::Completed => records::Execution::Completed,
                TurnStatus::Failed => {
                    records::Execution::Failed(turn.error.clone().unwrap_or_default())
                }
                TurnStatus::Unknown | TurnStatus::Interrupted => records::Execution::Interrupted,
            },
        };
        rows.push(records::Row {
            key: Key::Turn {
                thread: id.clone(),
                id: turn.id.clone(),
            },
            version: cursor.sequence,
            position: Position::Turn { ordinal },
            content: Content::Full(Arc::new(records::Record::Turn(record))),
        });
        let turn_items = turn.items.as_deref().unwrap_or_default();
        for (index, item) in turn_items.iter().enumerate() {
            if turn_items[index + 1..]
                .iter()
                .any(|later| later.id == item.id)
            {
                continue;
            }
            let mut item = item.clone();
            if matches!(
                item.body(),
                agent_protocol::items::ItemBody::UserMessage { .. }
            ) && let Some(input) = item.client_input_id.as_ref()
                && uuid::Uuid::parse_str(input.as_str()).is_ok()
            {
                let canonical = input.to_string();
                Arc::make_mut(&mut item).id = canonical.into();
            }
            if !item_ids.insert(item.id.clone()) {
                Arc::make_mut(&mut item).id = format!("{}:{}", turn.id, item.id).into();
            }
            position += 1;
            let content = Arc::new(records::Record::Item(records::Item {
                turn: turn.id.clone(),
                position,
                content: item.clone(),
            }));
            rows.push(records::Row {
                key: Key::Item {
                    thread: id.clone(),
                    id: item.id.clone(),
                },
                version: cursor.sequence,
                position: Position::Item {
                    turn: turn.id.clone(),
                    ordinal,
                    position,
                },
                content: if item.is_deferred() {
                    Content::Summary(content)
                } else {
                    Content::Full(content)
                },
            });
        }
    }
    records::Replica::new(Scope::Conversation(id), generation)
        .apply(
            generation,
            Message::Snapshot {
                cursor,
                window: Some(Window {
                    turn: floor,
                    item: 0,
                }),
                rows,
            },
        )
        .unwrap()
}

fn input_row(
    id: &agent_protocol::session::SessionRef,
    input: agent_protocol::operations::Submission,
    position: u64,
    state: records::Delivery,
    version: u64,
) -> records::Row {
    records::Row {
        key: Key::Input {
            thread: id.clone(),
            id: input.client_user_message_id.clone(),
        },
        version,
        position: Position::Unordered,
        content: Content::Full(Arc::new(records::Record::Input(records::Input {
            id: input.client_user_message_id,
            position,
            state,
            content: decision::InputContent {
                parts: input.input,
                model: input.model,
                effort: input.effort,
                service_tier: input.service_tier,
                follow_up: input.follow_up,
            },
        }))),
    }
}
