use super::*;

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
    let target = Thread {
        id: target_id.into(),
        title: format!("{} fork", source.title),
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        active_provider_thread_id: None,
        lineage: Lineage {
            parent_thread_id: Some(source.id.clone()),
            relationship_to_parent: Some("fork".into()),
            root_thread_id: source.lineage.root_thread_id.clone(),
            side_chat: None,
        },
        forked_from: Some(ThreadForkSource::Run {
            thread_id: source.id.clone(),
            run_id: run.id.clone(),
        }),
        created_at: now.into(),
        updated_at: now.into(),
        archived_at: None,
        settled_override: None,
        settled_at: None,
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
        source_provider_instance_id: Some(run.provider_instance_id.clone()),
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
