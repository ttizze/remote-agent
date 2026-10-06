//! CheckpointRollbackService.test.ts and CheckpointRestoreSafety.test.ts.
use super::*;
use crate::session::tests::codex_replies;
use agent_domain::{CheckpointStatus, ProviderCommand};
use serde_json::{Value, json};

/// A Codex app-server whose history holds turns 1 and 2; `fail` rejects the revert.
fn revert_replies(ops: Arc<FakeOps>, fail: bool) -> impl Fn(&Value) -> Vec<Value> + Send + Sync {
    move |frame| {
        let id = frame["id"].clone();
        match frame["method"].as_str() {
            Some("thread/read") => vec![
                json!({"id":id,"result":{"thread":{"historyMode":"paginated","status":{"type":"idle"}}}}),
            ],
            Some("thread/turns/list") => {
                vec![json!({"id":id,"result":{"data":[{"id":"turn-2"},{"id":"turn-1"}]}})]
            }
            Some("thread/revert") => {
                ops.record("provider");
                if fail {
                    vec![json!({"id":id,"error":{"code":-32000,"message":"revert failed"}})]
                } else {
                    vec![json!({"id":id,"result":{}})]
                }
            }
            _ => codex_replies(frame),
        }
    }
}

fn rollback_calls(ops: &FakeOps) -> Vec<String> {
    ops.logged()
        .into_iter()
        .filter_map(|entry| {
            let name = entry.split(' ').next().unwrap_or_default();
            ["prepare", "provider", "commit", "undo"]
                .contains(&name)
                .then(|| name.to_owned())
        })
        .collect()
}

async fn rollback(
    rig: &Rig,
    thread: &ThreadId,
    scope: &CheckpointScope,
    ordinal: u64,
    restore_files: bool,
) -> Reply {
    rig.command(
        thread,
        Command::Rollback {
            checkpoint: checkpoint_id(&scope.id, ordinal),
            restore_files,
        },
    )
    .await
}

fn rolled_back(state: &State) -> Vec<u64> {
    state
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::RolledBack)
        .map(|run| run.ordinal)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Shared {
    None,
    Root,
    Worktree,
    Historical,
}

// "rewinds safely with %s"
#[tokio::test(flavor = "multi_thread")]
async fn rewinds_safely() {
    for (restore_files, shared, target) in [
        (true, Shared::None, 0),
        (false, Shared::Root, 0),
        (true, Shared::Root, 0),
        (true, Shared::Worktree, 0),
        (false, Shared::Worktree, 0),
        (true, Shared::Historical, 0),
        (false, Shared::None, 1),
    ] {
        let rig = rig();
        rig.ops
            .projects
            .lock()
            .unwrap()
            .push(project("other", "/elsewhere"));
        rig.host.respond(revert_replies(rig.ops.clone(), false));
        let id = tid("rewind-files");
        let workspace = if shared == Shared::Root {
            root_workspace("/repo")
        } else {
            worktree("/wt")
        };
        let cwd = workspace.cwd.clone();
        let scope = rig.scoped(&id, workspace).await;
        rig.completed_run(&id, "first", "turn-1").await;
        rig.completed_run(&id, "second", "turn-2").await;
        let other = tid("other-thread");
        match shared {
            Shared::Worktree => {
                rig.create_in(&other, "other", Some(worktree(&cwd))).await;
                rig.command(&other, Command::Archive { archived: true })
                    .await;
            }
            Shared::Historical => {
                rig.create_in(&other, "other", None).await;
                rig.input(
                    &other,
                    Input::CheckpointScope {
                        run: None,
                        attempt: None,
                        scope: Some(checkpoint_scope(&other, &cwd)),
                    },
                )
                .await;
                rig.command(&other, Command::Archive { archived: true })
                    .await;
            }
            Shared::None | Shared::Root => {}
        }
        rig.ops.log.lock().unwrap().clear();

        assert_eq!(
            rollback(&rig, &id, &scope, target, restore_files).await,
            Reply::Accepted
        );
        rig.drain().await;

        let state = rig.state(&id).await;
        let case = format!("{restore_files} {shared:?} {target}");
        assert!(state.rollback.is_none(), "{case}");
        if restore_files && shared != Shared::None {
            assert_eq!(
                state.rollback_failure.as_deref(),
                Some(SHARED_WORKSPACE_RESTORE_MESSAGE),
                "{case}"
            );
            assert!(rollback_calls(&rig.ops).is_empty(), "{case}");
            assert!(rolled_back(&state).is_empty(), "{case}");
            continue;
        }
        assert_eq!(state.rollback_failure, None, "{case}");
        assert_eq!(
            rollback_calls(&rig.ops),
            if restore_files {
                vec!["prepare", "provider", "commit"]
            } else {
                vec!["provider"]
            },
            "{case}"
        );
        assert_eq!(
            rolled_back(&state),
            if target == 0 { vec![1, 2] } else { vec![2] },
            "{case}"
        );
        assert_eq!(
            rig.ops.logged_with("delete"),
            [format!(
                "delete {cwd} {}",
                if target == 0 { "1,2" } else { "2" }
            )],
            "{case}"
        );
        let stale: Vec<_> = state
            .checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.status == CheckpointStatus::Stale)
            .map(|checkpoint| checkpoint.run_ordinal)
            .collect();
        assert_eq!(
            stale,
            if target == 0 { vec![1, 2] } else { vec![2] },
            "{case}"
        );
        let revert = rig
            .host
            .last()
            .unwrap()
            .written()
            .into_iter()
            .find(|frame| frame["method"] == "thread/revert")
            .unwrap();
        assert_eq!(
            revert["params"]["beforeTurnId"],
            if target == 0 { "turn-1" } else { "turn-2" },
            "{case}"
        );
    }
}

// "rejects a non-ready checkpoint before opening a session or restoring files"
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_non_ready_checkpoint_before_any_provider_or_file_work() {
    let rig = rig();
    let id = tid("rewind-not-ready");
    rig.ops.not_git.lock().unwrap().insert("/wt".into());
    let scope = rig.scoped(&id, worktree("/wt")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    rig.ops.log.lock().unwrap().clear();

    assert_eq!(
        rollback(&rig, &id, &scope, 0, true).await,
        Reply::Rejected {
            reason: "checkpoint-not-ready".into()
        }
    );
    rig.drain().await;
    assert!(rig.ops.logged().is_empty());
    assert_eq!(rig.host.spawned(), 0);
    assert!(
        !rig.outbox_kinds(&id)
            .await
            .iter()
            .any(|(kind, _)| kind == "Rollback")
    );
}

// "reports a missing provider turn as a structured rollback failure", adapted:
// a provider that cannot rewind fails the rollback and keeps the files.
#[tokio::test(flavor = "multi_thread")]
async fn a_provider_that_cannot_rewind_fails_the_rollback_and_puts_the_files_back() {
    let rig = rig_with(RigOptions {
        max_attempts: 1,
        ..RigOptions::default()
    });
    rig.host.respond(revert_replies(rig.ops.clone(), true));
    let id = tid("rewind-provider-failure");
    let scope = rig.scoped(&id, worktree("/wt")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    rig.completed_run(&id, "second", "turn-2").await;
    rig.ops.log.lock().unwrap().clear();

    assert_eq!(rollback(&rig, &id, &scope, 1, true).await, Reply::Accepted);
    rig.drain().await;

    assert_eq!(rollback_calls(&rig.ops), ["prepare", "provider", "undo"]);
    assert!(rig.ops.logged_with("delete").is_empty());
    let state = rig.state(&id).await;
    assert_eq!(
        state.rollback_failure.as_deref(),
        Some(ROLLBACK_FAILED_MESSAGE)
    );
    assert!(rolled_back(&state).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rollback_without_a_native_session_resets_nothing_and_still_finishes() {
    let rig = rig();
    let id = tid("rewind-no-provider");
    let scope = rig.scoped(&id, worktree("/wt")).await;
    let run = rig.send(&id, "first", "first").await;
    rig.drain().await;
    let attempt = rig.attempt(&id, &run).await;
    rig.provider(
        &id,
        &attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Failed,
            native_head: None,
        },
    )
    .await;
    rig.drain().await;
    rig.ops.log.lock().unwrap().clear();

    assert_eq!(rollback(&rig, &id, &scope, 0, false).await, Reply::Accepted);
    rig.drain().await;

    let state = rig.state(&id).await;
    assert!(state.rollback.is_none());
    assert_eq!(state.rollback_failure, None);
    assert!(rollback_calls(&rig.ops).is_empty());
    assert_eq!(rig.host.spawned(), 0);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Owner {
    Nested,
    Ancestor,
    ArchivedNested,
    AliasedNested,
    AliasedWorktree,
    Project,
    Provider,
    Scope,
    Sibling,
    StoppedProvider,
    Conversation,
}

// CheckpointRestoreSafety.test.ts: "preserves overlapping workspace files, owner=%s",
// and "rejects an archived thread sharing a worktree through a symlink".
#[tokio::test(flavor = "multi_thread")]
async fn preserves_overlapping_workspace_files() {
    for owner in [
        Owner::Nested,
        Owner::Ancestor,
        Owner::ArchivedNested,
        Owner::AliasedNested,
        Owner::AliasedWorktree,
        Owner::Project,
        Owner::Provider,
        Owner::Scope,
        Owner::Sibling,
        Owner::StoppedProvider,
        Owner::Conversation,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().to_path_buf();
        let cwd = parent.join("worktree");
        let nested = cwd.join("nested");
        let sibling = parent.join("worktree2");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let other_file = nested.join("other-thread.txt");
        std::fs::write(&other_file, "other thread's uncommitted work").unwrap();
        let alias = parent.join("alias");
        match owner {
            Owner::AliasedNested => std::os::unix::fs::symlink(&nested, &alias).unwrap(),
            Owner::AliasedWorktree => std::os::unix::fs::symlink(&cwd, &alias).unwrap(),
            _ => {}
        }
        let text = |path: &std::path::Path| path.to_string_lossy().into_owned();
        let rig = rig();
        rig.ops.real_files.store(true, Ordering::SeqCst);
        *rig.ops.projects.lock().unwrap() = vec![project("project", &text(&parent))];
        rig.host.respond(revert_replies(rig.ops.clone(), false));
        let removed = other_file.clone();
        *rig.ops.on_restore.lock().unwrap() = Some(Arc::new(move || {
            let _ = std::fs::remove_file(&removed);
        }));
        let id = tid("restore-current");
        let scope = rig.scoped(&id, worktree(&text(&cwd))).await;
        rig.completed_run(&id, "first", "turn-1").await;

        let other = tid("restore-other");
        let other_workspace = match owner {
            Owner::Project => None,
            Owner::Ancestor | Owner::Conversation => Some(worktree(&text(&parent))),
            Owner::Nested | Owner::ArchivedNested => Some(worktree(&text(&nested))),
            Owner::AliasedNested | Owner::AliasedWorktree => Some(worktree(&text(&alias))),
            Owner::Provider | Owner::StoppedProvider => Some(worktree(&text(&nested))),
            Owner::Scope | Owner::Sibling => Some(worktree(&text(&sibling))),
        };
        rig.create(&other, other_workspace).await;
        if owner == Owner::Scope {
            rig.input(
                &other,
                Input::CheckpointScope {
                    run: None,
                    attempt: None,
                    scope: Some(checkpoint_scope(&other, &text(&nested))),
                },
            )
            .await;
        }
        if matches!(owner, Owner::Provider | Owner::StoppedProvider) {
            // A live provider process keeps the directory it was opened in.
            rig.sessions
                .rollback(
                    &other,
                    "codex",
                    &ProviderCommand::Rollback {
                        native_thread: "native-other".into(),
                        absolute_head: Some("turn-1".into()),
                    },
                )
                .await
                .unwrap();
            rig.input(
                &other,
                Input::Workspace {
                    workspace: Some(worktree(&text(&sibling))),
                },
            )
            .await;
            if owner == Owner::StoppedProvider {
                rig.sessions.detach(&other, false).await;
            }
        }
        if matches!(owner, Owner::ArchivedNested | Owner::AliasedWorktree) {
            rig.command(&other, Command::Archive { archived: true })
                .await;
        }
        rig.ops.log.lock().unwrap().clear();

        let restore_files = owner != Owner::Conversation;
        assert_eq!(
            rollback(&rig, &id, &scope, 0, restore_files).await,
            Reply::Accepted
        );
        rig.drain().await;

        let state = rig.state(&id).await;
        let rejected = !matches!(
            owner,
            Owner::Sibling | Owner::StoppedProvider | Owner::Conversation
        );
        if rejected {
            assert_eq!(
                state.rollback_failure.as_deref(),
                Some(SHARED_WORKSPACE_RESTORE_MESSAGE),
                "{owner:?}"
            );
            assert!(rollback_calls(&rig.ops).is_empty(), "{owner:?}");
            assert_eq!(
                std::fs::read_to_string(&other_file).unwrap(),
                "other thread's uncommitted work"
            );
        } else {
            assert_eq!(state.rollback_failure, None, "{owner:?}");
            assert_eq!(
                rollback_calls(&rig.ops),
                if restore_files {
                    vec!["prepare", "provider", "commit"]
                } else {
                    vec!["provider"]
                },
                "{owner:?}"
            );
            assert_eq!(other_file.exists(), !restore_files, "{owner:?}");
        }
    }
}
