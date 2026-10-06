//! ThreadLaunchService.test.ts.
use super::*;
use crate::executor::tests::{FakeOps, Rig, RigOptions, codex, rig, rig_with};
use crate::session::tests::fake::Gate;
use crate::{CreatedWorktree, DaemonOptions, EffectStatus};
use agent_domain::{AttachmentKind, EffectBody, ItemKind, RunStatus, State};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn request(
    command: &str,
    thread: Option<&str>,
    message: Option<&str>,
    workspace: WorkspaceStrategy,
) -> LaunchThread {
    LaunchThread {
        command: CommandId::new(command).unwrap(),
        thread: thread.map(|id| ThreadId::new(id).unwrap()),
        project: "project".into(),
        title: "New thread".into(),
        generate_title: false,
        selection: codex(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace,
        initial_message: message.map(|text| InitialMessage {
            id: Some(MessageId::new(format!("{text}:id")).unwrap()),
            text: text.into(),
            attachments: vec![],
            created_by: MessageAuthor::User,
            creation_source: "web".into(),
        }),
    }
}

fn worktree_strategy() -> WorkspaceStrategy {
    WorkspaceStrategy::Worktree {
        base_ref: "main".into(),
        branch: None,
        start_from_origin: false,
    }
}

fn root() -> WorkspaceStrategy {
    WorkspaceStrategy::Root { branch: None }
}

async fn launch_on(rig: &Rig, request: LaunchThread) -> Result<LaunchReply, LaunchError> {
    launch(&rig.context, request).await
}

async fn state(rig: &Rig, thread: &ThreadId) -> Arc<State> {
    rig.state(thread).await
}

fn gate_setup(ops: &FakeOps, gate: &Arc<Gate>) {
    let gate = gate.clone();
    *ops.setup.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = gate.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(())
        })
    }));
}

fn fail_setup_once(ops: &FakeOps, message: &str) {
    let failures = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let message = message.to_owned();
    *ops.setup.lock().unwrap() = Some(Arc::new(move |_| {
        let (failures, message) = (failures.clone(), message.clone());
        Box::pin(async move {
            if failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                Err(message)
            } else {
                Ok(())
            }
        })
    }));
}

async fn provider_starts(rig: &Rig, thread: &ThreadId) -> usize {
    rig.outbox_kinds(thread)
        .await
        .iter()
        .filter(|(kind, _)| kind == "Provider.Start")
        .count()
}

async fn until(what: &str, check: impl AsyncFn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn error_text(state: &State) -> Option<String> {
    state.items.iter().find_map(|item| match &item.kind {
        ItemKind::Error { message, .. } => Some(message.clone()),
        _ => None,
    })
}

// "returns a visible preparing message while provisioning is still blocked" and
// "enqueues provider work only after setup has been initiated".
#[tokio::test(flavor = "multi_thread")]
async fn returns_a_visible_preparing_message_while_provisioning_is_still_blocked() {
    let rig = Arc::new(rig());
    let worktree_gate = Arc::new(Gate::default());
    let gate = worktree_gate.clone();
    *rig.ops.worktree.lock().unwrap() = Some(Arc::new(move |request| {
        let gate = gate.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(CreatedWorktree {
                path: "/repo-worktrees/feature".into(),
                branch: request.branch.or(Some("feature".into())),
            })
        })
    }));
    let setup_gate = Arc::new(Gate::default());
    gate_setup(&rig.ops, &setup_gate);

    let launched = launch_on(
        &rig,
        request(
            "command:launch:blocked",
            Some("thread:launch:blocked"),
            Some("Build the feature"),
            worktree_strategy(),
        ),
    )
    .await
    .unwrap();
    let current = state(&rig, &launched.thread).await;
    assert_eq!(current.messages[0].text, "Build the feature");
    assert_eq!(current.runs[0].status, RunStatus::Preparing);

    let worker = rig.clone();
    let drained = tokio::spawn(async move { worker.drain().await });
    worktree_gate.until_arrived(1).await;
    assert_eq!(provider_starts(&rig, &launched.thread).await, 0);
    worktree_gate.release();
    setup_gate.until_arrived(1).await;
    let current = state(&rig, &launched.thread).await;
    assert_eq!(
        current
            .thread
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .cwd,
        "/repo-worktrees/feature"
    );
    assert_eq!(current.runs[0].status, RunStatus::Preparing);
    assert_eq!(provider_starts(&rig, &launched.thread).await, 0);
    setup_gate.release();
    drained.await.unwrap();

    let current = state(&rig, &launched.thread).await;
    assert_eq!(current.runs[0].status, RunStatus::Starting);
    assert_eq!(provider_starts(&rig, &launched.thread).await, 1);
    let scope = current.runs[0].checkpoint_scope.as_ref().unwrap();
    assert_eq!(scope.cwd, "/repo-worktrees/feature");
}

#[tokio::test(flavor = "multi_thread")]
async fn provisions_independent_launches_concurrently() {
    let rig = rig();
    let gate = Arc::new(Gate::default());
    gate_setup(&rig.ops, &gate);
    let _daemon = rig.worker.clone().spawn(DaemonOptions {
        concurrency: 4,
        liveness: Duration::from_millis(20),
    });
    for index in 0..2 {
        launch_on(
            &rig,
            request(
                &format!("command:launch:concurrent:{index}"),
                Some(&format!("thread:launch:concurrent:{index}")),
                Some("Run in parallel"),
                root(),
            ),
        )
        .await
        .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), gate.until_arrived(2))
        .await
        .expect("both setups run at once");
    gate.release();
}

// "queues follow-up messages behind preparation and checkpoints them in the final workspace"
#[tokio::test(flavor = "multi_thread")]
async fn queues_follow_up_messages_behind_preparation_in_the_final_workspace() {
    let rig = rig();
    fail_setup_once(&rig.ops, "setup failed");
    let launched = launch_on(
        &rig,
        request(
            "command:launch:queued-during-preparation",
            Some("thread:launch:queued-during-preparation"),
            Some("Prepare the workspace"),
            worktree_strategy(),
        ),
    )
    .await
    .unwrap();
    let follow_up = rig
        .command(
            &launched.thread,
            Command::Send(crate::executor::tests::message(
                "message:launch:queued-follow-up",
                "Run after preparation",
                DispatchMode::QueueAfterActive,
            )),
        )
        .await;
    let Reply::Run(follow_up) = follow_up else {
        panic!("{follow_up:?}")
    };
    assert_eq!(
        rig.run(&launched.thread, &follow_up).await.status,
        RunStatus::Queued
    );
    rig.drain().await;

    let queued = rig.run(&launched.thread, &follow_up).await;
    assert_eq!(queued.status, RunStatus::Starting);
    assert_eq!(
        queued.checkpoint_scope.unwrap().cwd,
        "/repo-worktrees/feature"
    );
}

// "%s failure keeps the thread and message visible and emits failure items"
#[tokio::test(flavor = "multi_thread")]
async fn a_preparation_failure_keeps_the_thread_and_message_visible() {
    for point in ["worktree", "setup"] {
        let rig = rig();
        let failure = format!("{point} failed");
        if point == "worktree" {
            let failure = failure.clone();
            *rig.ops.worktree.lock().unwrap() = Some(Arc::new(move |_| {
                let failure = failure.clone();
                Box::pin(async move { Err(failure) })
            }));
        } else {
            fail_setup_once(&rig.ops, &failure);
        }
        let launched = launch_on(
            &rig,
            request(
                &format!("command:launch:{point}-failure"),
                Some(&format!("thread:launch:{point}-failure")),
                Some(&format!("Fail during {point}")),
                worktree_strategy(),
            ),
        )
        .await
        .unwrap();
        rig.drain().await;

        let current = state(&rig, &launched.thread).await;
        assert_eq!(current.messages[0].text, format!("Fail during {point}"));
        assert_eq!(current.runs[0].status, RunStatus::Failed, "{point}");
        let error = error_text(&current).unwrap();
        assert!(error.contains(&failure), "{error}");
        assert!(
            error.starts_with(if point == "worktree" {
                "Workspace preparation failed during provision worktree: "
            } else {
                "Workspace preparation failed during run setup script: "
            }),
            "{error}"
        );
    }
}

// "retries a failed workspace preparation on the same run"
#[tokio::test(flavor = "multi_thread")]
async fn retries_a_failed_workspace_preparation_on_the_same_run() {
    let rig = rig();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = attempts.clone();
    *rig.ops.worktree.lock().unwrap() = Some(Arc::new(move |_| {
        let attempt = counter.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if attempt == 0 {
                Err("Git could not update a local reference.".into())
            } else {
                Ok(CreatedWorktree {
                    path: "/repo-worktrees/feature".into(),
                    branch: Some("feature".into()),
                })
            }
        })
    }));
    let launched = launch_on(
        &rig,
        request(
            "command:launch:retry",
            Some("thread:launch:retry"),
            Some("Retry me"),
            WorkspaceStrategy::Worktree {
                base_ref: "main".into(),
                branch: None,
                start_from_origin: true,
            },
        ),
    )
    .await
    .unwrap();
    rig.drain().await;
    let failed = state(&rig, &launched.thread).await;
    let run = failed.runs[0].id.clone();
    assert_eq!(failed.runs[0].status, RunStatus::Failed);
    assert_eq!(
        error_text(&failed).as_deref(),
        Some(
            "Workspace preparation failed during provision worktree: Git could not update a local reference."
        )
    );

    assert_eq!(
        rig.command(
            &launched.thread,
            Command::RetryPrepared { run: run.clone() }
        )
        .await,
        Reply::Accepted
    );
    rig.drain().await;
    let retried = state(&rig, &launched.thread).await;
    assert_eq!(retried.runs.len(), 1);
    assert_eq!(retried.runs[0].status, RunStatus::Starting);
    assert_eq!(
        retried
            .thread
            .as_ref()
            .unwrap()
            .workspace
            .as_ref()
            .unwrap()
            .worktree_path
            .as_deref(),
        Some("/repo-worktrees/feature")
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    // The run left preparation, so a second retry has nothing to do.
    assert_eq!(
        rig.command(&launched.thread, Command::RetryPrepared { run })
            .await,
        Reply::Rejected {
            reason: "run-not-retryable".into()
        }
    );
}

// "a retry reuses a recorded worktree without undoing its branch rename"
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_reuses_a_recorded_worktree() {
    let rig = rig();
    fail_setup_once(&rig.ops, "setup failed");
    let launched = launch_on(
        &rig,
        request(
            "command:launch:reuse",
            Some("thread:launch:reuse"),
            Some("Reuse the worktree"),
            worktree_strategy(),
        ),
    )
    .await
    .unwrap();
    rig.drain().await;
    let failed = state(&rig, &launched.thread).await;
    assert_eq!(failed.runs[0].status, RunStatus::Failed);
    let workspace = failed.thread.as_ref().unwrap().workspace.clone().unwrap();
    assert_eq!(
        workspace.worktree_path.as_deref(),
        Some("/repo-worktrees/feature")
    );

    rig.command(
        &launched.thread,
        Command::RetryPrepared {
            run: failed.runs[0].id.clone(),
        },
    )
    .await;
    rig.drain().await;
    let retried = state(&rig, &launched.thread).await;
    assert_eq!(retried.runs[0].status, RunStatus::Starting);
    assert_eq!(rig.ops.logged_with("worktree").len(), 1);
    assert_eq!(retried.thread.as_ref().unwrap().workspace, Some(workspace));
}

fn without_thread(command: &str, message: Option<&str>) -> LaunchThread {
    request(command, None, message, root())
}

// "replays a server-allocated launch"
#[tokio::test(flavor = "multi_thread")]
async fn replays_a_server_allocated_launch() {
    let rig = Arc::new(rig());
    let gate = Arc::new(Gate::default());
    gate_setup(&rig.ops, &gate);
    let input = without_thread("command:launch:allocated-retry", Some("Only once"));
    let first = launch_on(&rig, input.clone()).await.unwrap();
    let worker = rig.clone();
    let drained = tokio::spawn(async move { worker.drain().await });
    gate.until_arrived(1).await;
    let retry = launch_on(&rig, input.clone()).await.unwrap();
    assert_eq!(first.thread, retry.thread);
    assert!(!first.resumed);
    assert!(retry.resumed);
    let current = state(&rig, &first.thread).await;
    assert_eq!(current.messages.len(), 1);
    assert_eq!(current.runs.len(), 1);
    assert_eq!(retry.committed.reply, first.committed.reply);
    gate.release();
    drained.await.unwrap();
    rig.drain().await;

    let settled = launch_on(&rig, input).await.unwrap();
    assert_eq!(settled.thread, first.thread);
    assert!(settled.resumed);
    assert_eq!(settled.committed.reply, first.committed.reply);
    assert_eq!(rig.ops.logged_with("setup").len(), 1);
}

// "rejects a server-allocated launch replay with a mismatching thread id"
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_launch_replay_with_a_mismatching_thread_id() {
    let rig = rig();
    let input = without_thread("command:launch:allocated-mismatch", Some("Mismatch"));
    let first = launch_on(&rig, input.clone()).await.unwrap();
    let mismatch = ThreadId::new("thread:launch:allocated-mismatch").unwrap();
    let failed = launch_on(
        &rig,
        LaunchThread {
            thread: Some(mismatch.clone()),
            ..input
        },
    )
    .await
    .unwrap_err();
    assert_ne!(first.thread, mismatch);
    assert_eq!(failed.operation, LaunchOperation::CreateThread);
    assert!(
        failed.cause.contains("cannot be replayed"),
        "{}",
        failed.cause
    );
}

// "rejects a server-allocated launch receipt from another project"
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_launch_replay_from_another_project() {
    let rig = rig();
    rig.ops
        .projects
        .lock()
        .unwrap()
        .push(crate::executor::tests::project("other", "/other"));
    let input = without_thread(
        "command:launch:allocated-wrong-project",
        Some("Wrong project"),
    );
    let first = launch_on(&rig, input.clone()).await.unwrap();
    let failed = launch_on(
        &rig,
        LaunchThread {
            project: "other".into(),
            ..input
        },
    )
    .await
    .unwrap_err();
    assert_eq!(failed.operation, LaunchOperation::ResolveProject);
    assert_eq!(failed.thread, Some(first.thread));
    assert_eq!(failed.cause, "Project identity changed.");
}

// "rejects a server-allocated launch retry after the thread is deleted"
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_launch_replay_after_the_thread_is_deleted() {
    let rig = rig();
    let input = without_thread(
        "command:launch:allocated-deleted",
        Some("Deleted before retry"),
    );
    let first = launch_on(&rig, input.clone()).await.unwrap();
    rig.command(&first.thread, Command::Delete).await;
    let failed = launch_on(&rig, input).await.unwrap_err();
    assert_eq!(failed.operation, LaunchOperation::CreateThread);
    assert_eq!(failed.thread, Some(first.thread));
    assert_eq!(failed.cause, "Thread not found.");
}

// "does not treat an unrelated accepted command receipt as a launch"
#[tokio::test(flavor = "multi_thread")]
async fn does_not_treat_an_unrelated_accepted_command_receipt_as_a_launch() {
    let rig = rig();
    let existing = ThreadId::new("thread:launch:unrelated-receipt").unwrap();
    rig.create(&existing, None).await;
    rig.registry
        .dispatch(
            &existing,
            CommandId::new("command:launch:unrelated-receipt").unwrap(),
            Command::Rename {
                title: "Existing".into(),
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let failed = launch_on(
        &rig,
        without_thread(
            "command:launch:unrelated-receipt",
            Some("Should not become a launch"),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(failed.operation, LaunchOperation::CreateThread);
    assert!(
        failed.cause.contains("cannot be replayed"),
        "{}",
        failed.cause
    );
    let current = state(&rig, &existing).await;
    assert!(current.messages.is_empty());
    assert!(current.runs.is_empty());
    assert_eq!(
        rig.store
            .launch(&CommandId::new("command:launch:unrelated-receipt").unwrap())
            .unwrap(),
        None
    );
}

// "bounds concurrent first launches to one thread per command" and
// "deduplicates retried launch side effects in-process"
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_launches_of_one_command_share_one_thread_and_one_preparation() {
    for thread in [None, Some("thread:launch:retry-concurrent")] {
        let rig = rig();
        let input = request(
            "command:launch:concurrent-allocated",
            thread,
            Some("Race me"),
            root(),
        );
        let (first, second) = tokio::join!(
            launch_on(&rig, input.clone()),
            launch_on(&rig, input.clone())
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_eq!(first.thread, second.thread);
        assert_eq!(
            [first.resumed, second.resumed]
                .iter()
                .filter(|resumed| !**resumed)
                .count(),
            1
        );
        let current = state(&rig, &first.thread).await;
        assert_eq!(current.messages.len(), 1);
        assert_eq!(current.runs.len(), 1);
        rig.drain().await;
        assert_eq!(rig.ops.logged_with("setup").len(), 1);
        assert_eq!(rig.store.live_threads().unwrap().len(), 1);
    }
}

// "schedules an accepted preparing message exactly once across concurrent retries"
#[tokio::test(flavor = "multi_thread")]
async fn schedules_an_accepted_preparing_message_exactly_once() {
    let rig = rig();
    let input = request(
        "command:launch:accepted-before-fork",
        Some("thread:launch:accepted-before-fork"),
        Some("Resume preparation"),
        root(),
    );
    let thread = input.thread.clone().unwrap();
    rig.registry
        .dispatch(
            &thread,
            input.command.clone(),
            Command::Create {
                thread: thread.clone(),
                project: "project".into(),
                title: input.title.clone(),
                selection: input.selection.clone(),
                runtime_mode: input.runtime_mode,
                interaction_mode: input.interaction_mode,
                workspace: Some(Workspace {
                    cwd: "/repo".into(),
                    worktree_path: None,
                    branch: None,
                }),
            },
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    let mut message = crate::executor::tests::message(
        "Resume preparation:id",
        "Resume preparation",
        DispatchMode::DeferStart,
    );
    message.selection = Some(input.selection.clone());
    rig.registry
        .dispatch(
            &thread,
            CommandId::new(format!("{}:initial-message", input.command)).unwrap(),
            Command::Send(message),
            CommandOrigin::Client,
        )
        .await
        .unwrap();
    assert_eq!(
        state(&rig, &thread).await.runs[0].status,
        RunStatus::Preparing
    );

    let (first, second) = tokio::join!(launch_on(&rig, input.clone()), launch_on(&rig, input));
    assert!(first.unwrap().resumed);
    assert!(second.unwrap().resumed);
    rig.drain().await;
    assert_eq!(rig.ops.logged_with("setup").len(), 1);
}

// "arms durable title generation after accepting the first message"
#[tokio::test(flavor = "multi_thread")]
async fn arms_durable_title_generation_after_accepting_the_first_message() {
    let rig = rig();
    let input = LaunchThread {
        title: "Generate my title".into(),
        generate_title: true,
        ..request(
            "command:launch:title-generation",
            Some("thread:launch:title-generation"),
            Some("Generate my title"),
            root(),
        )
    };
    let launched = launch_on(&rig, input).await.unwrap();
    let current = state(&rig, &launched.thread).await;
    let thread = current.thread.as_ref().unwrap();
    assert_eq!(thread.title, "Generate my title");
    let request = thread.title_request.clone().unwrap();
    let job = rig.job(&launched.thread, "GenerateTitle").await;
    assert_eq!(
        job.effect.body,
        EffectBody::GenerateTitle {
            request,
            message: Some(MessageId::new("Generate my title:id").unwrap()),
        }
    );
    rig.drain().await;
    assert_eq!(
        state(&rig, &launched.thread)
            .await
            .thread
            .as_ref()
            .unwrap()
            .title,
        "Generated title"
    );

    rig.ops
        .titles
        .lock()
        .unwrap()
        .push_back(Ok(r#"{"title":"Regenerated title"}"#.into()));
    rig.command(&launched.thread, Command::RegenerateTitle)
        .await;
    rig.drain().await;
    let regenerated = state(&rig, &launched.thread).await;
    assert_eq!(
        regenerated.thread.as_ref().unwrap().title,
        "Regenerated title"
    );
    let calls = rig.ops.generations.lock().unwrap().clone();
    assert!(
        calls[1]
            .prompt
            .contains("The previous title was \"Generated title\".")
    );
}

// "generates an initial title for an attachment-only message"
#[tokio::test(flavor = "multi_thread")]
async fn generates_an_initial_title_for_an_attachment_only_message() {
    let rig = rig();
    let mut input = request(
        "command:launch:image-only",
        Some("thread:launch:image-only"),
        None,
        root(),
    );
    input.title = "Image: screenshot.png".into();
    input.generate_title = true;
    input.initial_message = Some(InitialMessage {
        id: Some(MessageId::new("message:image-only").unwrap()),
        text: String::new(),
        attachments: vec![Attachment {
            kind: AttachmentKind::Image,
            source: None,
            id: "attachment-image-only".into(),
            name: "screenshot.png".into(),
            mime_type: "image/png".into(),
            path: "/attachments/screenshot.png".into(),
            size: 128,
        }],
        created_by: MessageAuthor::User,
        creation_source: "web".into(),
    });
    launch_on(&rig, input).await.unwrap();
    rig.drain().await;

    let calls = rig.ops.generations.lock().unwrap().clone();
    assert!(
        calls[0].prompt.contains("User message:\n\n"),
        "{}",
        calls[0].prompt
    );
    assert_eq!(calls[0].attachments[0].name, "screenshot.png");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_without_a_message_prepares_its_worktree_in_the_background() {
    let rig = rig_with(RigOptions::default());
    let launched = launch_on(
        &rig,
        request(
            "command:launch:empty",
            Some("thread:launch:empty"),
            None,
            worktree_strategy(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        launched.committed.reply,
        Reply::Thread(launched.thread.clone())
    );
    until("worktree bound", async || {
        state(&rig, &launched.thread)
            .await
            .checkpoint_scope
            .as_ref()
            .is_some_and(|scope| scope.cwd == "/repo-worktrees/feature")
    })
    .await;
    assert!(
        rig.outbox_kinds(&launched.thread)
            .await
            .iter()
            .all(|(_, status)| *status == EffectStatus::Succeeded)
    );
}
