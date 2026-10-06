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
    ErroredProvider,
    SharedProvider,
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
        Owner::ErroredProvider,
        Owner::SharedProvider,
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
            Owner::Provider
            | Owner::StoppedProvider
            | Owner::ErroredProvider
            | Owner::SharedProvider => Some(worktree(&text(&nested))),
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
        if matches!(
            owner,
            Owner::Provider
                | Owner::StoppedProvider
                | Owner::ErroredProvider
                | Owner::SharedProvider
        ) {
            // A thread's own live provider process keeps the directory it was
            // opened in; the shared app-server runs each turn in its thread's
            // workspace (an errored process is still live).
            if owner == Owner::SharedProvider {
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
            } else {
                crate::session::tests::live_claude_process(&rig.sessions, &other, &text(&nested))
                    .await;
            }
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
            Owner::Sibling | Owner::StoppedProvider | Owner::SharedProvider | Owner::Conversation
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

/// A thread in `/wt` with two completed runs, the files of both and a rollback to
/// the first run's checkpoint accepted.
async fn rolled_back_once(rig: &Rig, id: &ThreadId) -> CheckpointScope {
    rig.host.respond(revert_replies(rig.ops.clone(), false));
    let scope = rig.scoped(id, worktree("/wt")).await;
    rig.completed_run(id, "first", "turn-1").await;
    rig.completed_run(id, "second", "turn-2").await;
    rig.ops.log.lock().unwrap().clear();
    assert_eq!(rollback(rig, id, &scope, 1, true).await, Reply::Accepted);
    scope
}

// The originals are discarded only after the rollback is recorded, so a later
// failure can neither lose the user's files nor report a rollback that happened
// as failed.
#[tokio::test(flavor = "multi_thread")]
async fn a_failure_after_the_rollback_is_recorded_keeps_the_restored_files() {
    let rig = rig_with(RigOptions {
        max_attempts: 1,
        ..RigOptions::default()
    });
    let id = tid("rewind-recorded-then-failed");
    rig.ops.fail_delete.store(true, Ordering::SeqCst);
    let scope = rolled_back_once(&rig, &id).await;
    rig.drain().await;

    let state = rig.state(&id).await;
    assert_eq!(state.rollback_failure, None);
    assert_eq!(rolled_back(&state), [2]);
    assert_eq!(rollback_calls(&rig.ops), ["prepare", "provider", "commit"]);
    assert!(
        rig.outbox_kinds(&id)
            .await
            .contains(&("Rollback".to_owned(), crate::EffectStatus::Failed))
    );

    // The next turn's baseline replaces the stale ref the rollback left behind.
    rig.ops.fail_delete.store(false, Ordering::SeqCst);
    rig.ops.log.lock().unwrap().clear();
    let next = rig.send(&id, "third", "third").await;
    rig.drain().await;
    assert_eq!(rig.run(&id, &next).await.ordinal, 3);
    let reference = checkpoint_reference(&scope.id, 2);
    assert_eq!(
        rig.ops
            .logged()
            .into_iter()
            .filter(|entry| entry.starts_with("delete") || entry.starts_with("capture"))
            .collect::<Vec<_>>(),
        ["delete /wt 2", "capture /wt 2"]
    );
    assert!(
        rig.ops
            .refs
            .lock()
            .unwrap()
            .contains(&("/wt".into(), reference))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_a_failed_commit_finishes_the_recorded_restore() {
    let rig = rig_with(RigOptions {
        max_attempts: 2,
        ..RigOptions::default()
    });
    let id = tid("rewind-commit-failed");
    rig.ops.fail_commit.store(true, Ordering::SeqCst);
    rolled_back_once(&rig, &id).await;
    rig.drain().await;
    rig.clock.advance(1_000);
    rig.drain().await;

    let state = rig.state(&id).await;
    assert_eq!(state.rollback_failure, None);
    assert_eq!(rolled_back(&state), [2]);
    let calls: Vec<_> = rig
        .ops
        .logged()
        .into_iter()
        .filter(|entry| !entry.starts_with("lookup") && !entry.starts_with("capture"))
        .collect();
    assert_eq!(
        calls,
        [
            "prepare /wt 1",
            "provider",
            "commit",
            "finish-restore /wt",
            "delete /wt 2",
        ]
    );
}

// A provider that rewound before another one failed no longer matches the kept
// conversation, so its native session is replaced by portable history.
#[tokio::test(flavor = "multi_thread")]
async fn a_partially_rewound_rollback_resets_the_rewound_native_sessions() {
    let rig = rig_with(RigOptions {
        max_attempts: 1,
        ..RigOptions::default()
    });
    let reverts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (ops, counter) = (rig.ops.clone(), reverts.clone());
    rig.host.respond(move |frame| {
        if frame["method"] == "thread/revert" {
            let id = frame["id"].clone();
            ops.record("provider");
            return if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![json!({"id":id,"result":{}})]
            } else {
                vec![json!({"id":id,"error":{"code":-32000,"message":"revert failed"}})]
            };
        }
        revert_replies(ops.clone(), false)(frame)
    });
    let id = tid("rewind-two-providers");
    let scope = rig.scoped(&id, worktree("/wt")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    let mut other = codex();
    other.instance = "codex-other".into();
    rig.command(&id, Command::SwitchProvider { selection: other })
        .await;
    rig.completed_run(&id, "second", "turn-2").await;
    let before = rig.state(&id).await;
    assert!(before.native_sessions.contains_key("codex"));
    assert!(before.native_sessions.contains_key("codex-other"));
    rig.ops.log.lock().unwrap().clear();

    assert_eq!(rollback(&rig, &id, &scope, 0, true).await, Reply::Accepted);
    rig.drain().await;

    assert_eq!(
        rollback_calls(&rig.ops),
        ["prepare", "provider", "provider", "undo"]
    );
    let state = rig.state(&id).await;
    assert_eq!(
        state.rollback_failure.as_deref(),
        Some(ROLLBACK_FAILED_MESSAGE)
    );
    assert!(rolled_back(&state).is_empty());
    assert!(
        state.native_sessions.is_empty(),
        "{:?}",
        state.native_sessions
    );
}

// A thread that binds a workspace while a restore is in progress waits for it,
// so the restore never overwrites work it did not check for.
#[tokio::test(flavor = "multi_thread")]
async fn a_launch_into_the_restored_worktree_waits_for_the_restore() {
    let rig = Arc::new(rig());
    rig.host.respond(revert_replies(rig.ops.clone(), false));
    let id = tid("rewind-fenced");
    let scope = rig.scoped(&id, worktree("/wt")).await;
    rig.completed_run(&id, "first", "turn-1").await;
    rig.ops.log.lock().unwrap().clear();
    let launched = Arc::new(Mutex::new(None));
    let (context, ops, slot) = (rig.context.clone(), rig.ops.clone(), launched.clone());
    let started = Arc::new(AtomicBool::new(false));
    let once = started.clone();
    *rig.ops.on_restore.lock().unwrap() = Some(Arc::new(move || {
        if once.swap(true, Ordering::SeqCst) {
            return;
        }
        let (context, ops) = (context.clone(), ops.clone());
        *slot.lock().unwrap() = Some(tokio::spawn(async move {
            let preparations = crate::Preparations::default();
            let reply = crate::launch::launch(
                &context,
                &preparations,
                crate::LaunchThread {
                    command: CommandId::new("command:launch:into-restore").unwrap(),
                    thread: Some(tid("thread:into-restore")),
                    project: "project".into(),
                    title: "Same worktree".into(),
                    generate_title: false,
                    selection: codex(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                    workspace: crate::WorkspaceStrategy::ExistingWorktree {
                        path: "/wt".into(),
                        branch: None,
                    },
                    initial_message: None,
                },
            )
            .await;
            ops.record("launched");
            reply
        }));
    }));

    assert_eq!(rollback(&rig, &id, &scope, 0, true).await, Reply::Accepted);
    rig.drain().await;
    let task = launched.lock().unwrap().take().unwrap();
    task.await.unwrap().unwrap();

    let calls: Vec<_> = rig
        .ops
        .logged()
        .into_iter()
        .filter(|entry| {
            ["prepare", "provider", "commit", "launched"]
                .contains(&entry.split(' ').next().unwrap())
        })
        .map(|entry| entry.split(' ').next().unwrap().to_owned())
        .collect();
    assert_eq!(calls, ["prepare", "provider", "commit", "launched"]);
    assert_eq!(rig.state(&id).await.rollback_failure, None);
}
