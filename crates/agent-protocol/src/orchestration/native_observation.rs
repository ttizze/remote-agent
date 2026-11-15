//! Native work observed outside an application run still owns real turns and
//! nodes. It must never create an application input, attempt, or delivery effect.
use super::*;

pub(super) fn selected<'a>(
    state: &'a State,
    provider_thread_id: &str,
    native_thread_id: &str,
) -> Option<&'a ProviderThread> {
    state.selected_native_thread().filter(|thread| {
        thread.id == provider_thread_id
            && state.thread.deleted_at.is_none()
            && thread
                .native_thread_ref
                .as_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some(native_thread_id)
    })
}

pub fn observe_external_turn(
    state: &State,
    provider_thread_id: &str,
    native_thread_id: &str,
    native_turn_ref: ProviderRef,
    status: ProviderTurnStatus,
    now: &str,
) -> Result<Decision, String> {
    let Some(provider) = selected(state, provider_thread_id, native_thread_id) else {
        return Ok(Decision::default());
    };
    if native_turn_ref.driver != provider.driver
        || native_turn_ref
            .native_id
            .as_ref()
            .is_none_or(|id| id.is_empty())
    {
        return Err("native turn does not belong to this provider".into());
    }
    let existing = state.provider_turns.values().find(|turn| {
        turn.provider_thread_id == provider_thread_id
            && turn.native_thread_ref == provider.native_thread_ref
            && turn.native_turn_ref.as_ref() == Some(&native_turn_ref)
    });
    // A callback with no owned attempt cannot adopt an application's write or
    // convert its unknown delivery receipt into a fresh external execution.
    if state.active_run().is_some()
        || state.has_uncertain_input()
        || existing.is_some_and(|turn| {
            turn.run_attempt_id.is_some() || turn.status != ProviderTurnStatus::Running
        })
    {
        return Ok(Decision::default());
    }
    let terminal = status != ProviderTurnStatus::Running;
    let turn = if let Some(turn) = existing {
        if turn.status == status {
            return Ok(Decision::default());
        }
        ProviderTurn {
            status,
            completed_at: terminal.then(|| now.into()),
            ..turn.clone()
        }
    } else {
        let id = derived_id(
            "provider-turn",
            &[
                provider_thread_id,
                native_thread_id,
                native_turn_ref
                    .native_id
                    .as_deref()
                    .expect("validated native turn"),
            ],
        );
        let node_id = derived_id("node", &[&id, "root"]);
        ProviderTurn {
            id,
            provider_thread_id: provider.id.clone(),
            node_id,
            run_attempt_id: None,
            native_thread_ref: provider.native_thread_ref.clone(),
            native_turn_ref: Some(native_turn_ref),
            native_control_id: None,
            ordinal: state
                .provider_turns
                .values()
                .filter(|turn| turn.provider_thread_id == provider.id)
                .map(|turn| turn.ordinal)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("provider turn ordinal exhausted")?,
            status,
            started_at: Some(now.into()),
            completed_at: terminal.then(|| now.into()),
            token_usage: None,
            model: None,
        }
    };
    let node = ExecutionNode {
        id: turn.node_id.clone(),
        thread_id: state.thread.id.clone(),
        run_id: None,
        parent_node_id: None,
        root_node_id: turn.node_id.clone(),
        kind: NodeKind::RootTurn,
        status: match status {
            ProviderTurnStatus::Pending => {
                return Err("an observed native turn is not pending".into());
            }
            ProviderTurnStatus::Running => NodeStatus::Running,
            ProviderTurnStatus::Completed => NodeStatus::Completed,
            ProviderTurnStatus::Interrupted => NodeStatus::Interrupted,
            ProviderTurnStatus::Cancelled => NodeStatus::Cancelled,
            ProviderTurnStatus::Failed => NodeStatus::Failed,
        },
        counts_for_run: false,
        provider_thread_id: Some(provider.id.clone()),
        provider_turn_id: Some(turn.id.clone()),
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: turn.started_at.clone(),
        completed_at: turn.completed_at.clone(),
    };
    let another_running = state.provider_turns.values().any(|other| {
        other.provider_thread_id == provider.id
            && other.native_thread_ref == provider.native_thread_ref
            && other.id != turn.id
            && other.status == ProviderTurnStatus::Running
    });
    let provider_status = if !terminal || another_running {
        ProviderThreadStatus::Active
    } else {
        ProviderThreadStatus::Idle
    };
    let mut changes = if terminal {
        finish_items(state, &turn, now)
    } else {
        Vec::new()
    };
    changes.extend([
        Change::ProviderTurn(turn),
        Change::Node(node),
        Change::ProviderThread(ProviderThread {
            status: provider_status,
            updated_at: now.into(),
            ..provider.clone()
        }),
    ]);
    Ok(Decision {
        changes,
        ..Default::default()
    })
}

pub fn observe_external_item(
    state: &State,
    provider_turn_id: &str,
    native_thread_id: &str,
    item: TurnItem,
) -> Result<Decision, String> {
    let Some(turn) = state.provider_turns.get(provider_turn_id).filter(|turn| {
        turn.run_attempt_id.is_none()
            && selected(state, &turn.provider_thread_id, native_thread_id)
                .is_some_and(|provider| provider.native_thread_ref == turn.native_thread_ref)
    }) else {
        return Ok(Decision::default());
    };
    let background = turn.status == ProviderTurnStatus::Completed
        && execution::is_background_item(
            state,
            &turn.provider_thread_id,
            state.turn_items.get(&item.id).unwrap_or(&item),
        );
    if !background
        && (turn.status != ProviderTurnStatus::Running
            || state.active_run().is_some()
            || state.has_uncertain_input())
    {
        return Ok(Decision::default());
    }
    if item.thread_id != state.thread.id
        || item.run_id.is_some()
        || !execution::item_provider_matches(state, &item, &turn.provider_thread_id)
        || item.provider_turn_id.as_deref() != Some(provider_turn_id)
    {
        return Err("item does not belong to this native turn".into());
    }
    if let Some(saved) = state.turn_items.get(&item.id) {
        if saved.run_id.is_some() || saved.provider_turn_id != item.provider_turn_id {
            return Err("item identity is already owned by another native turn".into());
        }
        if saved.status.is_terminal() && item.status == TurnItemStatus::Running {
            return Ok(Decision::default());
        }
    }
    Ok(execution::commit_item(
        state,
        &turn.provider_thread_id,
        item,
    ))
}

pub fn observe_external_plan(
    state: &State,
    provider_turn_id: &str,
    native_thread_id: &str,
    plan: Plan,
    item: TurnItem,
) -> Result<Decision, String> {
    execution::validate_plan(&plan, &item)?;
    let mut decision = observe_external_item(state, provider_turn_id, native_thread_id, item)?;
    if !decision.changes.is_empty() {
        decision.changes.push(Change::Plan(plan));
    }
    Ok(decision)
}

pub fn observe_external_subagent(
    state: &State,
    provider_turn_id: &str,
    native_thread_id: &str,
    subagent: Subagent,
    item: TurnItem,
) -> Result<Decision, String> {
    execution::validate_subagent(&subagent, &item)?;
    let mut decision = observe_external_item(state, provider_turn_id, native_thread_id, item)?;
    if !decision.changes.is_empty() {
        decision.changes.push(Change::Subagent(subagent));
    }
    Ok(decision)
}

fn finish_items(state: &State, turn: &ProviderTurn, now: &str) -> Vec<Change> {
    let (item_status, node_status) = match turn.status {
        ProviderTurnStatus::Failed => (TurnItemStatus::Failed, NodeStatus::Failed),
        ProviderTurnStatus::Cancelled => (TurnItemStatus::Cancelled, NodeStatus::Cancelled),
        ProviderTurnStatus::Interrupted => (TurnItemStatus::Interrupted, NodeStatus::Interrupted),
        _ => (TurnItemStatus::Completed, NodeStatus::Completed),
    };
    let mut changes = Vec::new();
    for item in state.turn_items.values().filter(|item| {
        item.run_id.is_none()
            && item.provider_turn_id.as_deref() == Some(&turn.id)
            && matches!(
                item.status,
                TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
            )
            && !(turn.status == ProviderTurnStatus::Completed
                && execution::is_background_item(state, &turn.provider_thread_id, item))
    }) {
        changes.push(Change::TurnItem(execution::settle_item(
            item,
            item_status,
            now,
        )));
        if let Some(node) = item.node_id.as_ref().and_then(|id| state.nodes.get(id)) {
            changes.push(Change::Node(ExecutionNode {
                status: node_status,
                completed_at: Some(now.into()),
                ..node.clone()
            }));
        }
    }
    changes.extend(
        state
            .runtime_requests
            .values()
            .filter(|request| {
                request.status == RequestStatus::Pending
                    && request.provider_turn_id.as_deref() == Some(&turn.id)
            })
            .map(|request| {
                Change::RuntimeRequest(RuntimeRequest {
                    status: if turn.status == ProviderTurnStatus::Interrupted {
                        RequestStatus::Cancelled
                    } else {
                        RequestStatus::Expired
                    },
                    resolved_at: Some(now.into()),
                    ..request.clone()
                })
            }),
    );
    changes
}
