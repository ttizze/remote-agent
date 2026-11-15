use super::*;

impl ContextTransfer {
    pub(super) fn for_run(&self, run_id: &str, provider_instance_id: &str, now: &str) -> Self {
        Self {
            target_run_id: Some(run_id.into()),
            target_provider_instance_id: Some(provider_instance_id.into()),
            updated_at: now.into(),
            ..self.clone()
        }
    }
}

/// ThreadForkService: freeze the source point, inherit the header, and leave
/// provider resources unresolved until the fork receives its first input.
pub fn fork_thread(
    source: &Thread,
    run: &Run,
    point: ContextSourcePoint,
    target_id: &str,
    transfer_id: &str,
    now: &str,
) -> Result<(Thread, ContextTransfer), String> {
    if source.id == target_id
        || run.thread_id != source.id
        || point.run_id.as_ref() != Some(&run.id)
        || point.thread_id != source.id
    {
        return Err("invalid fork source".into());
    }
    if source.deleted_at.is_some()
        || !matches!(
            run.status,
            RunStatus::Completed
                | RunStatus::Waiting
                | RunStatus::Failed
                | RunStatus::Interrupted
                | RunStatus::Cancelled
        )
    {
        return Err("finish the selected run before forking".into());
    }
    plan_fork(
        source,
        ThreadForkSource::Run {
            thread_id: source.id.clone(),
            run_id: run.id.clone(),
        },
        &run.provider_instance_id,
        point,
        target_id,
        transfer_id,
        now,
    )
}

/// Native/imported history has a real item boundary, without an application run.
pub fn fork_native_thread(
    source: &Thread,
    provider: &ProviderThread,
    turn: Option<&ProviderTurn>,
    point: ContextSourcePoint,
    target_id: &str,
    transfer_id: &str,
    now: &str,
) -> Result<(Thread, ContextTransfer), String> {
    if source.id == target_id
        || source.deleted_at.is_some()
        || provider.app_thread_id.as_deref() != Some(&source.id)
        || point.thread_id != source.id
        || point.run_id.is_some()
        || point.turn_item_id.is_none()
        || point.provider_thread_ref.as_ref().is_some_and(|reference| {
            Some(reference)
                != turn.map_or(provider.native_thread_ref.as_ref(), |turn| {
                    turn.native_thread_ref.as_ref()
                })
        })
    {
        return Err("invalid native fork source".into());
    }
    if let Some(turn) = turn {
        if turn.provider_thread_id != provider.id
            || turn.run_attempt_id.is_some()
            || point.provider_turn_ref != turn.native_turn_ref
        {
            return Err("native fork boundary belongs to another execution".into());
        }
        if matches!(
            turn.status,
            ProviderTurnStatus::Pending | ProviderTurnStatus::Running
        ) {
            return Err("finish the selected native turn before forking".into());
        }
    } else if source.history_origin != Some(ThreadHistoryOrigin::V1Import)
        || point.provider_turn_ref.is_some()
    {
        return Err("native fork has no history boundary".into());
    }
    plan_fork(
        source,
        ThreadForkSource::ProviderThread {
            provider_thread_id: provider.id.clone(),
            provider_turn_id: turn.map(|turn| turn.id.clone()),
        },
        &provider.provider_instance_id,
        point,
        target_id,
        transfer_id,
        now,
    )
}

fn plan_fork(
    source: &Thread,
    boundary: ThreadForkSource,
    provider_instance_id: &str,
    point: ContextSourcePoint,
    target_id: &str,
    transfer_id: &str,
    now: &str,
) -> Result<(Thread, ContextTransfer), String> {
    let target = Thread {
        id: target_id.into(),
        title: format!("{} fork", source.title),
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        active_provider_thread_id: None,
        workspace_launch: None,
        lineage: Lineage {
            parent_thread_id: Some(source.id.clone()),
            relationship_to_parent: Some("fork".into()),
            root_thread_id: source.lineage.root_thread_id.clone(),
            side_chat: None,
        },
        forked_from: Some(boundary),
        created_at: now.into(),
        updated_at: now.into(),
        archived_at: None,
        settled_override: None,
        settled_at: None,
        rollback_request_id: None,
        rollback_failure: None,
        last_visited_at: None,
        ..source.clone()
    };
    let error = point
        .provider_thread_ref
        .as_ref()
        .is_none_or(|reference| reference.strength != NativeRefStrength::Strong)
        .then(|| "Source provider thread does not expose a strong native thread ref.".into());
    let transfer = ContextTransfer {
        id: transfer_id.into(),
        kind: TransferType::Fork,
        source_thread_id: source.id.clone(),
        target_thread_id: target_id.into(),
        source_point: point,
        base_point: None,
        source_provider_instance_id: Some(provider_instance_id.into()),
        target_provider_instance_id: None,
        target_run_id: None,
        status: TransferStatus::Pending,
        resolution: None,
        created_by: Actor::User,
        error,
        created_at: now.into(),
        updated_at: now.into(),
        consumed_at: None,
    };
    Ok((target, transfer))
}
