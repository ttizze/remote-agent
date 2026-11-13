//! A display projection. The replica remains the sole source of saved state.
use super::*;
use crate::{
    execution::TurnStatus,
    models, operations, requests, session,
    sync::{Content, Key},
};
use std::{collections::BTreeMap, sync::Arc};

pub fn project(replica: &Replica) -> Option<models::ThreadResponse> {
    let crate::sync::Scope::Conversation(id) = replica.scope() else {
        return None;
    };
    let mut header = None;
    let mut inputs = Vec::new();
    let mut turns = BTreeMap::new();
    let mut items = BTreeMap::<_, Vec<_>>::new();
    let mut requests = BTreeMap::new();
    for row in replica.rows().values() {
        let record = match &row.content {
            Content::Full(record) | Content::Summary(record) => record,
        };
        match record.as_ref() {
            Record::Thread(value) if row.key == Key::Thread(id.clone()) => header = Some(value),
            Record::Input(value) => inputs.push(value),
            Record::Turn(value) if value.state != Execution::RolledBack => {
                turns.insert(value.ordinal, value);
            }
            Record::Item(value) => items.entry(value.turn.clone()).or_default().push(value),
            Record::Request(value) => {
                requests.insert(
                    value.id.clone(),
                    Arc::new(requests::Request {
                        id: value.id.clone(),
                        target: requests::RequestTarget::Turn {
                            turn_id: value.turn.clone(),
                            item_id: None,
                        },
                        delivery: match value.state {
                            AnswerState::Awaiting => session::RequestDelivery::Awaiting,
                            AnswerState::Sending => session::RequestDelivery::Sending,
                            AnswerState::Unknown => session::RequestDelivery::Unknown,
                        },
                        body: value.body.clone(),
                    }),
                );
            }
            _ => {}
        }
    }
    let header = header?;
    inputs.sort_by_key(|input| input.position);
    let mut queue = operations::InputQueue {
        held: header.state.queue_held,
        ..Default::default()
    };
    let mut preparations = Vec::new();
    let mut submissions = BTreeMap::new();
    for input in inputs {
        let submission = operations::Submission {
            thread_id: id.clone(),
            client_user_message_id: input.id.clone(),
            input: input.content.parts.clone(),
            model: input.content.model.clone(),
            effort: input.content.effort.clone(),
            service_tier: input.content.service_tier.clone(),
            follow_up: input.content.follow_up,
        };
        let delivery = match &input.state {
            Delivery::Queued => {
                queue.inputs.push(submission);
                session::SubmissionDelivery::Accepted { turn_id: None }
            }
            state => {
                let (status, stage) = match state {
                    Delivery::Preparing(stage) => (session::PreparationStatus::Preparing, *stage),
                    Delivery::Delivering => (
                        session::PreparationStatus::Sending,
                        session::PreparationStage::Provider,
                    ),
                    Delivery::Unknown { .. } => (
                        session::PreparationStatus::Unknown,
                        session::PreparationStage::Provider,
                    ),
                    Delivery::Failed { stage, error } => (
                        error
                            .as_ref()
                            .map_or(session::PreparationStatus::Interrupted, |error| {
                                session::PreparationStatus::Failed {
                                    error: error.clone(),
                                }
                            }),
                        *stage,
                    ),
                    Delivery::Queued => unreachable!(),
                };
                preparations.push(session::SessionPreparation {
                    submission,
                    status,
                    stage,
                });
                match state {
                    Delivery::Unknown { .. } => session::SubmissionDelivery::Unknown,
                    Delivery::Failed { .. } => session::SubmissionDelivery::Rejected,
                    _ => session::SubmissionDelivery::Sending,
                }
            }
        };
        submissions.insert(input.id.clone(), delivery);
    }
    let turns = turns
        .into_values()
        .map(|turn| {
            let mut body = items.remove(&turn.id).unwrap_or_default();
            body.sort_by_key(|item| item.position);
            let body = body
                .into_iter()
                .map(|item| {
                    let value = &item.content;
                    if let Some(input) = &value.client_input_id {
                        submissions.insert(
                            input.clone(),
                            session::SubmissionDelivery::Accepted {
                                turn_id: Some(turn.id.clone()),
                            },
                        );
                    }
                    value.clone()
                })
                .collect();
            Arc::new(models::Turn {
                id: turn.id.clone(),
                provider: Some(turn.provider),
                supports_fork: Some(header.details.capabilities.fork),
                queued: turn.queued,
                status: match turn.state {
                    Execution::Running { .. } | Execution::Waiting(Wait::Retry { .. }) => {
                        TurnStatus::Running
                    }
                    Execution::Waiting(Wait::Background { .. }) | Execution::Completed => {
                        TurnStatus::Completed
                    }
                    Execution::Interrupted => TurnStatus::Interrupted,
                    Execution::Failed(_) => TurnStatus::Failed,
                    Execution::RolledBack => unreachable!(),
                },
                items: Some(body),
                started_at: turn.started_at.map(|at| at as f64 / 1000.0),
                started_at_ms: turn.started_at.and_then(|at| at.try_into().ok()),
                completed_at_ms: turn.completed_at.and_then(|at| at.try_into().ok()),
                duration_ms: turn.duration_ms,
                error: match &turn.state {
                    Execution::Failed(error) => Some(error.clone()),
                    _ => turn.error.clone(),
                }
                .filter(|error| !error.message.is_empty()),
            })
        })
        .collect();
    let floor = replica.window()?;
    Some(models::ThreadResponse {
        thread: models::Thread {
            turns: Some(turns),
            requests,
            submissions,
            queue,
            steerable: header.details.steerable,
            skips_compaction: header.details.skips_compaction,
            preparations,
            workspace_setup: header
                .details
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.visible.then(|| workspace.setup.clone()).flatten()),
            background: header.details.activity.background.clone(),
            history_has_more: Some(floor.turn != 0 || floor.item != 0),
            history_cursor: (floor.turn != 0 || floor.item != 0)
                .then(|| format!("{}:{}", floor.turn, floor.item)),
            ..summary(id, header)
        },
        model: header.details.selection.model.clone(),
        effort: header.details.selection.effort.clone(),
        service_tier: header.details.selection.service_tier.clone(),
    })
}

pub fn preview(text: &str) -> Option<String> {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(120).collect())
}

/// Native catalogs and saved headers expose the same compact list contract.
pub fn compact(thread: models::Thread) -> models::Thread {
    let name = thread
        .name
        .filter(|name| !name.trim().is_empty())
        .or_else(|| thread.preview.as_deref().and_then(preview));
    models::Thread {
        id: thread.id,
        active_provider: thread.active_provider,
        parent_id: thread.parent_id,
        name,
        cwd: thread.cwd,
        project_id: thread.project_id,
        updated_at: thread.updated_at,
        status: thread.status,
        capabilities: thread.capabilities,
        can_accept_direct_input: thread.can_accept_direct_input,
        has_changes: thread.has_changes,
        git_branch: thread.git_branch,
        list_stale: thread.list_stale,
        agent_id: thread.agent_id,
        history_read_state: thread.history_read_state,
        ..Default::default()
    }
}

pub fn unavailable(mut thread: models::Thread, issue: &str) -> models::Thread {
    thread.status = models::SessionStatus::Unavailable;
    thread.capabilities = Some(session::Capabilities {
        rename: true,
        fork: true,
        ..Default::default()
    });
    thread.history_read_state = Some(session::HistoryReadState::new(
        session::HistoryReadKind::Unavailable,
        vec![issue.to_owned()],
    ));
    thread
}

/// List metadata and conversation views come from the same committed header.
pub fn summary(id: &SessionRef, header: &Header) -> models::Thread {
    compact(models::Thread {
        id: Some(id.clone()),
        active_provider: Some(header.state.provider),
        list_stale: header.details.activity.stale.then_some(true),
        name: Some(header.state.name.clone()),
        cwd: header.state.workspace.cwd().map(str::to_owned),
        parent_id: header.details.parent.clone(),
        can_accept_direct_input: header.details.can_accept_direct_input,
        preview: header.details.preview.clone(),
        updated_at: Some(header.details.updated_at as f64 / 1000.0),
        capabilities: Some(header.details.capabilities),
        history_read_state: Some(
            header
                .details
                .history_read_state
                .clone()
                .unwrap_or_else(|| {
                    session::HistoryReadState::new(session::HistoryReadKind::Complete, vec![])
                }),
        ),
        git_branch: header.details.branch.clone(),
        has_changes: header.details.has_changes,
        project_id: header.details.project.clone(),
        agent_id: header.details.agent.clone(),
        status: header.details.activity.status(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversation::Workspace,
        session::ProviderKind,
        sync::{Cursor, Position, Scope, Window},
    };

    #[test]
    fn opening_validates_the_actual_rpc_identity_and_required_header() {
        for (wrong_session, expected) in [
            (true, "conversation identity changed"),
            (false, "conversation header is missing"),
        ] {
            use crate::operations::RpcMethod;
            let session =
                SessionRef::new(None, uuid::Uuid::from_u128(u128::MAX - 1).to_string()).unwrap();
            let response = Opened {
                session: if wrong_session {
                    SessionRef::new(None, uuid::Uuid::from_u128(u128::MAX - 2).to_string()).unwrap()
                } else {
                    session.clone()
                },
                subscription_id: uuid::Uuid::from_u128(u128::MAX - 3),
                initial: Message::Snapshot {
                    cursor: Cursor {
                        store: uuid::Uuid::from_u128(u128::MAX - 4),
                        sequence: 1,
                    },
                    window: Some(Window { turn: 0, item: 0 }),
                    rows: vec![],
                },
            };
            let request = Subscribe {
                session: session.clone(),
                subscription: uuid::Uuid::from_u128(u128::MAX - 5),
                cached: None,
                window: None,
            };
            assert_eq!(request.validate(&response), Err(expected));
            let create = Create {
                workspace: None,
                session,
                provider: ProviderKind::Codex,
                subscription: uuid::Uuid::from_u128(u128::MAX - 6),
                cwd: None,
                selection: Default::default(),
                initial: None,
            };
            assert_eq!(create.validate(&response), Err(expected));
        }
    }

    #[test]
    fn list_metadata_keeps_only_a_short_fallback_title_and_no_conversation_payload() {
        let native = models::Thread {
            name: Some("  ".into()),
            preview: Some(format!("\n {}\nprivate body", "界".repeat(140))),
            turns: Some(vec![Arc::new(models::Turn {
                id: "secret".into(),
                ..Default::default()
            })]),
            history_cursor: Some("private cursor".into()),
            history_has_more: Some(true),
            ..Default::default()
        };
        let metadata = compact(native);
        assert_eq!(metadata.name.as_deref(), Some("界".repeat(120).as_str()));
        assert!(metadata.preview.is_none());
        assert!(metadata.turns.is_none());
        assert!(metadata.history_cursor.is_none());
        assert!(metadata.history_has_more.is_none());
        assert!(metadata.requests.is_empty());
        let named = compact(models::Thread {
            name: Some("explicit title".into()),
            preview: Some("ignored".into()),
            ..Default::default()
        });
        assert_eq!(named.name.as_deref(), Some("explicit title"));
    }

    #[test]
    fn projection_keeps_every_unconfirmed_input_in_admission_order() {
        let id = SessionRef::new(None, uuid::Uuid::from_u128(u128::MAX - 7).to_string()).unwrap();
        let header = Header {
            state: Thread {
                name: "conversation".into(),
                workspace: Workspace::Ready("/project".into()),
                provider: ProviderKind::Claude,
                queue_held: true,
            },
            details: Default::default(),
        };
        let mut rows = vec![Row {
            key: Key::Thread(id.clone()),
            version: 1,
            position: Position::Unordered,
            content: Content::Full(Arc::new(Record::Thread(header))),
        }];
        for (name, position, state) in [
            (
                "stopped",
                4,
                Delivery::Failed {
                    stage: session::PreparationStage::Worktree,
                    error: None,
                },
            ),
            ("third", 3, Delivery::Queued),
            ("second", 2, Delivery::Unknown { resolving: false }),
            (
                "first",
                1,
                Delivery::Failed {
                    stage: session::PreparationStage::SetupScript,
                    error: Some(Default::default()),
                },
            ),
        ] {
            rows.push(Row {
                key: Key::Input {
                    thread: id.clone(),
                    id: name.into(),
                },
                version: 1,
                position: Position::Unordered,
                content: Content::Full(Arc::new(Record::Input(Input {
                    id: name.into(),
                    position,
                    state,
                    content: InputContent {
                        parts: vec![operations::Input::Text { text: name.into() }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                }))),
            });
        }
        let replica = Replica::new(Scope::Conversation(id), 1)
            .apply(
                1,
                Message::Snapshot {
                    cursor: Cursor {
                        store: uuid::Uuid::from_u128(u128::MAX - 8),
                        sequence: 1,
                    },
                    window: Some(Window { turn: 0, item: 0 }),
                    rows,
                },
            )
            .unwrap();
        let thread = project(&replica).unwrap().thread;
        assert_eq!(
            thread
                .preparations
                .iter()
                .map(|input| input.submission.client_user_message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "stopped"]
        );
        assert!(matches!(
            thread.preparations[0].status,
            session::PreparationStatus::Failed { .. }
        ));
        assert_eq!(
            thread.preparations[1].status,
            session::PreparationStatus::Unknown
        );
        assert_eq!(
            thread.preparations[0].stage,
            session::PreparationStage::SetupScript
        );
        assert_eq!(
            thread.preparations[2].stage,
            session::PreparationStage::Worktree
        );
        assert_eq!(
            thread.preparations[2].status,
            session::PreparationStatus::Interrupted
        );
        assert_eq!(
            thread.queue.inputs[0].client_user_message_id.as_str(),
            "third"
        );
        assert_eq!(thread.submissions.len(), 4);
    }
}
