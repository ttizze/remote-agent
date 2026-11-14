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

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-11-14T00:00:00.000Z";
    fn source() -> (Thread, Run, ContextSourcePoint) {
        let selection = ModelSelection {
            instance_id: "codex".into(),
            model: "fixture".into(),
            options: vec![],
        };
        let thread = Thread {
            id: "source".into(),
            history_origin: None,
            created_by: Actor::User,
            creation_source: CreationSource::Mobile,
            project_id: None,
            title: "Source".into(),
            provider_instance_id: "codex".into(),
            model_selection: selection.clone(),
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            branch: None,
            worktree_path: Some("/project".into()),
            active_provider_thread_id: None,
            lineage: Lineage {
                parent_thread_id: None,
                relationship_to_parent: None,
                root_thread_id: "source".into(),
                side_chat: None,
            },
            forked_from: None,
            created_at: NOW.into(),
            updated_at: NOW.into(),
            archived_at: None,
            settled_override: None,
            settled_at: None,
            last_visited_at: None,
            deleted_at: None,
        };
        let decision = admit(
            &State::new(thread.clone()),
            Input {
                message_id: "message".into(),
                created_by: Actor::User,
                creation_source: CreationSource::Mobile,
                text: "question".into(),
                attachments: vec![],
                context: None,
                model_selection: selection,
                driver: "codex".into(),
                workspace_preparation: None,
            },
            Admission::Queue,
            Context {
                command_id: "start",
                now: NOW,
                next_run_ordinal: 1,
                can_steer: false,
            },
        )
        .unwrap();
        let run = decision
            .changes
            .into_iter()
            .find_map(|change| match change {
                Change::RunCreated(run) => Some(run),
                _ => None,
            })
            .unwrap();
        let point = ContextSourcePoint {
            thread_id: thread.id.clone(),
            run_id: Some(run.id.clone()),
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        };
        (thread, run, point)
    }

    // ThreadForkService.ts36-45: waiting is provider-finished/checkpoint pending.
    #[test]
    fn only_finished_source_runs_can_be_forked() {
        for (status, allowed) in [
            (RunStatus::Preparing, false),
            (RunStatus::Queued, false),
            (RunStatus::Starting, false),
            (RunStatus::Running, false),
            (RunStatus::RolledBack, false),
            (RunStatus::Waiting, true),
            (RunStatus::Completed, true),
            (RunStatus::Failed, true),
            (RunStatus::Interrupted, true),
            (RunStatus::Cancelled, true),
        ] {
            let (thread, mut run, point) = source();
            run.status = status;
            let fork = fork_thread(&thread, &run, point.clone(), "target", "transfer", NOW);
            assert_eq!(fork.is_ok(), allowed);
            if let Ok((target, transfer)) = fork {
                assert_eq!(target.lineage.parent_thread_id.as_ref(), Some(&thread.id));
                assert_eq!(
                    target.lineage.relationship_to_parent.as_deref(),
                    Some("fork")
                );
                assert_eq!(transfer.source_point, point);
                assert_eq!(transfer.status, TransferStatus::Pending);
                assert!(target.active_provider_thread_id.is_none());
                assert!(transfer.target_run_id.is_none());
                assert!(
                    transfer.error.is_some(),
                    "portable context is required without a strong native source"
                );
            }
        }
    }

    #[test]
    fn an_invalid_source_cannot_create_a_fork() {
        for change in [
            "same-thread",
            "foreign-run",
            "foreign-point",
            "wrong-run",
            "deleted",
        ] {
            let (mut thread, mut run, mut point) = source();
            run.status = RunStatus::Completed;
            let target = if change == "same-thread" {
                "source"
            } else {
                "target"
            };
            match change {
                "foreign-run" => run.thread_id = "other".into(),
                "foreign-point" => point.thread_id = "other".into(),
                "wrong-run" => point.run_id = Some("other".into()),
                "deleted" => thread.deleted_at = Some(NOW.into()),
                _ => {}
            }
            assert!(fork_thread(&thread, &run, point, target, "transfer", NOW).is_err());
        }
    }

    #[test]
    fn a_strong_native_source_needs_no_portable_context_warning() {
        for strength in [NativeRefStrength::Strong, NativeRefStrength::Weak] {
            let (thread, mut run, mut point) = source();
            run.status = RunStatus::Completed;
            point.provider_thread_ref = Some(ProviderRef {
                driver: "codex".into(),
                native_id: Some("native".into()),
                strength,
                fingerprint: None,
                ordinal: None,
            });
            let (_, transfer) =
                fork_thread(&thread, &run, point, "target", "transfer", NOW).unwrap();
            assert_eq!(
                transfer.error.is_none(),
                strength == NativeRefStrength::Strong
            );
        }
    }

    #[test]
    fn a_fork_starts_its_own_lifecycle_and_keeps_the_source_provenance() {
        let (mut source, mut run, point) = source();
        run.status = RunStatus::Completed;
        source.created_by = Actor::System;
        source.creation_source = CreationSource::Provider;
        source.active_provider_thread_id = Some("parent-native-thread".into());
        source.created_at = "2026-10-01T00:00:00.000Z".into();
        source.updated_at = source.created_at.clone();
        source.archived_at = Some(source.created_at.clone());
        source.settled_override = Some("settled".into());
        source.settled_at = Some(source.created_at.clone());
        source.last_visited_at = Some(source.created_at.clone());
        let (target, transfer) =
            fork_thread(&source, &run, point, "target", "transfer", NOW).unwrap();
        assert_eq!(target.id, "target");
        assert_eq!(target.title, "Source fork");
        assert_eq!(target.created_by, Actor::User);
        assert_eq!(target.creation_source, CreationSource::Mobile);
        assert_eq!(target.created_at, NOW);
        assert_eq!(target.updated_at, NOW);
        assert!(target.archived_at.is_none());
        assert!(target.settled_override.is_none());
        assert!(target.settled_at.is_none());
        assert!(target.last_visited_at.is_none());
        assert!(target.active_provider_thread_id.is_none());
        assert!(target.deleted_at.is_none());
        assert_eq!(
            target.forked_from,
            Some(ThreadForkSource::Run {
                thread_id: source.id.clone(),
                run_id: run.id.clone()
            })
        );
        assert_eq!(target.model_selection, source.model_selection);
        assert_eq!(target.worktree_path, source.worktree_path);
        assert_eq!(transfer.target_thread_id, target.id);
        assert_eq!(transfer.source_thread_id, source.id);
    }
}
