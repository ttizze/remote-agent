use super::*;

fn row(s: &State, run: &RunId) -> Item {
    s.items
        .iter()
        .find(|item| {
            item.run.as_ref() == Some(run)
                && matches!(&item.kind, ItemKind::CommandExecution { command, .. }
                    if command == WORKSPACE_PREPARATION_INPUT)
        })
        .unwrap()
        .clone()
}
fn title(item: &Item) -> Option<&str> {
    match &item.kind {
        ItemKind::CommandExecution { title, .. } => title.as_deref(),
        _ => None,
    }
}
fn exit_code(item: &Item) -> Option<i64> {
    match &item.kind {
        ItemKind::CommandExecution { exit_code, .. } => *exit_code,
        _ => None,
    }
}
fn deferred(s: &mut State, key: &str) -> RunId {
    let Reply::Run(run) = command(s, key, send_message(key, DispatchMode::DeferStart)).reply else {
        panic!()
    };
    run
}
fn kinds(s: &State, run: &RunId) -> Vec<&'static str> {
    s.items
        .iter()
        .filter(|item| item.run.as_ref() == Some(run))
        .map(|item| match item.kind {
            ItemKind::UserMessage { .. } => "user_message",
            ItemKind::CommandExecution { .. } => "command_execution",
            ItemKind::RunInterruptRequest => "run_interrupt_request",
            ItemKind::RunInterruptResult { .. } => "run_interrupt_result",
            ItemKind::Error { .. } => "error",
            _ => "other",
        })
        .collect()
}

// T3 Orchestrator.ts message dispatch with defer_start and
// ThreadLaunchService.test.ts "returns a visible preparing message while
// provisioning is still blocked".
#[test]
fn a_deferred_run_shows_its_message_and_a_running_preparation_row() {
    let mut s = state();
    let run = deferred(&mut s, "launch");
    assert_eq!(s.runs[0].status, RunStatus::Preparing);
    assert_eq!(kinds(&s, &run), ["user_message", "command_execution"]);
    let preparing = row(&s, &run);
    assert_eq!(preparing.status, ItemStatus::Running);
    assert_eq!(title(&preparing), Some("Preparing workspace"));

    for (phase, expected) in [
        (PreparationPhase::Worktree, "Preparing worktree"),
        (PreparationPhase::Setup, "Starting setup script"),
    ] {
        let progress = command(
            &mut s,
            &format!("progress-{expected}"),
            Command::PreparedRunProgress {
                run: run.clone(),
                phase,
            },
        );
        assert_eq!(progress.reply, Reply::Accepted);
        assert_eq!(title(&row(&s, &run)), Some(expected));
    }

    command(
        &mut s,
        "release",
        Command::ReleasePrepared { run: run.clone() },
    );
    let ready = row(&s, &run);
    assert_eq!(s.runs[0].status, RunStatus::Starting);
    assert_eq!(ready.status, ItemStatus::Completed);
    assert_eq!(title(&ready), Some("Workspace ready"));
    assert_eq!(ready.text, "Workspace preparation completed.");
    assert_eq!(exit_code(&ready), Some(0));
    assert_eq!(
        command(
            &mut s,
            "late-progress",
            Command::PreparedRunProgress {
                run,
                phase: PreparationPhase::Setup,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "run-not-preparing".into()
        }
    );
}

// T3 Orchestrator.ts dispatchPreparedRunFail / dispatchPreparedRunRetry and
// ThreadLaunchService.test.ts "%s failure keeps the thread and message visible
// and emits failure items" and "retries a failed workspace preparation on the
// same run".
#[test]
fn a_failed_preparation_row_fails_and_a_retry_runs_it_again() {
    let mut s = state();
    let run = deferred(&mut s, "launch");
    command(
        &mut s,
        "fail",
        Command::FailPrepared {
            run: run.clone(),
            message: "Workspace preparation failed during run setup script: setup failed".into(),
        },
    );
    let failed = row(&s, &run);
    assert_eq!(s.runs[0].status, RunStatus::Failed);
    assert_eq!(failed.status, ItemStatus::Failed);
    assert_eq!(title(&failed), Some("Workspace preparation failed"));
    assert_eq!(exit_code(&failed), Some(1));
    assert!(failed.text.contains("setup failed"));
    let error = s
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::Error { .. }))
        .unwrap();
    assert!(
        matches!(&error.kind, ItemKind::Error { message, code: Some(code), .. }
        if message.contains("setup failed") && code == WORKSPACE_PREPARATION_FAILURE_CODE)
    );

    command(&mut s, "retry", Command::RetryPrepared { run: run.clone() });
    let retried = row(&s, &run);
    assert_eq!(s.runs[0].status, RunStatus::Preparing);
    assert_eq!(retried.status, ItemStatus::Running);
    assert_eq!(title(&retried), Some("Preparing workspace"));
    assert_eq!(exit_code(&retried), None);
    assert_eq!(retried.text, "");
    assert_eq!(
        s.items
            .iter()
            .find(|item| matches!(item.kind, ItemKind::Error { .. }))
            .unwrap()
            .status,
        ItemStatus::Cancelled
    );
    command(
        &mut s,
        "release",
        Command::ReleasePrepared { run: run.clone() },
    );
    assert_eq!(row(&s, &run).status, ItemStatus::Completed);
}

// T3 Orchestrator.ts dispatchRunInterrupt before the provider turn starts.
#[test]
fn interrupting_a_preparing_run_interrupts_its_preparation_row() {
    let mut s = state();
    let run = deferred(&mut s, "launch");
    command(
        &mut s,
        "stop",
        Command::Interrupt {
            run: run.clone(),
            hold_queue: false,
            reason: Some("Stopped by the user".into()),
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Interrupted);
    let preparation = row(&s, &run);
    assert_eq!(preparation.status, ItemStatus::Interrupted);
    assert_eq!(
        title(&preparation),
        Some("Workspace preparation interrupted")
    );
    assert_eq!(preparation.text, "Stopped by the user");
    let request = s
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::RunInterruptRequest))
        .unwrap();
    assert_eq!(request.text, "Stopped by the user");
    let result = s
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::RunInterruptResult { .. }))
        .unwrap();
    assert_eq!(result.status, ItemStatus::Interrupted);
    assert_eq!(result.text, "Run interrupted before provider start");
}

// T3 runtimeLayer.test.ts "interrupts a pending provider start without
// launching provider work". The provider interrupt effect that cancels the
// in-flight start is this runtime's own mechanism.
#[test]
fn interrupts_a_pending_provider_start() {
    let mut s = state();
    let (run, attempt) = starting(&mut s, "pending");
    assert_eq!(s.runs[0].status, RunStatus::Starting);
    command(
        &mut s,
        "interrupt",
        Command::Interrupt {
            run: run.clone(),
            hold_queue: false,
            reason: Some("Cancelled before provider start".into()),
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Interrupted);
    assert_eq!(
        s.attempts
            .iter()
            .find(|candidate| candidate.id == attempt)
            .unwrap()
            .status,
        AttemptStatus::Interrupted
    );
    assert_eq!(
        kinds(&s, &run),
        [
            "user_message",
            "run_interrupt_request",
            "run_interrupt_result"
        ]
    );
}

// T3 RunExecutionService.ts makeInterruptResultTurnItem.
#[test]
fn a_confirmed_stop_records_the_default_request_and_result_messages() {
    let mut s = state();
    let (run, attempt) = running(&mut s, "running");
    command(
        &mut s,
        "interrupt",
        Command::Interrupt {
            run: run.clone(),
            hold_queue: false,
            reason: None,
        },
    );
    provider(
        &mut s,
        "aborted",
        &attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Interrupted,
            native_head: None,
        },
    );
    let text = |kind: fn(&ItemKind) -> bool| {
        s.items
            .iter()
            .find(|item| kind(&item.kind))
            .unwrap()
            .text
            .clone()
    };
    assert_eq!(
        text(|kind| matches!(kind, ItemKind::RunInterruptRequest)),
        "Interrupt requested"
    );
    assert_eq!(
        text(|kind| matches!(kind, ItemKind::RunInterruptResult { .. })),
        "Run interrupted by user"
    );
}
