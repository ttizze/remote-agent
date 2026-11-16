//! Disposable native display DTOs built directly from the canonical projection.
//! Grouping never invents persisted runs or changes the inherited row order.
use super::*;
use agent_protocol::{execution as display, items, models, operations as op, requests, session};

pub fn project(
    snapshot: &ThreadSnapshot,
    identity: &session::SessionRef,
    capability_provider: session::ProviderKind,
    capabilities: session::Capabilities,
    project_root: Option<&str>,
    details: &BTreeMap<(String, String), Arc<model::TurnItem>>,
) -> Result<models::ThreadResponse, String> {
    let projection = &snapshot.projection;
    let selected =
        session::ProviderKind::from_instance_id(&projection.thread.provider_instance_id)?;
    let mut capabilities = session::Capabilities {
        steer: selected == capability_provider && capabilities.steer,
        model_change: selected == capability_provider && capabilities.model_change,
        rollback: selected == capability_provider && capabilities.rollback,
        ..capabilities
    };
    let native = projection
        .thread
        .active_provider_thread_id
        .as_ref()
        .and_then(|id| {
            projection.provider_threads.iter().find(|thread| {
                &thread.id == id
                    && thread.provider_instance_id == projection.thread.provider_instance_id
            })
        });
    let native_session = native
        .and_then(|thread| thread.provider_session_id.as_ref())
        .and_then(|id| {
            projection.provider_sessions.iter().find(|session| {
                &session.id == id
                    && session.provider_instance_id == projection.thread.provider_instance_id
            })
        });
    if let Some(session) = native_session {
        capabilities.rollback = session.capabilities.can_rollback_conversation();
        capabilities.steer = session.capabilities.turns.supports_active_steering;
        capabilities.model_change = session
            .capabilities
            .sessions
            .supports_model_switch_in_session;
    }
    let provider = selected;
    let cwd = projection
        .thread
        .worktree_path
        .as_deref()
        .or(project_root)
        .or_else(|| {
            projection
                .thread
                .workspace_launch
                .as_ref()
                .map(|launch| launch.project_cwd.as_str())
        });
    if identity.application_id() != projection.thread.id {
        return Err("conversation identity changed".into());
    }
    let id = identity.clone();
    let rewindable = native
        .filter(|_| capabilities.rollback && native_session.is_some())
        .map(|native| rewindable_runs(projection, native, snapshot.has_more_history))
        .unwrap_or_default();
    let turns = timeline(
        projection,
        provider,
        details,
        capabilities.fork,
        &rewindable,
    )?;
    let mut queue = op::InputQueue::default();
    let mut preparations = Vec::new();
    let mut submissions = BTreeMap::new();
    let mut runs: Vec<_> = projection.runs.iter().collect();
    runs.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    for run in runs {
        let Some(item)=projection.turn_items.iter().find(|item|matches!(&item.body,model::TurnItemBody::UserMessage {message_id,..} if message_id==&run.user_message_id)) else {continue;};
        let submission = submission(&id, run, item, projection, provider)?;
        let uncertain = delivery_uncertain(projection, &item.id);
        let delivery = match run.status {
            model::RunStatus::Queued => {
                queue.held |= run.queue_held;
                queue.inputs.push(submission);
                session::SubmissionDelivery::Accepted { turn_id: None }
            }
            model::RunStatus::Preparing | model::RunStatus::Starting => {
                preparations.push(session::SessionPreparation {
                    submission,
                    status: if uncertain {
                        session::PreparationStatus::Unknown
                    } else if run.status == model::RunStatus::Preparing {
                        session::PreparationStatus::Preparing
                    } else {
                        session::PreparationStatus::Sending
                    },
                    stage: preparation_stage(projection, run),
                });
                if uncertain {
                    session::SubmissionDelivery::Unknown
                } else {
                    session::SubmissionDelivery::Sending
                }
            }
            model::RunStatus::Failed | model::RunStatus::Cancelled
                if item.status != model::TurnItemStatus::Cancelled
                    && projection
                        .attempts
                        .iter()
                        .find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref())
                        .is_some_and(|attempt| {
                            attempt.provider_turn_id.is_none() && attempt.started_at.is_none()
                        })
                    && !projection.provider_turns.iter().any(|turn| {
                        projection.attempts.iter().any(|attempt| {
                            attempt.run_id == run.id
                                && turn.run_attempt_id.as_ref() == Some(&attempt.id)
                        })
                    }) =>
            {
                let failure = projection.turn_items.iter().find_map(|item| {
                    if item.run_id.as_ref() != Some(&run.id)
                        || item.status != model::TurnItemStatus::Failed
                    {
                        return None;
                    }
                    match &item.body {
                        model::TurnItemBody::Error { failure, .. } => Some(failure_view(failure)),
                        _ => None,
                    }
                });
                preparations.push(session::SessionPreparation {
                    submission,
                    status: if run.status == model::RunStatus::Cancelled {
                        session::PreparationStatus::Interrupted
                    } else {
                        session::PreparationStatus::Failed {
                            error: failure.unwrap_or_default(),
                        }
                    },
                    stage: preparation_stage(projection, run),
                });
                session::SubmissionDelivery::Accepted { turn_id: None }
            }
            _ => session::SubmissionDelivery::Accepted {
                turn_id: Some(run.id.clone().into()),
            },
        };
        submissions.insert(run.user_message_id.clone().into(), delivery);
    }
    for item in &projection.turn_items {
        let model::TurnItemBody::UserMessage {
            message_id,
            input_intent,
            ..
        } = &item.body
        else {
            continue;
        };
        if input_intent != "steer" || !delivery_uncertain(projection, &item.id) {
            continue;
        }
        let run = projection
            .runs
            .iter()
            .find(|run| Some(&run.id) == item.run_id.as_ref())
            .ok_or("unconfirmed input's run is missing")?;
        preparations.push(session::SessionPreparation {
            submission: submission(&id, run, item, projection, provider)?,
            status: session::PreparationStatus::Unknown,
            stage: session::PreparationStage::Provider,
        });
        submissions.insert(
            message_id.clone().into(),
            session::SubmissionDelivery::Unknown,
        );
    }
    let active = projection
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                model::RunStatus::Preparing
                    | model::RunStatus::Starting
                    | model::RunStatus::Running
                    | model::RunStatus::Waiting
            )
        })
        .max_by_key(|run| run.ordinal);
    let native_status = projection
        .thread
        .active_provider_thread_id
        .as_deref()
        .and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|thread| thread.id == id)
        })
        .filter(|thread| {
            thread.app_thread_id.as_deref() == Some(&projection.thread.id)
                && thread.provider_instance_id == projection.thread.provider_instance_id
                && thread.driver == provider.driver()
        })
        .map(|thread| thread.status);
    let background = model::shell::background_tasks(
        projection
            .provider_threads
            .iter()
            .flat_map(|thread| &thread.pending_background_tasks),
    );
    let selection = &projection.thread.model_selection;
    Ok(models::ThreadResponse {
        thread: models::Thread {
            id: Some(id),
            active_provider: Some(provider),
            name: Some(projection.thread.title.clone()),
            cwd: cwd.map(str::to_owned),
            git_branch: projection.thread.branch.clone(),
            parent_id: if model::shell::is_listed_lineage(
                projection.thread.lineage.relationship_to_parent.as_deref(),
                projection.thread.lineage.side_chat,
            ) {
                None
            } else {
                projection
                    .thread
                    .lineage
                    .parent_thread_id
                    .as_deref()
                    .map(model::shell::public_session_ref)
                    .transpose()?
            },
            project_id: projection
                .thread
                .project_id
                .clone()
                .map(models::ProjectMembership::Assigned)
                .unwrap_or(models::ProjectMembership::Unassigned {}),
            can_accept_direct_input: Some(!projection.thread.is_provider_subagent()),
            capabilities: Some(capabilities),
            steerable: capabilities.steer && active.is_some(),
            status: agent_protocol::orchestration::shell::task_status(
                active.map(|run| run.status),
                native_status,
            ),
            updated_at: millis(Some(&projection.updated_at)).map(|at| at as f64 / 1000.0),
            pull_requests: projection
                .thread
                .pull_requests
                .iter()
                .map(agent_protocol::pull_requests::ThreadPullRequestLink::pull_request)
                .collect(),
            turns: Some(turns),
            background,
            queue,
            preparations,
            submissions,
            requests: request_views(projection)?,
            workspace_setup: projection
                .thread
                .workspace_launch
                .as_ref()
                .filter(|workspace| workspace.visible)
                .and_then(|workspace| workspace.setup.clone()),
            history_has_more: Some(snapshot.has_more_history),
            history_cursor: snapshot.history_cursor.clone(),
            history_read_state: Some(session::HistoryReadState::new(
                session::HistoryReadKind::Complete,
                Vec::new(),
            )),
            ..Default::default()
        },
        model: Some(models::ModelRef {
            provider,
            id: selection.model.clone(),
        }),
        effort: selection
            .text_option(if provider == session::ProviderKind::Claude {
                "effort"
            } else {
                "reasoningEffort"
            })
            .map(str::to_owned),
        service_tier: selection.text_option("serviceTier").map(str::to_owned),
    })
}

fn preparation_stage(projection: &ThreadProjection, run: &model::Run) -> session::PreparationStage {
    use agent_protocol::workspace::{SetupStageId, SetupStageStatus};
    if run.workspace_preparation.is_none() || run.status == model::RunStatus::Starting {
        return session::PreparationStage::Provider;
    }
    let Some(setup) = run
        .workspace_preparation
        .as_ref()
        .and(
            projection
                .thread
                .workspace_launch
                .as_ref()
                .filter(|workspace| workspace.run_id.as_ref() == Some(&run.id)),
        )
        .and_then(|workspace| workspace.setup.as_ref())
    else {
        return session::PreparationStage::Workspace;
    };
    for (id, stage) in [
        (SetupStageId::Agent, session::PreparationStage::Provider),
        (
            SetupStageId::SetupScript,
            session::PreparationStage::SetupScript,
        ),
        (SetupStageId::Checkout, session::PreparationStage::Worktree),
    ] {
        if setup
            .stages
            .iter()
            .any(|saved| saved.id == id && saved.status != SetupStageStatus::Pending)
        {
            return stage;
        }
    }
    session::PreparationStage::Worktree
}

fn millis(value: Option<&str>) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value?)
        .ok()?
        .timestamp_millis()
        .try_into()
        .ok()
}

fn driver_kind(driver: &str) -> Result<session::ProviderKind, String> {
    match driver {
        "codex" => Ok(session::ProviderKind::Codex),
        "claudeAgent" => Ok(session::ProviderKind::Claude),
        "dsh" => Ok(session::ProviderKind::DeepSeek),
        _ => Err("unsupported provider driver".into()),
    }
}

fn timeline_groups(items: &[ProjectedTurnItem]) -> Vec<Vec<&ProjectedTurnItem>> {
    let mut groups: Vec<Vec<&ProjectedTurnItem>> = Vec::new();
    let mut previous = None;
    for row in items {
        let key = (
            row.source_thread_id.as_str(),
            row.item
                .run_id
                .as_deref()
                .or(row.item.provider_turn_id.as_deref()),
        );
        let imported_input = row.item.run_id.is_none()
            && row.item.provider_turn_id.is_none()
            && matches!(row.item.body, model::TurnItemBody::UserMessage { .. });
        if previous != Some(key) || imported_input {
            groups.push(Vec::new());
        }
        groups
            .last_mut()
            .expect("row allocated its group")
            .push(row);
        previous = Some(key);
    }
    groups
}

fn group_id(row: &ProjectedTurnItem) -> String {
    row.item
        .run_id
        .clone()
        .or_else(|| row.item.provider_turn_id.clone())
        .unwrap_or_else(|| format!("history:{}:{}", row.source_thread_id, row.source_item_id))
}

pub(crate) fn fork_point(
    projection: &ThreadProjection,
    turn: &agent_protocol::ids::TurnId,
) -> Option<(
    session::SessionRef,
    agent_protocol::conversation::records::ForkPoint,
)> {
    use agent_protocol::conversation::records::ForkPoint;
    let group = timeline_groups(&projection.visible_turn_items)
        .into_iter()
        .find(|group| group_id(group[0]) == turn.as_str())?;
    let first = group[0];
    if first.visibility == Visibility::Synthetic {
        return None;
    }
    let source = model::shell::public_session_ref(&first.source_thread_id).ok()?;
    if let Some(run_id) = &first.item.run_id {
        return Some((
            source,
            ForkPoint::Run {
                run_id: run_id.clone(),
            },
        ));
    }
    if let Some(provider_turn_id) = &first.item.provider_turn_id {
        return Some((
            source,
            ForkPoint::ProviderTurn {
                provider_turn_id: provider_turn_id.clone(),
            },
        ));
    }
    (first.visibility == Visibility::Inherited
        || projection.thread.history_origin == Some(model::ThreadHistoryOrigin::V1Import))
    .then(|| {
        (
            source,
            ForkPoint::TurnItem {
                turn_item_id: group.last().expect("nonempty group").source_item_id.clone(),
            },
        )
    })
}

// The display sees a partial history window. As in T3 ChatView.logic,
// only a local app run with an available retained ordinal/checkpoint can be
// offered. The Host separately validates the complete state before native I/O.
fn rewindable_runs(
    projection: &ThreadProjection,
    native: &model::ProviderThread,
    has_more_history: bool,
) -> BTreeSet<String> {
    if native.provider_instance_id != projection.thread.model_selection.instance_id
        || native.app_thread_id.as_deref() != Some(projection.thread.id.as_str())
        || native
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_deref())
            .is_none_or(str::is_empty)
    {
        return BTreeSet::new();
    }
    let mut available = BTreeSet::from([0]);
    let mut latest = BTreeMap::<&str, &model::ProviderTurn>::new();
    for turn in &projection.provider_turns {
        if let Some(attempt) = turn.run_attempt_id.as_deref() {
            let slot = latest.entry(attempt).or_insert(turn);
            if turn.ordinal > slot.ordinal {
                *slot = turn;
            }
        }
    }
    let mut runs: Vec<_> = projection
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                model::RunStatus::Completed
                    | model::RunStatus::Failed
                    | model::RunStatus::Interrupted
                    | model::RunStatus::Cancelled
            )
        })
        .collect();
    runs.sort_by_key(|run| run.ordinal);
    let attempts: BTreeMap<_, _> = projection
        .attempts
        .iter()
        .map(|attempt| (attempt.id.as_str(), attempt))
        .collect();
    let by_id: BTreeMap<_, _> = projection
        .provider_turns
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect();
    for run in &runs {
        let attempt = run
            .active_attempt_id
            .as_deref()
            .and_then(|id| attempts.get(id).copied());
        let turn = run
            .active_attempt_id
            .as_deref()
            .and_then(|attempt| latest.get(attempt).copied())
            .or_else(|| {
                attempt
                    .and_then(|attempt| attempt.provider_turn_id.as_deref())
                    .and_then(|id| by_id.get(id).copied())
            });
        if model::retained_rollback_boundary(run, attempt, native, turn).is_ok() {
            available.insert(run.ordinal);
        }
    }
    let scopes: BTreeSet<_> = projection
        .checkpoint_scopes
        .iter()
        .filter(|scope| scope.thread_id == projection.thread.id)
        .map(|scope| scope.id.as_str())
        .collect();
    let checkpoints: BTreeSet<_> = projection
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.thread_id == projection.thread.id
                && scopes.contains(checkpoint.scope_id.as_str())
                && checkpoint.is_conversation_rewind_ready()
        })
        .map(|checkpoint| {
            (
                checkpoint.scope_id.as_str(),
                checkpoint.app_run_ordinal.unwrap_or(0),
            )
        })
        .collect();
    let roots: BTreeMap<_, _> = projection
        .nodes
        .iter()
        .filter(|node| node.thread_id == projection.thread.id)
        .filter_map(|node| {
            node.checkpoint_scope_id
                .as_deref()
                .map(|scope| (node.id.as_str(), scope))
        })
        .collect();
    let mut previous = 0;
    let mut result = BTreeSet::new();
    for run in runs {
        let retained = previous;
        previous = run.ordinal;
        if retained == 0 && has_more_history {
            continue;
        }
        if available.contains(&retained)
            && run
                .root_node_id
                .as_deref()
                .and_then(|root| roots.get(root))
                .is_some_and(|scope| checkpoints.contains(&(*scope, retained)))
        {
            result.insert(run.id.clone());
        }
    }
    result
}

fn timeline(
    projection: &ThreadProjection,
    provider: session::ProviderKind,
    details: &BTreeMap<(String, String), Arc<model::TurnItem>>,
    can_fork: bool,
    rewindable: &BTreeSet<String>,
) -> Result<Vec<Arc<models::Turn>>, String> {
    let mut turns: Vec<Arc<models::Turn>> = Vec::new();
    let native_by_id = projection
        .provider_turns
        .iter()
        .map(|turn| (turn.id.as_str(), turn))
        .collect::<BTreeMap<_, _>>();
    for group in timeline_groups(&projection.visible_turn_items) {
        let row = group[0];
        let local = row.visibility == Visibility::Local;
        let run = local
            .then(|| {
                projection
                    .runs
                    .iter()
                    .find(|run| Some(run.id.as_str()) == row.item.run_id.as_deref())
            })
            .flatten();
        let native_turns = group
            .iter()
            .filter_map(|row| row.item.provider_turn_id.as_deref())
            .filter_map(|id| native_by_id.get(id).copied())
            .collect::<Vec<_>>();
        let native_turn = native_turns.iter().min_by_key(|turn| turn.ordinal).copied();
        let started_at = run
            .and_then(|run| run.started_at.as_deref())
            .or_else(|| native_turn.and_then(|turn| turn.started_at.as_deref()));
        let completed_at = run.and_then(|run| run.completed_at.as_deref()).or_else(|| {
            // A successful app run settles after its root checkpoint. Other
            // late items (images, commands and child results) do not extend it.
            let checkpoint = native_turns
                .iter()
                .all(|turn| turn.status == model::ProviderTurnStatus::Completed)
                .then(|| {
                    group
                        .iter()
                        .filter_map(|row| {
                            matches!(row.item.body, model::TurnItemBody::Checkpoint { .. })
                                .then_some(row.item.completed_at.as_deref())
                                .flatten()
                        })
                        .max()
                })
                .flatten();
            native_turns
                .iter()
                .filter_map(|turn| turn.completed_at.as_deref())
                .chain(checkpoint)
                .max()
        });
        turns.push(Arc::new(models::Turn {
            id: group_id(row).into(),
            provider: if local {
                Some(
                    row.item
                        .provider_thread_id
                        .as_ref()
                        .and_then(|id| {
                            projection
                                .provider_threads
                                .iter()
                                .find(|thread| &thread.id == id)
                        })
                        .map(|thread| driver_kind(&thread.driver))
                        .transpose()?
                        .unwrap_or(provider),
                )
            } else {
                native_turn
                    .and_then(|turn| turn.native_turn_ref.as_ref())
                    .and_then(|reference| driver_kind(&reference.driver).ok())
                    .or_else(|| {
                        row.item
                            .native_item_ref
                            .as_ref()
                            .and_then(|reference| driver_kind(&reference.driver).ok())
                    })
            },
            supports_rewind: Some(
                local
                    && row.source_thread_id == projection.thread.id
                    && run.is_some_and(|run| rewindable.contains(&run.id)),
            ),
            supports_fork: Some(
                row.visibility == Visibility::Inherited
                    || (local
                        && can_fork
                        && (run.is_some()
                            || native_turn.is_some()
                            || projection.thread.history_origin
                                == Some(model::ThreadHistoryOrigin::V1Import))),
            ),
            status: run
                .map(|run| run_status(run.status))
                .or_else(|| {
                    native_turns
                        .iter()
                        .max_by_key(|turn| turn.ordinal)
                        .map(|turn| match turn.status {
                            model::ProviderTurnStatus::Pending
                            | model::ProviderTurnStatus::Running => display::TurnStatus::Running,
                            model::ProviderTurnStatus::Completed => display::TurnStatus::Completed,
                            model::ProviderTurnStatus::Failed => display::TurnStatus::Failed,
                            model::ProviderTurnStatus::Interrupted
                            | model::ProviderTurnStatus::Cancelled => {
                                display::TurnStatus::Interrupted
                            }
                        })
                })
                .unwrap_or(display::TurnStatus::Completed),
            attempts: run.map_or_else(Vec::new, |run| run_attempts(projection, &run.id)),
            fresh_context: run.is_some_and(|run| run.fresh_context),
            items: Some(Vec::new()),
            started_at: millis(started_at).map(|at| at as f64 / 1000.0),
            started_at_ms: millis(started_at),
            completed_at_ms: millis(completed_at),
            ..Default::default()
        }));
        for row in group {
            let turn = Arc::make_mut(turns.last_mut().expect("row allocated its display group"));
            if matches!(&row.item.body,model::TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
            {
                turn.queued = true;
            }
            if row.visibility != Visibility::Local
                && native_turns.is_empty()
                && matches!(
                    row.item.status,
                    model::TurnItemStatus::Running | model::TurnItemStatus::Waiting
                )
            {
                turn.status = display::TurnStatus::Running;
            }
            if let model::TurnItemBody::Error { failure, .. } = &row.item.body
                && row.item.status == model::TurnItemStatus::Failed
                && failure.class != "delivery_uncertain"
            {
                turn.error = Some(failure_view(failure));
            }
            let detail = details
                .get(&(row.source_thread_id.clone(), row.source_item_id.clone()))
                .filter(|detail| same_detail_revision(&row.item, detail));
            let attempt =
                item_attempt(projection, &row.item).map(|attempt| attempt_id(&attempt.id));
            let author = author(projection, &row.item);
            turn.items.as_mut().unwrap().extend(
                item_view(
                    detail.map_or(row.item.as_ref(), |detail| detail.as_ref()),
                    provider,
                    detail.is_some(),
                    match &row.item.body {
                        model::TurnItemBody::Subagent { subagent_id, .. } => projection
                            .subagents
                            .iter()
                            .find(|task| &task.id == subagent_id)
                            .and_then(|task| task.model.as_deref()),
                        _ => None,
                    },
                )?
                .into_iter()
                .map(|mut item| {
                    let saved = Arc::make_mut(&mut item);
                    saved.attempt = attempt;
                    saved.author = author.clone();
                    item
                }),
            );
        }
    }
    turns.retain(|turn| {
        turn.items.as_ref().is_some_and(|items| !items.is_empty()) || turn.error.is_some()
    });
    Ok(turns)
}

/// A user message's sender other than the local user, from T3 createdBy and
/// scheduledTaskId. A provider subagent's prompt comes from its parent's agent.
fn author(
    projection: &ThreadProjection,
    item: &model::TurnItem,
) -> Option<Box<agent_protocol::attribution::Author>> {
    use agent_protocol::attribution::Author;
    let model::TurnItemBody::UserMessage {
        created_by,
        scheduled_task_id,
        ..
    } = &item.body
    else {
        return None;
    };
    if let Some(id) = scheduled_task_id
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
    {
        return Some(Box::new(Author::Automation { id }));
    }
    match created_by {
        model::Actor::User => None,
        model::Actor::System => Some(Box::new(Author::System)),
        model::Actor::Agent => projection
            .thread
            .lineage
            .parent_thread_id
            .as_deref()
            .and_then(|parent| model::shell::public_session_ref(parent).ok())
            .map(|session| Box::new(Author::Agent { session })),
    }
}

/// Client identity of a T3 RunAttempt id.
fn attempt_id(id: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(id)
        .unwrap_or_else(|_| uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, id.as_bytes()))
}

/// T3 resolveAttempt: an item belongs to the attempt rooted on its node chain.
fn item_attempt<'a>(
    projection: &'a ThreadProjection,
    item: &model::TurnItem,
) -> Option<&'a model::RunAttempt> {
    let run = item.run_id.as_deref()?;
    let rooted = |node: &str| {
        projection
            .attempts
            .iter()
            .find(|attempt| attempt.root_node_id == node && attempt.run_id == run)
    };
    let mut visited = BTreeSet::new();
    let mut node = item.node_id.as_deref();
    while let Some(id) = node.filter(|id| visited.insert(*id)) {
        if let Some(attempt) = rooted(id) {
            return Some(attempt);
        }
        let current = projection.nodes.iter().find(|node| node.id == id)?;
        if let Some(attempt) = rooted(&current.root_node_id) {
            return Some(attempt);
        }
        node = current.parent_node_id.as_deref();
    }
    None
}

/// A run's T3 attempts in order. A steering restart cuts off its predecessor.
fn run_attempts(projection: &ThreadProjection, run: &str) -> Vec<models::ExecutionAttempt> {
    let mut attempts: Vec<_> = projection
        .attempts
        .iter()
        .filter(|attempt| attempt.run_id == run)
        .collect();
    attempts.sort_by_key(|attempt| attempt.attempt_ordinal);
    attempts
        .iter()
        .enumerate()
        .map(|(index, attempt)| {
            use model::RunAttemptStatus as Status;
            models::ExecutionAttempt {
                id: attempt_id(&attempt.id),
                state: match attempt.status {
                    Status::Pending | Status::Running => models::AttemptState::Running,
                    Status::Completed => models::AttemptState::Completed,
                    Status::Interrupted | Status::Cancelled => models::AttemptState::Interrupted,
                    Status::Failed => models::AttemptState::Failed,
                    Status::Superseded => models::AttemptState::Superseded {
                        by_steer: attempts.get(index + 1).is_some_and(|next| {
                            next.reason == model::AttemptReason::SteeringRestart
                        }),
                    },
                },
            }
        })
        .collect()
}

fn run_status(status: model::RunStatus) -> display::TurnStatus {
    match status {
        model::RunStatus::Preparing
        | model::RunStatus::Queued
        | model::RunStatus::Starting
        | model::RunStatus::Running
        | model::RunStatus::Waiting => display::TurnStatus::Running,
        model::RunStatus::Failed => display::TurnStatus::Failed,
        model::RunStatus::Interrupted | model::RunStatus::Cancelled => {
            display::TurnStatus::Interrupted
        }
        model::RunStatus::Completed | model::RunStatus::RolledBack => {
            display::TurnStatus::Completed
        }
    }
}

fn failure_view(failure: &model::ProviderFailure) -> display::ExecutionError {
    display::ExecutionError {
        message: failure.message.clone(),
        provider_code: failure.code.clone(),
        category: match failure.class.as_str() {
            "usage_limit" => display::ErrorCategory::UsageLimit,
            "transport_error" => display::ErrorCategory::Network,
            "permission_error" => display::ErrorCategory::Policy,
            "validation_error" => display::ErrorCategory::InvalidInput,
            _ => display::ErrorCategory::Other,
        },
        ..Default::default()
    }
}

pub(crate) fn attachments(
    attachments: &[serde_json::Value],
) -> Result<Vec<crate::state::Attachment>, String> {
    attachments
        .iter()
        .map(|attachment| {
            let id = attachment
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("attachment ID is missing")?;
            let name = attachment
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("attachment name is missing")?;
            let image = attachment.get("type").and_then(serde_json::Value::as_str) == Some("image");
            let extension = agent_protocol::paths::attachment_extension(
                name,
                image
                    .then(|| {
                        attachment
                            .get("mimeType")
                            .and_then(serde_json::Value::as_str)
                    })
                    .flatten(),
            );
            let source = format!(
                "{}{id}.{extension}",
                agent_protocol::paths::ATTACHMENT_SCHEME
            );
            Ok(crate::state::Attachment {
                name: name.into(),
                path: source,
                is_image: image,
            })
        })
        .collect()
}

pub(crate) fn attachment_parts(
    values: &[serde_json::Value],
) -> Result<Vec<items::MessagePart>, String> {
    Ok(attachments(values)?
        .into_iter()
        .map(|attachment| {
            if attachment.is_image {
                items::MessagePart::Image {
                    source: attachment.path,
                }
            } else {
                items::MessagePart::Attachment {
                    name: attachment.name,
                    path: attachment.path,
                }
            }
        })
        .collect())
}

/// Stored T3 context records as message parts; undecodable records are dropped.
pub(crate) fn context_parts(context: Option<&serde_json::Value>) -> Vec<items::MessagePart> {
    use agent_protocol::context::StoredContext;
    agent_protocol::context::stored_contexts(context)
        .into_iter()
        .map(|record| match record {
            StoredContext::Mention { path, label } => {
                items::MessagePart::Attachment { name: label, path }
            }
            // T3 stores skills by name. The native executor resolves their current catalog path.
            StoredContext::Skill { name } => items::MessagePart::Invocation {
                name,
                path: String::new(),
            },
            StoredContext::Context(context) => items::MessagePart::Context { context },
        })
        .collect()
}

fn delivery_uncertain(projection: &ThreadProjection, item_id: &str) -> bool {
    projection.turn_items.iter().any(|error| {
        error.parent_item_id.as_deref()==Some(item_id) && error.status==model::TurnItemStatus::Failed
            && matches!(&error.body,model::TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")
    })
}

fn submission(
    id: &session::SessionRef,
    run: &model::Run,
    item: &model::TurnItem,
    projection: &ThreadProjection,
    fallback: session::ProviderKind,
) -> Result<op::Submission, String> {
    let model::TurnItemBody::UserMessage {
        message_id,
        text,
        attachments,
        context,
        ..
    } = &item.body
    else {
        return Err("run input is not a user message".into());
    };
    let provider = run
        .provider_thread_id
        .as_ref()
        .and_then(|id| {
            projection
                .provider_threads
                .iter()
                .find(|thread| &thread.id == id)
        })
        .map(|thread| driver_kind(&thread.driver))
        .transpose()?
        .unwrap_or(fallback);
    let mut input = vec![op::Input::Text { text: text.clone() }];
    for part in attachment_parts(attachments)? {
        input.push(match part {
            items::MessagePart::Image { source } => op::Input::LocalImage { path: source },
            items::MessagePart::Attachment { path, name } => op::Input::Mention { path, name },
            _ => unreachable!(),
        });
    }
    for part in context_parts(context.as_ref()) {
        input.push(match part {
            items::MessagePart::Attachment { name, path } => op::Input::Mention { name, path },
            items::MessagePart::Invocation { name, path } => op::Input::Skill { name, path },
            items::MessagePart::Context { context } => op::Input::Context { context },
            _ => unreachable!("context records are mentions, skills or contexts"),
        });
    }
    Ok(op::Submission {
        fresh_context: run.fresh_context,
        thread_id: id.clone(),
        client_user_message_id: message_id.clone().into(),
        input,
        model: Some(models::ModelRef {
            provider,
            id: run.model_selection.model.clone(),
        }),
        effort: run
            .model_selection
            .text_option(if provider == session::ProviderKind::Claude {
                "effort"
            } else {
                "reasoningEffort"
            })
            .map(str::to_owned),
        service_tier: run
            .model_selection
            .text_option("serviceTier")
            .map(str::to_owned),
        follow_up: op::FollowUp::Queue,
    })
}

fn request_views(
    projection: &ThreadProjection,
) -> Result<BTreeMap<agent_protocol::ids::RequestId, Arc<requests::Request>>, String> {
    projection.runtime_requests.iter().filter(|request|request.status==model::RequestStatus::Pending).map(|request|{
        let item=projection.turn_items.iter().find(|item|item.node_id.as_ref()==Some(&request.node_id) && (
            matches!(&item.body,model::TurnItemBody::UserInputRequest {request_id,..}|model::TurnItemBody::ApprovalRequest {request_id,..} if request_id==&request.id)
            || matches!(&item.body,model::TurnItemBody::DynamicTool {..}) && matches!(request.kind.as_str(),"dynamic_tool_call"|"mcp-elicitation"))).ok_or("pending request card is missing")?;
        let body=match &item.body {
            model::TurnItemBody::UserInputRequest {questions,..}=>requests::RequestBody::Question {questions:questions.iter().map(|question|requests::Question {
                id:question.id.clone(),header:question.header.clone(),prompt:question.question.clone(),secret:question.is_secret.unwrap_or(false),
                allow_free_text:question.allow_custom_answer.unwrap_or(true),multiple:question.multi_select.unwrap_or(false),
                choices:question.options.iter().map(|option|requests::QuestionChoice {id:option.value.clone().unwrap_or_else(||option.label.clone()),label:option.label.clone(),description:option.description.clone()}).collect(),
            }).collect()},
            model::TurnItemBody::ApprovalRequest {prompt,reason,options,..}=> {
                let description=prompt.clone().unwrap_or_else(||"Approval required".into());
                let details=reason.clone().unwrap_or_default();
                let choices=options.iter().map(|option|requests::Choice {id:serde_json::to_value(option.decision).unwrap().as_str().unwrap().into(),label:option.label.clone(),description:option.warning.clone().unwrap_or_default(),
                    effect:match option.decision {model::ApprovalDecision::Accept=>requests::ChoiceEffect::Allow,model::ApprovalDecision::AcceptForSession|model::ApprovalDecision::AcceptAlways=>requests::ChoiceEffect::AllowLasting,_=>requests::ChoiceEffect::Deny}}).collect();
                if request.kind=="permission" {requests::RequestBody::Permission {description,details,choices}}
                else {requests::RequestBody::Approval {kind:match request.kind.as_str() {"file-change"=>requests::ApprovalKind::FileChange,"file-read"=>requests::ApprovalKind::Tool,_=>requests::ApprovalKind::Command},description,details,choices}}
            },
            model::TurnItemBody::DynamicTool {tool_name,input,..} if request.kind=="dynamic_tool_call"=>requests::RequestBody::ToolExecution {tool:tool_name.clone().unwrap_or_else(||"tool".into()),namespace:None,arguments:input.clone()},
            model::TurnItemBody::DynamicTool {input,..}=>requests::RequestBody::Elicitation {
                server:input.get("server").and_then(serde_json::Value::as_str).ok_or("elicitation server is missing")?.into(),
                message:input.get("message").and_then(serde_json::Value::as_str).ok_or("elicitation message is missing")?.into(),
                input:serde_json::from_value(input.get("input").cloned().ok_or("elicitation input is missing")?).map_err(|error|error.to_string())?,
            },
            _=>unreachable!(),
        };
        let id:agent_protocol::ids::RequestId=request.id.clone().into();
        Ok((id.clone(),Arc::new(requests::Request {id,target:match item.provider_turn_id.as_ref() {Some(id)=>requests::RequestTarget::Turn {turn_id:item.run_id.as_ref().unwrap_or(id).clone().into(),item_id:Some(item.id.clone().into())},None=>requests::RequestTarget::Session},delivery:session::RequestDelivery::Awaiting,body})))
    }).collect()
}

fn item_view(
    item: &model::TurnItem,
    provider: session::ProviderKind,
    has_detail: bool,
    subagent_model: Option<&str>,
) -> Result<Vec<Arc<items::Item>>, String> {
    use model::TurnItemBody as Body;
    let body = match &item.body {
        Body::UserMessage {
            text,
            attachments,
            context,
            ..
        } => {
            let mut content = attachment_parts(attachments)?;
            content.extend(context_parts(context.as_ref()));
            items::ItemBody::UserMessage {
                text: Some(text.clone()),
                content,
            }
        }
        Body::AssistantMessage { text, phase, .. } => items::ItemBody::AssistantText {
            text: text.clone(),
            phase: match phase.as_deref() {
                Some("commentary") => items::AssistantPhase::Commentary,
                Some("final_answer") => items::AssistantPhase::Final,
                _ => items::AssistantPhase::Unknown,
            },
        },
        Body::Reasoning { text, .. } => items::ItemBody::Reasoning {
            content: vec![text.clone()],
            summary: Vec::new(),
        },
        Body::WebSearch { patterns, .. } => items::ItemBody::WebSearch {
            query: patterns.join(" | "),
            action: Some(items::WebSearchAction::Search {
                query: patterns.first().cloned(),
                queries: patterns.clone(),
            }),
        },
        Body::Notification {
            source,
            outcome,
            summary,
            ..
        } => items::ItemBody::Notification {
            reports: vec![display::BackgroundReport {
                kind: match source.get("kind").and_then(serde_json::Value::as_str) {
                    Some("command" | "background_command") => display::BackgroundTaskKind::Command,
                    Some("subagent" | "delegated_task") => display::BackgroundTaskKind::Subagent,
                    Some("monitor") => display::BackgroundTaskKind::Monitor,
                    _ => display::BackgroundTaskKind::BackgroundTask,
                },
                label: source
                    .get("label")
                    .map(|label| label.as_str().map(str::to_owned))
                    .unwrap_or_else(|| Some(summary.clone())),
                outcome: match outcome.as_str() {
                    "completed" => display::BackgroundOutcome::Completed,
                    "failed" => display::BackgroundOutcome::Failed,
                    "cancelled" => display::BackgroundOutcome::Cancelled,
                    "updated" => display::BackgroundOutcome::Updated,
                    _ => display::BackgroundOutcome::Unknown,
                },
                exit_code: source
                    .get("exitCode")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|value| value.try_into().ok()),
            }],
        },
        Body::CommandExecution {
            input,
            output,
            exit_code,
            output_omitted,
            ..
        } => items::ItemBody::CommandExecution {
            command: input.clone(),
            cwd: None,
            output: output.clone().unwrap_or_default(),
            exit_code: *exit_code,
            // Stream omission requests details; the hydrated result determines
            // whether the provider itself omitted any output.
            output_omitted: has_detail && output_omitted == &Some(true),
        },
        Body::FileChange {
            file_name,
            changes,
            diff_str,
        } => items::ItemBody::FileChange {
            changes: if changes.is_empty() {
                vec![items::FileChange {
                    path: file_name.clone(),
                    kind: items::FileChangeKind::Unknown,
                    diff: diff_str.clone(),
                    proposal: None,
                }]
            } else {
                changes
                    .iter()
                    .enumerate()
                    .map(|(index, change)| items::FileChange {
                        path: change
                            .get("oldPath")
                            .or_else(|| change.get("path"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(file_name)
                            .into(),
                        kind: match change.get("operation").and_then(serde_json::Value::as_str) {
                            Some("add" | "create") => items::FileChangeKind::Add,
                            Some("delete") => items::FileChangeKind::Delete,
                            _ => items::FileChangeKind::Update {
                                move_path: change
                                    .get("oldPath")
                                    .and_then(serde_json::Value::as_str)
                                    .map(|_| {
                                        change
                                            .get("path")
                                            .and_then(serde_json::Value::as_str)
                                            .unwrap_or(file_name)
                                            .to_owned()
                                    }),
                            },
                        },
                        // T3 carries the first change's diff, not one diff per
                        // metadata entry in changes.
                        diff: (index == 0).then(|| diff_str.clone()).flatten(),
                        proposal: None,
                    })
                    .collect()
            },
            output: String::new(),
        },
        Body::ProposedPlan { markdown, .. } => items::ItemBody::Plan {
            text: markdown.clone(),
        },
        Body::TodoList {
            steps, explanation, ..
        } => items::ItemBody::Plan {
            text: explanation
                .iter()
                .cloned()
                .chain(steps.iter().map(|step| {
                    format!(
                        "- [{}] {}",
                        if step.status == "completed" { "x" } else { " " },
                        step.text
                    )
                }))
                .collect::<Vec<_>>()
                .join("\n"),
        },
        Body::Subagent {
            prompt,
            result,
            child_thread_id,
            ..
        } => items::ItemBody::Subagent {
            tool: String::new(),
            prompt: Some(prompt.clone()),
            model: subagent_model.map(str::to_owned),
            effort: None,
            sender: None,
            receivers: child_thread_id
                .as_deref()
                .map(agent_protocol::orchestration::shell::public_session_ref)
                .transpose()?
                .into_iter()
                .collect(),
            states: child_thread_id
                .as_deref()
                .map(|child| {
                    Ok::<_, String>(items::SubagentState {
                        session: agent_protocol::orchestration::shell::public_session_ref(child)?,
                        status: match item.status {
                            model::TurnItemStatus::Completed => display::TurnStatus::Completed,
                            model::TurnItemStatus::Failed => display::TurnStatus::Failed,
                            model::TurnItemStatus::Cancelled
                            | model::TurnItemStatus::Interrupted => {
                                display::TurnStatus::Interrupted
                            }
                            _ => display::TurnStatus::Running,
                        },
                        native_status: None,
                        message: result.clone(),
                    })
                })
                .transpose()?
                .into_iter()
                .collect(),
            agent_id: None,
            result: result.clone().map(serde_json::Value::String),
        },
        Body::DynamicTool {
            viewed_image_path: Some(path),
            ..
        } => items::ItemBody::ImageView { path: path.clone() },
        Body::DynamicTool {
            tool_name,
            input,
            output,
            ..
        } => {
            let collaboration = matches!(
                tool_name.as_deref(),
                Some("codex.collaboration" | "claudeAgent.collaboration" | "dsh.collaboration")
            )
            .then(|| collaboration(input, output).ok())
            .flatten();
            collaboration.unwrap_or_else(|| items::ItemBody::ToolCall {
                kind: items::ToolKind::Dynamic,
                tool: tool_name.clone().unwrap_or_else(|| "Tool".into()),
                server: None,
                namespace: None,
                resource_uri: None,
                plugin_id: None,
                arguments: input.clone(),
                result: output.clone(),
                error: None,
                content: Vec::new(),
                success: None,
                duration_ms: None,
            })
        }
        Body::Compaction { .. } => items::ItemBody::Compaction {},
        Body::SystemNotice { message }
        | Body::RunInterruptRequest { message }
        | Body::RunInterruptResult { message } => items::ItemBody::Hook {
            fragments: vec![message.clone()],
        },
        Body::Handoff { summary, .. } => items::ItemBody::Custom {
            provider,
            kind: "context_handoff".into(),
            value: serde_json::to_value(summary).unwrap(),
        },
        Body::UserInputRequest { .. }
        | Body::ApprovalRequest { .. }
        | Body::Checkpoint { .. }
        | Body::Fork { .. }
        | Body::Error { .. } => return Ok(Vec::new()),
    };
    let status = match item.status {
        model::TurnItemStatus::Idle
        | model::TurnItemStatus::Pending
        | model::TurnItemStatus::Running
        | model::TurnItemStatus::Waiting => display::ItemStatus::Running,
        model::TurnItemStatus::Completed => display::ItemStatus::Completed,
        model::TurnItemStatus::Failed => display::ItemStatus::Failed,
        model::TurnItemStatus::Cancelled | model::TurnItemStatus::Interrupted => {
            display::ItemStatus::Interrupted
        }
    };
    let mut view = items::Item::new(item.id.clone().into(), status, body);
    if !has_detail && needs_detail(item) {
        view.defer();
    }
    if let Body::UserMessage { message_id, .. } = &item.body {
        view.client_input_id = Some(message_id.clone().into());
    }
    let mut views = vec![Arc::new(view)];
    if let Body::AssistantMessage { attachments, .. } = &item.body {
        for (attachment, part) in attachments.iter().zip(attachment_parts(attachments)?) {
            if let items::MessagePart::Image { source } = part {
                let id = attachment
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or("image attachment ID is missing")?;
                views.push(Arc::new(items::Item::new(
                    format!("{}:attachment:{id}", item.id).into(),
                    status,
                    items::ItemBody::ImageGeneration {
                        saved_path: Some(source),
                        data: None,
                        revised_prompt: None,
                    },
                )));
            }
        }
    }
    Ok(views)
}

fn collaboration(
    input: &serde_json::Value,
    output: &Option<serde_json::Value>,
) -> Result<items::ItemBody, String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Collaboration {
        tool: String,
        prompt: Option<String>,
        model: Option<String>,
        effort: Option<String>,
        sender_thread_id: Option<String>,
        receiver_thread_ids: Vec<String>,
        agents_states: BTreeMap<String, State>,
        agent_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct State {
        status: display::TurnStatus,
        message: Option<String>,
    }
    let value: Collaboration =
        serde_json::from_value(input.clone()).map_err(|error| error.to_string())?;
    let public = agent_protocol::orchestration::shell::public_session_ref;
    Ok(items::ItemBody::Subagent {
        tool: value.tool,
        prompt: value.prompt,
        model: value.model,
        effort: value.effort,
        sender: value.sender_thread_id.as_deref().map(public).transpose()?,
        receivers: value
            .receiver_thread_ids
            .iter()
            .map(|id| public(id))
            .collect::<Result<_, _>>()?,
        states: value
            .agents_states
            .into_iter()
            .map(|(id, state)| {
                Ok(items::SubagentState {
                    session: public(&id)?,
                    status: state.status,
                    native_status: None,
                    message: state.message,
                })
            })
            .collect::<Result<_, String>>()?,
        agent_id: value.agent_id,
        result: output.clone(),
    })
}

fn needs_detail(item: &model::TurnItem) -> bool {
    item.detail_omitted
        || item.body.has_omitted_detail()
        || matches!(&item.body,model::TurnItemBody::DynamicTool {input,viewed_image_path:None,..} if input.get("truncated")==Some(&serde_json::Value::Bool(true)))
}
