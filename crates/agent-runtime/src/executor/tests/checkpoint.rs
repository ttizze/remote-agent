//! CheckpointService.test.ts, CheckpointCaptureService.test.ts,
//! CheckpointScopeOwnership.test.ts and RunFinalizationService.test.ts.
use super::*;
use agent_domain::{CheckpointStatus, Command};

fn statuses(state: &State) -> Vec<(u64, CheckpointStatus, bool)> {
    let mut checkpoints: Vec<_> = state
        .checkpoints
        .iter()
        .map(|checkpoint| {
            (
                checkpoint.run_ordinal,
                checkpoint.status,
                checkpoint.run.is_some(),
            )
        })
        .collect();
    checkpoints.sort_by_key(|(ordinal, ..)| *ordinal);
    checkpoints
}

#[tokio::test(flavor = "multi_thread")]
async fn captures_a_finished_turn_after_its_thread_start_baseline() {
    let rig = rig();
    let id = tid("thread-capture");
    let scope = rig.scoped(&id, root_workspace("/repo")).await;
    let run = rig.completed_run(&id, "first", "turn-1").await;

    assert_eq!(
        rig.ops.logged(),
        [
            "lookup /repo 0",
            "capture /repo 0",
            "provider-start thread-capture",
            "lookup /repo 0",
            "capture /repo 1",
            "lookup /repo 0",
            "files /repo 0 1",
            "finalized thread-capture /repo",
        ]
    );
    let state = rig.state(&id).await;
    assert_eq!(
        statuses(&state),
        [
            (0, CheckpointStatus::Ready, false),
            (1, CheckpointStatus::Ready, true)
        ]
    );
    let finished = rig.run(&id, &run).await;
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(finished.checkpoint, Some(checkpoint_id(&scope.id, 1)));
    let captured = state
        .checkpoints
        .iter()
        .find(|checkpoint| checkpoint.run_ordinal == 1)
        .unwrap();
    assert_eq!(captured.file_ref, checkpoint_reference(&scope.id, 1));
    assert_eq!(
        captured.native_heads.get("codex"),
        Some(&Some("turn-1".to_owned()))
    );
    let baseline = rig
        .store
        .checkpoint_baseline(&scope.id, 0)
        .unwrap()
        .unwrap();
    assert_eq!(baseline.checkpoint, checkpoint_id(&scope.id, 0));
}

// CheckpointService.test.ts: "materializes baseline, lookup fails=%s".
#[tokio::test(flavor = "multi_thread")]
async fn materializes_a_baseline_as_missing_when_its_ref_lookup_fails() {
    for lookup_fails in [false, true] {
        let rig = rig();
        let id = tid("thread-materialize");
        let scope = rig.scoped(&id, root_workspace("/repo")).await;
        let run = rig.send(&id, "first", "first").await;
        rig.drain().await;
        rig.ops.fail_lookup.store(lookup_fails, Ordering::SeqCst);
        rig.ops.log.lock().unwrap().clear();
        rig.turn(&id, &run, "turn-1", RunStatus::Completed).await;
        rig.drain().await;

        let state = rig.state(&id).await;
        let baseline = state
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.run_ordinal == 0)
            .unwrap();
        assert_eq!(baseline.file_ref, checkpoint_reference(&scope.id, 0));
        assert_eq!(
            baseline.status,
            if lookup_fails {
                CheckpointStatus::Missing
            } else {
                CheckpointStatus::Ready
            }
        );
        // The baseline, then the previous ref of the capture's file summary.
        assert_eq!(
            rig.ops.logged_with("lookup"),
            ["lookup /repo 0", "lookup /repo 0"]
        );
        assert_eq!(
            rig.ops.logged_with("files"),
            if lookup_fails {
                vec![]
            } else {
                vec!["files /repo 0 1"]
            }
        );
        assert_eq!(rig.run(&id, &run).await.status, RunStatus::Completed);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_outside_git_records_missing_checkpoints_and_finishes_the_run() {
    let rig = rig();
    let id = tid("thread-not-git");
    rig.ops.not_git.lock().unwrap().insert("/scratch".into());
    rig.scoped(&id, root_workspace("/scratch")).await;
    let run = rig.completed_run(&id, "first", "turn-1").await;

    assert!(rig.ops.logged_with("capture").is_empty());
    assert_eq!(
        statuses(&*rig.state(&id).await),
        [
            (0, CheckpointStatus::Missing, false),
            (1, CheckpointStatus::Missing, true)
        ]
    );
    assert_eq!(rig.run(&id, &run).await.status, RunStatus::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_capture_records_an_error_checkpoint_and_finishes_the_run() {
    let rig = rig();
    let id = tid("thread-capture-error");
    rig.scoped(&id, root_workspace("/repo")).await;
    let run = rig.send(&id, "first", "first").await;
    rig.drain().await;
    rig.ops.fail_capture.store(true, Ordering::SeqCst);
    rig.turn(&id, &run, "turn-1", RunStatus::Completed).await;
    rig.drain().await;

    assert_eq!(
        statuses(&*rig.state(&id).await),
        [
            (0, CheckpointStatus::Ready, false),
            (1, CheckpointStatus::Error, true)
        ]
    );
    assert_eq!(rig.run(&id, &run).await.status, RunStatus::Completed);
}

// T3 CheckpointService.capture: a turn's checkpoint lists the files changed
// since the scope's previous checkpoint; an unavailable previous ref, diff or
// capture leaves the list empty and the run still finishes.
#[tokio::test(flavor = "multi_thread")]
async fn a_captured_turn_records_the_files_changed_since_the_previous_checkpoint() {
    let file = agent_domain::CheckpointFile {
        path: "a.txt".into(),
        kind: "modified".into(),
        additions: 2,
        deletions: 1,
    };
    for case in ["summary", "diff-fails", "previous-missing", "capture-fails"] {
        let rig = rig();
        let id = tid("thread-files");
        rig.scoped(&id, root_workspace("/repo")).await;
        let run = rig.send(&id, "first", "first").await;
        rig.drain().await;
        *rig.ops.files.lock().unwrap() = if case == "diff-fails" {
            Err("simulated diff failure".into())
        } else {
            Ok(vec![file.clone()])
        };
        if case == "previous-missing" {
            rig.ops.refs.lock().unwrap().clear();
        }
        rig.ops
            .fail_capture
            .store(case == "capture-fails", Ordering::SeqCst);
        rig.turn(&id, &run, "turn-1", RunStatus::Completed).await;
        rig.drain().await;

        let state = rig.state(&id).await;
        let files = |ordinal| {
            state
                .checkpoints
                .iter()
                .find(|checkpoint| checkpoint.run_ordinal == ordinal)
                .unwrap()
                .files
                .clone()
        };
        assert!(files(0).is_empty(), "{case}");
        assert_eq!(
            files(1),
            if case == "summary" {
                vec![file.clone()]
            } else {
                vec![]
            },
            "{case}"
        );
        assert_eq!(
            rig.run(&id, &run).await.status,
            RunStatus::Completed,
            "{case}"
        );
    }
}

// CheckpointScopeOwnership.test.ts: a later root run resolves the thread baseline.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_run_reuses_the_scope_baselines() {
    let rig = rig();
    let id = tid("thread-scope-owner");
    let scope = rig.scoped(&id, root_workspace("/repo")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    rig.ops.log.lock().unwrap().clear();
    let second = rig.completed_run(&id, "second", "turn-2").await;

    assert_eq!(
        rig.ops.logged(),
        [
            "lookup /repo 1",
            "provider-start thread-scope-owner",
            "capture /repo 2",
            "lookup /repo 1",
            "files /repo 1 2",
            "finalized thread-scope-owner /repo",
        ]
    );
    let state = rig.state(&id).await;
    assert_eq!(
        statuses(&state),
        [
            (0, CheckpointStatus::Ready, false),
            (1, CheckpointStatus::Ready, true),
            (2, CheckpointStatus::Ready, true)
        ]
    );
    assert_eq!(
        rig.run(&id, &second).await.checkpoint,
        Some(checkpoint_id(&scope.id, 2))
    );
}

// CheckpointCaptureService.test.ts: "records the checkpoint of a cancelled run and keeps it cancelled".
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_run_records_its_checkpoint_and_keeps_its_status() {
    let rig = rig();
    let id = tid("thread-stopped-capture");
    let scope = rig.scoped(&id, root_workspace("/repo")).await;
    let run = rig.send(&id, "first", "first").await;
    rig.turn(&id, &run, "turn-1", RunStatus::Interrupted).await;
    rig.drain().await;

    let stopped = rig.run(&id, &run).await;
    assert_eq!(stopped.status, RunStatus::Interrupted);
    assert_eq!(stopped.checkpoint, Some(checkpoint_id(&scope.id, 1)));
}

// CheckpointCaptureService.test.ts: "does not capture a stopped run that a rollback already discarded".
#[tokio::test(flavor = "multi_thread")]
async fn does_not_capture_a_stopped_run_that_a_rollback_already_discarded() {
    let rig = rig();
    let id = tid("thread-discarded-capture");
    let scope = rig.scoped(&id, root_workspace("/repo")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    let second = rig.send(&id, "second", "second").await;
    rig.turn(&id, &second, "turn-2", RunStatus::Interrupted)
        .await;
    let rollback = CommandId::new("rollback-before-capture").unwrap();
    let reply = rig
        .registry
        .dispatch(
            &id,
            rollback.clone(),
            Command::Rollback {
                checkpoint: checkpoint_id(&scope.id, 1),
                restore_files: false,
                restore_refusal: None,
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap()
        .reply;
    assert_eq!(reply, Reply::Accepted);
    // The rollback lands while the capture waits (e.g. to retry).
    rig.input(
        &id,
        Input::Effect(EffectResult::RollbackFinished {
            bindings: vec![],
            command: rollback,
        }),
    )
    .await;
    rig.ops.log.lock().unwrap().clear();
    rig.drain().await;

    assert!(rig.ops.logged_with("capture").is_empty());
    let discarded = rig.run(&id, &second).await;
    assert_eq!(discarded.status, RunStatus::RolledBack);
    assert_eq!(discarded.checkpoint, None);
    assert!(
        rig.outbox_kinds(&id)
            .await
            .iter()
            .all(|(_, status)| *status == crate::EffectStatus::Succeeded)
    );
}

// T3 CheckpointCaptureService.ts skips only ready baselines: one that was missing
// because its lookup failed becomes ready once the ref can be read again.
#[tokio::test(flavor = "multi_thread")]
async fn a_missing_baseline_is_materialized_again_by_a_later_capture() {
    let rig = rig();
    let id = tid("thread-missing-baseline");
    rig.scoped(&id, root_workspace("/repo")).await;
    let first = rig.send(&id, "first", "first").await;
    rig.drain().await;
    rig.ops.fail_lookup.store(true, Ordering::SeqCst);
    rig.turn(&id, &first, "turn-1", RunStatus::Completed).await;
    rig.drain().await;
    assert_eq!(
        statuses(&*rig.state(&id).await),
        [
            (0, CheckpointStatus::Missing, false),
            (1, CheckpointStatus::Ready, true)
        ]
    );

    rig.ops.fail_lookup.store(false, Ordering::SeqCst);
    rig.completed_run(&id, "second", "turn-2").await;
    assert_eq!(
        statuses(&*rig.state(&id).await),
        [
            (0, CheckpointStatus::Ready, false),
            (1, CheckpointStatus::Ready, true),
            (2, CheckpointStatus::Ready, true)
        ]
    );
}

// T3 Orchestrator.ts prepares a root scope for every root run: a thread that never
// went through workspace preparation (imported, forked or delegated) still
// checkpoints its runs in a scope of its own.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_without_a_prepared_scope_checkpoints_in_its_threads_own_scope() {
    let rig = rig();
    let parent = tid("thread-scoped-parent");
    let child = tid("thread-unscoped-child");
    let parent_scope = rig.scoped(&parent, worktree("/wt")).await;
    rig.create(&child, Some(worktree("/wt"))).await;
    assert_eq!(rig.state(&child).await.checkpoint_scope, None);

    let run = rig.completed_run(&child, "first", "turn-1").await;

    let scope = checkpoint_scope(&child, "/wt");
    assert_ne!(scope.id, parent_scope.id);
    let state = rig.state(&child).await;
    assert_eq!(state.checkpoint_scope.as_ref(), Some(&scope));
    assert_eq!(
        rig.run(&child, &run).await.checkpoint_scope,
        Some(scope.clone())
    );
    assert_eq!(
        statuses(&state),
        [
            (0, CheckpointStatus::Ready, false),
            (1, CheckpointStatus::Ready, true)
        ]
    );
    assert_eq!(
        rig.run(&child, &run).await.checkpoint,
        Some(checkpoint_id(&scope.id, 1))
    );
    assert!(
        rig.ops
            .refs
            .lock()
            .unwrap()
            .contains(&("/wt".to_owned(), checkpoint_reference(&scope.id, 0)))
    );
}
