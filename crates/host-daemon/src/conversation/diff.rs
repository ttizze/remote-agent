//! Which checkpoints a turn diff compares (T3 `CheckpointDiffQuery`).
use agent_domain::{CheckpointStatus, RunStatus, State};
use agent_runtime::checkpoint_reference;

/// The refs of a turn diff and the checkout that holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiffRefs {
    pub(crate) cwd: String,
    pub(crate) from: String,
    pub(crate) to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffUnavailable {
    /// A run ordinal past the last ready checkpoint of a completed run.
    Range {
        requested: u64,
        available: u64,
    },
    /// The checkpoint (`from` or `to`) of an ordinal is missing.
    Checkpoint {
        from: bool,
        ordinal: u64,
    },
    WorkspaceMissing,
}

/// Ordinal 0 is the baseline of the thread's first checkpoint scope; other
/// ordinals are ready checkpoints of completed runs.
pub(crate) fn diff_refs(state: &State, from: u64, to: u64) -> Result<DiffRefs, DiffUnavailable> {
    let ready: Vec<_> = state
        .checkpoints
        .iter()
        .filter(|checkpoint| {
            checkpoint.status == CheckpointStatus::Ready
                && checkpoint.run.as_ref().is_some_and(|run| {
                    state.runs.iter().any(|candidate| {
                        &candidate.id == run && candidate.status == RunStatus::Completed
                    })
                })
        })
        .collect();
    let available = ready.iter().map(|c| c.run_ordinal).max().unwrap_or(0);
    if to > available {
        return Err(DiffUnavailable::Range {
            requested: to,
            available,
        });
    }
    let to_checkpoint = ready
        .iter()
        .find(|checkpoint| checkpoint.run_ordinal == to)
        .ok_or(DiffUnavailable::Checkpoint {
            from: false,
            ordinal: to,
        })?;
    let scope = to_checkpoint
        .scope
        .as_ref()
        .ok_or(DiffUnavailable::WorkspaceMissing)?;
    let from_ref = if from == 0 {
        state
            .runs
            .iter()
            .filter_map(|run| {
                run.checkpoint_scope
                    .as_ref()
                    .map(|scope| (run.ordinal, scope))
            })
            .min_by_key(|(ordinal, _)| *ordinal)
            .map(|(_, first)| checkpoint_reference(&first.id, 0))
    } else {
        ready
            .iter()
            .find(|checkpoint| checkpoint.run_ordinal == from)
            .map(|checkpoint| checkpoint.file_ref.clone())
    }
    .ok_or(DiffUnavailable::Checkpoint {
        from: true,
        ordinal: from,
    })?;
    Ok(DiffRefs {
        cwd: scope.cwd.clone(),
        from: from_ref,
        to: to_checkpoint.file_ref.clone(),
    })
}

#[cfg(test)]
mod tests {
    //! T3 `CheckpointDiffQuery.test.ts` over the domain projection.
    use super::*;
    use agent_domain::{
        Checkpoint, CheckpointId, CheckpointScope, CheckpointScopeId, Driver, MessageId,
        ModelSelection, Run, RunId, Timestamp,
    };
    use std::collections::BTreeMap;

    fn scope(id: &str) -> CheckpointScope {
        CheckpointScope {
            id: CheckpointScopeId::new(id).unwrap(),
            cwd: "/repo".into(),
        }
    }
    fn run(id: &str, ordinal: u64, scope_id: &str) -> Run {
        Run {
            restart_of: None,
            restart_cancelled_work: vec![],
            checkpoint_scope: Some(scope(scope_id)),
            native_baseline_heads: BTreeMap::new(),
            id: RunId::new(id).unwrap(),
            ordinal,
            message: MessageId::new(format!("message:{id}")).unwrap(),
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "gpt-6-luna".into(),
                options: BTreeMap::new(),
            },
            status: RunStatus::Completed,
            attempt: None,
            queue_position: None,
            queue_held: false,
            requested_at: Timestamp::from_millis(0).unwrap(),
            started_at: None,
            completed_at: None,
            source_plan: None,
            checkpoint: None,
            continuation: false,
        }
    }
    fn checkpoint(run: &str, ordinal: u64, scope_id: &str, file_ref: &str) -> Checkpoint {
        Checkpoint {
            status: CheckpointStatus::Ready,
            scope: Some(scope(scope_id)),
            id: CheckpointId::new(format!("checkpoint:{run}")).unwrap(),
            run: Some(RunId::new(run).unwrap()),
            run_ordinal: ordinal,
            native_heads: BTreeMap::new(),
            file_ref: file_ref.into(),
        }
    }
    /// Two completed runs; only the second has a ready checkpoint.
    fn projection() -> State {
        State {
            runs: vec![run("run:1", 1, "scope:1"), run("run:2", 2, "scope:2")],
            checkpoints: vec![checkpoint("run:2", 2, "scope:2", "refs/t3/test/second")],
            ..State::default()
        }
    }

    #[test]
    fn computes_run_diffs_from_projected_checkpoint_scopes() {
        assert_eq!(
            diff_refs(&projection(), 0, 2),
            Ok(DiffRefs {
                cwd: "/repo".into(),
                from: checkpoint_reference(&CheckpointScopeId::new("scope:1").unwrap(), 0),
                to: "refs/t3/test/second".into(),
            })
        );
    }

    #[test]
    fn reports_the_unavailable_range() {
        assert_eq!(
            diff_refs(&projection(), 0, 3),
            Err(DiffUnavailable::Range {
                requested: 3,
                available: 2
            })
        );
    }

    #[test]
    fn excludes_ready_checkpoints_of_rolled_back_runs() {
        let mut state = projection();
        state.runs[1].status = RunStatus::RolledBack;
        state
            .checkpoints
            .insert(0, checkpoint("run:1", 1, "scope:1", "refs/t3/test/first"));
        assert_eq!(
            diff_refs(&state, 0, 2),
            Err(DiffUnavailable::Range {
                requested: 2,
                available: 1
            })
        );
    }

    #[test]
    fn reports_a_missing_baseline_ref() {
        let mut state = projection();
        for run in &mut state.runs {
            run.checkpoint_scope = None;
        }
        assert_eq!(
            diff_refs(&state, 0, 2),
            Err(DiffUnavailable::Checkpoint {
                from: true,
                ordinal: 0
            })
        );
    }
}
