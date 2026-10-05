use crate::*;
use std::collections::BTreeMap;
fn selection() -> ModelSelection {
    ModelSelection {
        instance: "codex".into(),
        driver: Driver::Codex,
        model: "gpt-6-luna".into(),
        options: BTreeMap::new(),
    }
}
fn at() -> Timestamp {
    Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
}
fn command(state: &mut State, key: &str, command: Command) -> Step {
    let result = ThreadMachine::step(
        state,
        &InputEnvelope {
            at: at(),
            key: key.into(),
            input: Input::Command {
                id: CommandId::new(key).unwrap(),
                command: Box::new(command),
                receipt: None,
            },
        },
    );
    *state = fold(state, &result.facts).unwrap();
    result
}
fn state() -> State {
    let mut s = State::default();
    command(
        &mut s,
        "create",
        Command::Create {
            thread: ThreadId::new("thread").unwrap(),
            project: "project".into(),
            title: "Thread".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: None,
        },
    );
    s
}
#[test]
fn creation_is_a_pure_replayable_decision() {
    let s = state();
    assert_eq!(s.thread.unwrap().title, "Thread");
}
fn send_message(key: &str, mode: DispatchMode) -> Command {
    Command::Send(SendMessage {
        created_by: MessageAuthor::User,
        creation_source: "client".into(),
        id: MessageId::new(key).unwrap(),
        text: key.into(),
        attachments: vec![],
        selection: None,
        mode,
        intent: None,
        source_plan: None,
        title_seed: None,
    })
}
fn provider(s: &mut State, key: &str, attempt: &RunAttemptId, event: ProviderEvent) -> Step {
    let result = ThreadMachine::step(
        s,
        &InputEnvelope {
            at: at(),
            key: key.into(),
            input: Input::Provider {
                attempt: attempt.clone(),
                event: Box::new(event),
            },
        },
    );
    *s = fold(s, &result.facts).unwrap();
    result
}
fn running(s: &mut State, key: &str) -> (RunId, RunAttemptId) {
    let Reply::Run(run) = command(s, key, send_message(key, DispatchMode::StartImmediately)).reply
    else {
        panic!()
    };
    let attempt = s
        .runs
        .iter()
        .find(|r| r.id == run)
        .unwrap()
        .attempt
        .clone()
        .unwrap();
    provider(
        s,
        "session",
        &attempt,
        ProviderEvent::SessionReady {
            native_thread: "native-thread".into(),
        },
    );
    provider(
        s,
        "started",
        &attempt,
        ProviderEvent::TurnStarted {
            native_turn: Some(key.into()),
        },
    );
    (run, attempt)
}
/// A run whose provider has not accepted the turn yet, so completions queue.
fn starting(s: &mut State, key: &str) -> (RunId, RunAttemptId) {
    let Reply::Run(run) = command(s, key, send_message(key, DispatchMode::StartImmediately)).reply
    else {
        panic!()
    };
    let attempt = s.active_run().unwrap().attempt.clone().unwrap();
    (run, attempt)
}
fn finish(s: &mut State, attempt: &RunAttemptId) {
    provider(
        s,
        "finish",
        attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: Some("native-head".into()),
        },
    );
}
fn result(s: &mut State, key: &str, event: EffectResult) -> Step {
    let r = ThreadMachine::step(
        s,
        &InputEnvelope {
            at: at(),
            key: key.into(),
            input: Input::Effect(event),
        },
    );
    *s = fold(s, &r.facts).unwrap();
    r
}
fn recover(s: &mut State) {
    let r = ThreadMachine::step(
        s,
        &InputEnvelope {
            at: at(),
            key: "recover".into(),
            input: Input::Recover {
                trigger: RecoveryTrigger::Startup,
                continue_after_restart: false,
            },
        },
    );
    *s = fold(s, &r.facts).unwrap();
}

// T3 fixtures/queued_turn/codex_output.ts: the accepted queued turn becomes run 2.
#[test]
fn queued_turn_starts_after_active_turn_and_only_then_enters_the_timeline() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    command(
        &mut s,
        "second",
        send_message("second", DispatchMode::QueueAfterActive),
    );
    assert_eq!(s.visible_items().len(), 1);
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    finish(&mut s, &a);
    assert_eq!(s.runs.iter().map(|r| r.ordinal).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(s.runs[1].status, RunStatus::Starting);
    assert_eq!(s.messages[1].intent, InputIntent::QueuedTurn);
    assert_eq!(s.visible_items().len(), 2);
}
// T3 fixtures/message_steering/claude_output.ts: steering attaches to one run and turn.
#[test]
fn steering_preserves_run_and_attempt_and_records_the_input_intent() {
    let mut s = state();
    let (run, a) = running(&mut s, "initial");
    let reply = command(
        &mut s,
        "steer",
        send_message("steer", DispatchMode::SteerActive { run: run.clone() }),
    );
    assert_eq!(reply.reply, Reply::Run(run));
    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.attempts.len(), 1);
    assert_eq!(s.runs[0].attempt.as_ref(), Some(&a));
    assert_eq!(s.messages[1].intent, InputIntent::Steer);
    assert!(matches!(
        reply.effects[0].body,
        EffectBody::Provider(ProviderCommand::Steer { .. })
    ));
}
// T3 SelectionRestart.integration.test.ts and RunExecutionService.test.ts: a superseded
// attempt gets no interrupt rows and no subagent cascade.
#[test]
fn restart_supersedes_attempt_and_leaves_native_children_to_their_provider() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    provider(
        &mut s,
        "child",
        &a,
        ProviderEvent::SubagentStarted {
            background: false,
            native_thread: None,
            key: "child".into(),
            parent: None,
            prompt: "child prompt".into(),
            model: None,
        },
    );
    let restart = command(
        &mut s,
        "restart",
        send_message("restart", DispatchMode::RestartActive { run }),
    );
    assert_eq!(
        s.attempts.iter().map(|a| a.status).collect::<Vec<_>>(),
        [AttemptStatus::Superseded, AttemptStatus::Pending]
    );
    assert_eq!(s.runs[0].status, RunStatus::Starting);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
    assert!(!restart.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::SendToThread { command, .. } if matches!(command.as_ref(), Command::Stop)
    )));
    assert!(!s.items.iter().any(|item| matches!(
        item.kind,
        ItemKind::RunInterruptRequest | ItemKind::RunInterruptResult { .. }
    )));
    let next = s.runs[0].attempt.clone().unwrap();
    let routed = provider(
        &mut s,
        "child-output",
        &next,
        ProviderEvent::Child {
            key: "child".into(),
            event: Box::new(ProviderEvent::TextDelta {
                key: "child-text".into(),
                kind: ProviderItem::Text,
                text: "still working".into(),
            }),
        },
    );
    assert!(routed.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::SendToThread { command, .. }
            if matches!(command.as_ref(), Command::NativeInput { attempt, .. } if attempt == &a)
    )));
    let before = s.clone();
    let late = provider(
        &mut s,
        "late",
        &a,
        ProviderEvent::TextDelta {
            key: "old".into(),
            kind: ProviderItem::Text,
            text: "stale".into(),
        },
    );
    assert_eq!(late.reply, Reply::Ignored);
    assert_eq!(s, before);
}
fn claude_selection() -> ModelSelection {
    ModelSelection {
        instance: "claude".into(),
        driver: Driver::Claude,
        model: "claude-sonnet-4-6".into(),
        options: BTreeMap::new(),
    }
}
// T3 CommandPolicy.test.ts: providers without interrupt-and-restart reject a required restart.
#[test]
fn restart_and_steering_respect_capabilities_and_maintenance_turns() {
    let mut s = state();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let (run, _) = running(&mut s, "first");
    let mut restart = send_message("restart", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut restart {
        message.intent = Some(DeliveryIntent::Restart);
    }
    assert_eq!(
        command(&mut s, "restart", restart).reply,
        Reply::Rejected {
            reason: "restart-unsupported".into()
        }
    );
    let mut compact = send_message(
        "compact-steer",
        DispatchMode::SteerActive { run: run.clone() },
    );
    if let Command::Send(message) = &mut compact {
        message.text = " /Compact ".into();
    }
    assert_eq!(
        command(&mut s, "compact-steer", compact).reply,
        Reply::Rejected {
            reason: "maintenance-must-run-separately".into()
        }
    );
    let mut s = state();
    let mut logout = send_message("logout", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut logout {
        message.text = "/logout".into();
    }
    command(&mut s, "logout", logout);
    let run = s.runs[0].id.clone();
    let a = s.runs[0].attempt.clone().unwrap();
    provider(
        &mut s,
        "started",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    assert_eq!(
        command(
            &mut s,
            "steer",
            send_message("steer", DispatchMode::SteerActive { run })
        )
        .reply,
        Reply::Rejected {
            reason: "maintenance-in-progress".into()
        }
    );
}
#[test]
fn stop_during_start_emits_interrupt_even_without_native_turn() {
    let mut s = state();
    let Reply::Run(run) = command(
        &mut s,
        "first",
        send_message("first", DispatchMode::StartImmediately),
    )
    .reply
    else {
        panic!()
    };
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    let a = s.runs[0].attempt.clone().unwrap();
    let stopped = command(
        &mut s,
        "stop",
        Command::Interrupt {
            run,
            hold_queue: true,
        },
    );
    assert_eq!(stopped.effects[0].attempt, Some(a.clone()));
    assert!(matches!(
        stopped.effects[0].body,
        EffectBody::Provider(ProviderCommand::Interrupt { .. })
    ));
    assert_eq!(s.runs[0].status, RunStatus::Interrupted);
    assert!(s.runs[1].queue_held);
    assert_eq!(
        provider(
            &mut s,
            "late-start",
            &a,
            ProviderEvent::TurnStarted {
                native_turn: Some("late".into())
            }
        )
        .reply,
        Reply::Ignored
    );
}
#[test]
fn completed_steer_becomes_one_followup_on_the_original_selection() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    command(
        &mut s,
        "steer",
        send_message("steer", DispatchMode::SteerActive { run }),
    );
    finish(&mut s, &a);
    let failure = EffectResult::ProviderFailed {
        attempt: a,
        operation: ProviderOperation::Steer,
        message: "completed".into(),
        message_id: Some(MessageId::new("steer").unwrap()),
        turn_completed: true,
        session_lost: false,
    };
    result(&mut s, "fallback", failure.clone());
    result(&mut s, "fallback-retry", failure);
    assert_eq!(s.runs.len(), 2);
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.runs[0].status, RunStatus::Completed);
    assert_eq!(s.runs[1].selection, selection());
}
#[test]
fn failed_control_operation_keeps_live_native_work_authoritative() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    result(
        &mut s,
        "failed-steer",
        EffectResult::ProviderFailed {
            attempt: a,
            operation: ProviderOperation::Steer,
            message: "transport error".into(),
            message_id: None,
            turn_completed: false,
            session_lost: false,
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Running);
}
// T3 ProviderRuntimeRecoveryService.test.ts: queued identities survive; live questions expire.
#[test]
fn recovery_holds_queued_work_finishes_streaming_and_preserves_async_questions() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    provider(
        &mut s,
        "text",
        &a,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "partial".into(),
        },
    );
    for (key, capability) in [
        ("live", ResponseCapability::Live),
        ("async", ResponseCapability::Message),
    ] {
        provider(
            &mut s,
            key,
            &a,
            ProviderEvent::RequestOpened {
                owner_path: vec![],
                key: key.into(),
                body: RequestBody::Questions { questions: vec![] },
                capability,
            },
        );
    }
    command(
        &mut s,
        "queue",
        send_message("queue", DispatchMode::QueueAfterActive),
    );
    let queued = s.runs[1].clone();
    recover(&mut s);
    assert_eq!(s.runs[0].status, RunStatus::Cancelled);
    assert_eq!(s.runs[1].id, queued.id);
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    assert!(s.runs[1].queue_held);
    assert_eq!(s.requests[0].status, RequestStatus::Expired);
    assert_eq!(s.requests[1].status, RequestStatus::Pending);
    assert!(!s.messages.iter().any(|m| m.streaming));
    assert!(
        s.items
            .iter()
            .filter(|i| !matches!(i.kind, ItemKind::UserInputRequest { .. }))
            .all(|i| i.status.terminal())
    );
}
#[test]
fn repeated_command_returns_receipt_without_repeating_effects() {
    let mut s = state();
    let c = send_message("first", DispatchMode::StartImmediately);
    let first = command(&mut s, "first", c.clone());
    let retry = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "retry".into(),
            input: Input::Command {
                id: CommandId::new("first").unwrap(),
                command: Box::new(c),
                receipt: first.receipt.clone(),
            },
        },
    );
    assert_eq!(retry.reply, first.reply);
    assert!(retry.facts.is_empty());
    assert!(retry.effects.is_empty());
    let conflict = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "retry".into(),
            input: Input::Command {
                id: CommandId::new("first").unwrap(),
                command: Box::new(Command::Rename {
                    title: "other".into(),
                }),
                receipt: first.receipt,
            },
        },
    );
    assert_eq!(
        conflict.reply,
        Reply::Rejected {
            reason: "command-id-conflict".into()
        }
    );
}
#[test]
fn prepared_failure_promotes_queue_and_retry_cannot_overlap_it() {
    let mut s = state();
    let Reply::Run(run) = command(
        &mut s,
        "prepare",
        send_message("prepare", DispatchMode::DeferStart),
    )
    .reply
    else {
        panic!()
    };
    command(
        &mut s,
        "queue",
        send_message("queue", DispatchMode::QueueAfterActive),
    );
    command(
        &mut s,
        "fail",
        Command::FailPrepared {
            run: run.clone(),
            message: "preparation failed".into(),
        },
    );
    assert_eq!(s.runs[1].status, RunStatus::Starting);
    assert!(matches!(
        command(&mut s, "retry", Command::RetryPrepared { run }).reply,
        Reply::Rejected { .. }
    ));
}
fn checkpoint(s: &mut State, run: &RunId, attempt: &RunAttemptId, key: &str) -> CheckpointId {
    let id = CheckpointId::new(key).unwrap();
    let scope = s
        .runs
        .iter()
        .find(|candidate| &candidate.id == run)
        .unwrap()
        .checkpoint_scope
        .clone()
        .unwrap_or(CheckpointScope {
            id: CheckpointScopeId::new("workspace").unwrap(),
            cwd: "/workspace".into(),
        });
    let step = ThreadMachine::step(
        s,
        &InputEnvelope {
            at: at(),
            key: format!("scope-{key}"),
            input: Input::CheckpointScope {
                run: Some(run.clone()),
                attempt: Some(attempt.clone()),
                scope: Some(scope.clone()),
            },
        },
    );
    *s = fold(s, &step.facts).unwrap();
    let ordinal = s
        .runs
        .iter()
        .find(|candidate| &candidate.id == run)
        .unwrap()
        .ordinal;
    let baselines = [0, ordinal - 1]
        .into_iter()
        .filter(|ordinal| {
            !s.checkpoints.iter().any(|checkpoint| {
                checkpoint.scope.as_ref() == Some(&scope) && checkpoint.run_ordinal == *ordinal
            })
        })
        .map(|ordinal| CapturedBaseline {
            status: CheckpointStatus::Ready,
            checkpoint: CheckpointId::new(format!("{key}-baseline-{ordinal}")).unwrap(),
            ordinal,
            file_ref: format!("baseline-{ordinal}"),
            native_heads: s
                .runs
                .iter()
                .find(|candidate| &candidate.id == run)
                .unwrap()
                .native_baseline_heads
                .clone(),
        })
        .collect();
    result(
        s,
        key,
        EffectResult::CheckpointCaptured {
            status: CheckpointStatus::Ready,
            baselines,
            run: run.clone(),
            attempt: Some(attempt.clone()),
            checkpoint: id.clone(),
            file_ref: key.into(),
        },
    );
    id
}
#[test]
fn rollback_is_absolute_blocks_run_operations_and_preserves_new_metadata() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: true,
        },
    );
    for (key, c) in [
        ("resume", Command::ResumeQueue),
        ("send", send_message("new", DispatchMode::StartImmediately)),
        (
            "rollback-again",
            Command::Rollback {
                checkpoint: CheckpointId::new("cp-first").unwrap(),
                restore_files: false,
            },
        ),
    ] {
        assert_eq!(
            command(&mut s, key, c).reply,
            Reply::Rejected {
                reason: "rollback-pending".into()
            }
        );
    }
    command(
        &mut s,
        "rename",
        Command::Rename {
            title: "Renamed while restoring".into(),
        },
    );
    result(
        &mut s,
        "done",
        EffectResult::RollbackFinished {
            bindings: vec![],
            command: CommandId::new("rollback").unwrap(),
        },
    );
    assert_eq!(s.thread.unwrap().title, "Renamed while restoring");
    assert_eq!(s.runs[1].status, RunStatus::RolledBack);
}
#[test]
fn waiting_capture_survives_recovery_without_releasing_the_queue() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    finish(&mut s, &a);
    checkpoint(&mut s, &run, &a, "baseline");
    let (run, b) = running(&mut s, "second");
    command(
        &mut s,
        "queue",
        send_message("queue", DispatchMode::QueueAfterActive),
    );
    finish(&mut s, &b);
    assert_eq!(s.runs[1].status, RunStatus::Waiting);
    recover(&mut s);
    assert_eq!(s.runs[1].status, RunStatus::Waiting);
    assert!(s.runs[2].queue_held);
    checkpoint(&mut s, &run, &b, "second-cp");
    assert_eq!(s.runs[1].status, RunStatus::Completed);
    assert_eq!(s.runs[2].status, RunStatus::Queued);
}
#[test]
fn recovery_keeps_lost_work_for_its_provider_until_a_completed_non_compact_turn() {
    let mut s = state();
    let original_selection = selection();
    let (_, a) = running(&mut s, "root");
    provider(
        &mut s,
        "child-start",
        &a,
        ProviderEvent::SubagentStarted {
            background: true,
            native_thread: None,
            key: "child".into(),
            parent: None,
            prompt: "Background subagent test".into(),
            model: None,
        },
    );
    finish(&mut s, &a);
    recover(&mut s);
    assert_eq!(s.runs[0].status, RunStatus::Completed);
    assert_eq!(s.tasks[0].status, ItemStatus::Cancelled);
    assert_eq!(
        s.runs[0].restart_cancelled_work[0].label,
        "Background subagent test"
    );
    let mut other = original_selection.clone();
    other.instance = "other".into();
    command(
        &mut s,
        "other-selection",
        Command::SwitchProvider { selection: other },
    );
    let (_, other) = running(&mut s, "other");
    assert!(pending_restart_work(&s.runs[1], &s.runs, &s.attempts, &s.messages).is_empty());
    finish(&mut s, &other);
    command(
        &mut s,
        "original-selection",
        Command::SwitchProvider {
            selection: original_selection,
        },
    );
    let compact = command(&mut s, "compact", Command::Compact);
    assert!(compact.effects.iter().any(|effect| matches!(
        effect.body,
        EffectBody::Provider(ProviderCommand::Compact { .. })
    )));
    let compact = s.active_run().unwrap().attempt.clone().unwrap();
    finish(&mut s, &compact);
    let start = command(
        &mut s,
        "after-compact",
        send_message("continue", DispatchMode::StartImmediately),
    );
    assert!(start.effects.iter().any(|effect| matches!(&effect.body, EffectBody::Provider(ProviderCommand::Start { text, note, .. }) if provider_prompt(text, note.as_deref(), None) == "Note: the T3 server restarted, and this background work was cancelled before it finished. It will not report back:\n- subagent: Background subagent test\n\nUser message:\ncontinue")));
    let delivered = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "delivered",
        &delivered,
        ProviderEvent::TurnStarted {
            native_turn: Some("delivery".into()),
        },
    );
    finish(&mut s, &delivered);
    let later = command(
        &mut s,
        "later",
        send_message("later", DispatchMode::StartImmediately),
    );
    assert!(later.effects.iter().any(|effect| matches!(&effect.body, EffectBody::Provider(ProviderCommand::Start { text, .. }) if text == "later")));
}
#[test]
fn restart_notes_use_completion_order_and_do_not_repeat_after_a_delivered_attempt() {
    let mut s = state();
    let (_, first) = running(&mut s, "resumed");
    finish(&mut s, &first);
    let (_, second) = running(&mut s, "ran-first");
    finish(&mut s, &second);
    running(&mut s, "next");
    let lost = vec![CancelledBackgroundWork {
        id: "item-1".into(),
        kind: "subagent".into(),
        label: "Background subagent test".into(),
    }];
    s.runs[0].restart_cancelled_work = lost.clone();
    s.runs[0].completed_at = Some(Timestamp::parse("2026-10-03T10:05:00.000Z").unwrap());
    s.runs[1].completed_at = Some(Timestamp::parse("2026-10-03T10:00:00.000Z").unwrap());
    assert_eq!(
        pending_restart_work(&s.runs[2], &s.runs, &s.attempts, &s.messages),
        lost
    );
    let replacement = RunAttemptId::new("replacement").unwrap();
    let mut steered = s.runs[2].clone();
    steered.attempt = Some(replacement);
    let mut attempts = s.attempts.clone();
    attempts.last_mut().unwrap().status = AttemptStatus::Completed;
    assert!(pending_restart_work(&steered, &s.runs, &attempts, &s.messages).is_empty());
    attempts.last_mut().unwrap().status = AttemptStatus::Cancelled;
    assert_eq!(
        pending_restart_work(&steered, &s.runs, &attempts, &s.messages),
        lost
    );
}
#[test]
fn recovery_preserves_distinct_work_ids_and_shutdown_reason() {
    let mut s = state();
    let (_, a) = running(&mut s, "root");
    for key in ["first", "second"] {
        provider(
            &mut s,
            key,
            &a,
            ProviderEvent::SubagentStarted {
                background: true,
                native_thread: None,
                key: key.into(),
                parent: None,
                prompt: "sleep 20".into(),
                model: None,
            },
        );
    }
    let recovered = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "shutdown".into(),
            input: Input::Recover {
                trigger: RecoveryTrigger::Shutdown,
                continue_after_restart: true,
            },
        },
    );
    s = fold(&s, &recovered.facts).unwrap();
    assert_eq!(s.runs[0].restart_cancelled_work.len(), 2);
    assert_ne!(
        s.runs[0].restart_cancelled_work[0].id,
        s.runs[0].restart_cancelled_work[1].id
    );
    assert!(s.tasks.iter().all(|task| task.result.as_deref()
        == Some("Cancelled because the server shut down before the provider work completed.")));
    assert!(!recovered.effects.iter().any(|effect| matches!(effect.body, EffectBody::SendToThread { command: ref next, .. } if matches!(**next, Command::ContinueRestart { .. }))));
    recover(&mut s);
    assert_eq!(s.runs[0].restart_cancelled_work.len(), 2);
}
#[test]
fn restart_continuation_precedes_held_queue_and_carries_notes_across_an_unaccepted_restart() {
    for lost_work in [false, true] {
        let mut s = state();
        let (source, attempt) = running(&mut s, "cut");
        command(
            &mut s,
            "queue",
            send_message("queued", DispatchMode::QueueAfterActive),
        );
        if lost_work {
            provider(
                &mut s,
                "lost",
                &attempt,
                ProviderEvent::BackgroundTask {
                    key: "work".into(),
                    tool: "bash".into(),
                    kind: BackgroundKind::Command,
                    description: "sleep 20".into(),
                    status: None,
                    summary: None,
                },
            );
        }
        let recovered = ThreadMachine::step(
            &s,
            &InputEnvelope {
                at: at(),
                key: "restart".into(),
                input: Input::Recover {
                    trigger: RecoveryTrigger::Startup,
                    continue_after_restart: true,
                },
            },
        );
        s = fold(&s, &recovered.facts).unwrap();
        assert_eq!(s.runs[0].status, RunStatus::Cancelled);
        assert!(s.runs[1].queue_held);
        let next = recovered
            .effects
            .iter()
            .find_map(|effect| match &effect.body {
                EffectBody::SendToThread { command, .. } => Some(*command.clone()),
                _ => None,
            })
            .unwrap();
        let started = command(&mut s, "continue", next.clone());
        assert_eq!(s.runs[2].restart_of.as_ref(), Some(&source));
        assert_eq!(s.runs[1].status, RunStatus::Queued);
        assert_eq!(s.runs[2].status, RunStatus::Starting);
        assert!(started.effects.iter().any(|effect| matches!(&effect.body, EffectBody::Provider(ProviderCommand::Start { resume_interrupted_turn, text, .. }) if *resume_interrupted_turn != lost_work && if lost_work { text.ends_with("Continue where you left off.") && text.contains("sleep 20 (id work)") } else { text == "Continue where you left off." })));
        assert_eq!(command(&mut s, "duplicate", next).reply, Reply::Ignored);
        if lost_work {
            let recovered = ThreadMachine::step(
                &s,
                &InputEnvelope {
                    at: at(),
                    key: "restart-again".into(),
                    input: Input::Recover {
                        trigger: RecoveryTrigger::Startup,
                        continue_after_restart: true,
                    },
                },
            );
            s = fold(&s, &recovered.facts).unwrap();
            let next = recovered
                .effects
                .iter()
                .find_map(|effect| match &effect.body {
                    EffectBody::SendToThread { command, .. } => Some(*command.clone()),
                    _ => None,
                })
                .unwrap();
            let started = command(&mut s, "chained", next);
            assert!(started.effects.iter().any(|effect| matches!(&effect.body, EffectBody::Provider(ProviderCommand::Start { resume_interrupted_turn: false, text, .. }) if text.contains("sleep 20 (id work)"))));
        }
    }
}
#[test]
fn stopped_maintenance_and_settled_runs_do_not_receive_automatic_restart_prompts() {
    for kind in ["stop", "/compact", "/logout", "completed"] {
        let mut s = state();
        let (_, a) = running(&mut s, kind);
        if kind == "stop" {
            command(&mut s, "stop", Command::Stop);
        }
        if kind == "completed" {
            finish(&mut s, &a);
        }
        let recovered = ThreadMachine::step(
            &s,
            &InputEnvelope {
                at: at(),
                key: "restart".into(),
                input: Input::Recover {
                    trigger: RecoveryTrigger::Startup,
                    continue_after_restart: true,
                },
            },
        );
        s = fold(&s, &recovered.facts).unwrap();
        for effect in recovered.effects {
            if let EffectBody::SendToThread { command: next, .. } = effect.body {
                command(&mut s, "continue", *next);
            }
        }
        assert_eq!(s.runs.len(), 1);
    }
}
#[test]
fn a_new_user_run_takes_precedence_over_a_delayed_restart_continuation() {
    let mut s = state();
    let (source, _) = running(&mut s, "cut");
    command(
        &mut s,
        "queue",
        send_message("held", DispatchMode::QueueAfterActive),
    );
    recover(&mut s);
    let step = command(
        &mut s,
        "new-user",
        send_message("new-user", DispatchMode::StartImmediately),
    );
    assert!(step.effects.iter().any(|effect| matches!(
        effect.body,
        EffectBody::Provider(ProviderCommand::Start { .. })
    )));
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    assert!(s.runs[1].queue_held);
    let before = s.runs.len();
    assert_eq!(
        command(
            &mut s,
            "continue",
            Command::ContinueRestart {
                source,
                enabled: true
            }
        )
        .reply,
        Reply::Ignored
    );
    assert_eq!(s.runs.len(), before);
}
#[test]
fn a_delegated_child_reports_recovery_cancellation_or_its_continuation_result() {
    for (continue_after_restart, enabled) in [(false, false), (true, false), (true, true)] {
        let mut parent = state();
        let (_, _) = running(&mut parent, "parent");
        let task = NodeId::new("delegated").unwrap();
        let delegated = command(
            &mut parent,
            "delegate",
            Command::Delegate {
                task: task.clone(),
                child: ThreadId::new("child").unwrap(),
                prompt: "Inspect boundary".into(),
                selection: selection(),
                wake: CompletionWake::SettledOnly,
            },
        );
        let accept = delegated
            .effects
            .into_iter()
            .find_map(|effect| match effect.body {
                EffectBody::SendToThread { command, .. } => Some(*command),
                _ => None,
            })
            .unwrap();
        let mut child = State::default();
        command(&mut child, "accept", accept);
        let attempt = child.active_run().unwrap().attempt.clone().unwrap();
        provider(
            &mut child,
            "ready",
            &attempt,
            ProviderEvent::SessionReady {
                native_thread: "child-native".into(),
            },
        );
        provider(
            &mut child,
            "started",
            &attempt,
            ProviderEvent::TurnStarted {
                native_turn: Some("turn".into()),
            },
        );
        let recovered = ThreadMachine::step(
            &child,
            &InputEnvelope {
                at: at(),
                key: "restart-child".into(),
                input: Input::Recover {
                    trigger: RecoveryTrigger::Startup,
                    continue_after_restart,
                },
            },
        );
        child = fold(&child, &recovered.facts).unwrap();
        for effect in recovered.effects {
            if let EffectBody::SendToThread {
                thread,
                command: next,
            } = effect.body
            {
                if thread == ThreadId::new("child").unwrap() {
                    let mut next = *next;
                    if let Command::ContinueRestart {
                        enabled: current, ..
                    } = &mut next
                    {
                        *current = enabled;
                    }
                    let continued = command(&mut child, "continue", next);
                    for effect in continued.effects {
                        if let EffectBody::SendToThread { command: next, .. } = effect.body {
                            command(&mut parent, "result", *next);
                        }
                    }
                } else {
                    command(&mut parent, "result", *next);
                }
            }
        }
        if continue_after_restart && enabled {
            assert_eq!(parent.tasks[0].status, ItemStatus::Running);
            let attempt = child.active_run().unwrap().attempt.clone().unwrap();
            provider(
                &mut child,
                "accepted",
                &attempt,
                ProviderEvent::TurnStarted {
                    native_turn: Some("continued".into()),
                },
            );
            provider(
                &mut child,
                "reply",
                &attempt,
                ProviderEvent::TextDelta {
                    key: "reply".into(),
                    kind: ProviderItem::Text,
                    text: "Recovered result".into(),
                },
            );
            let finished = provider(
                &mut child,
                "finished",
                &attempt,
                ProviderEvent::TurnFinished {
                    status: RunStatus::Completed,
                    native_head: Some("continued".into()),
                },
            );
            for effect in finished.effects {
                if let EffectBody::SendToThread { command: next, .. } = effect.body {
                    command(&mut parent, "result", *next);
                }
            }
            assert_eq!(parent.tasks[0].status, ItemStatus::Completed);
            assert_eq!(parent.tasks[0].result.as_deref(), Some("Recovered result"));
            let status = delegated_task_status(
                &parent.tasks[0],
                &child.runs,
                &child.items,
                &parent.transfers,
                &child.messages,
            );
            assert_eq!(status.child_run_id.as_ref(), Some(&child.runs[0].id));
            assert_eq!(
                status.latest_terminal_run_id.as_ref(),
                Some(&child.runs[1].id)
            );
            assert!(status.result_context_transfer_id.is_some());
        } else {
            assert_eq!(parent.tasks[0].status, ItemStatus::Cancelled);
            assert_eq!(child.runs.len(), 1);
        }
    }
}
#[test]
fn recovered_native_children_reject_old_output_and_remain_provider_owned() {
    let mut child = state();
    let owner = RunAttemptId::new("native-owner").unwrap();
    command(
        &mut child,
        "bind",
        Command::BindNativeChild {
            native_thread: Some("native-child".into()),
            owner: owner.clone(),
            parent: ThreadId::new("parent").unwrap(),
            task: NodeId::new("task").unwrap(),
            generation: 0,
        },
    );
    provider(
        &mut child,
        "output",
        &owner,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "partial".into(),
        },
    );
    recover(&mut child);
    let before = child.clone();
    assert_eq!(
        provider(
            &mut child,
            "late",
            &owner,
            ProviderEvent::TextDelta {
                key: "text".into(),
                kind: ProviderItem::Text,
                text: "late".into()
            }
        )
        .reply,
        Reply::Ignored
    );
    assert_eq!(child, before);
    assert!(matches!(
        command(
            &mut child,
            "send",
            send_message("send", DispatchMode::StartImmediately)
        )
        .reply,
        Reply::Rejected { .. }
    ));
    assert!(child.runs.is_empty());
}
#[test]
fn fork_history_and_provider_context_are_fixed_at_creation() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    provider(
        &mut s,
        "text",
        &a,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "fork marker".into(),
        },
    );
    finish(&mut s, &a);
    let fork = command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: run,
            title: None,
        },
    );
    assert!(matches!(
        fork.effects[0].body,
        EffectBody::ForkNative { .. }
    ));
    let fork = result(
        &mut s,
        "fork-result",
        EffectResult::NativeForked {
            command: CommandId::new("fork").unwrap(),
            native_thread: "fork-native".into(),
        },
    );
    let EffectBody::SendToThread {
        command: accept, ..
    } = &fork.effects[0].body
    else {
        panic!()
    };
    let mut child = State::default();
    command(&mut child, "accept", *accept.clone());
    let history = child.inherited_items.clone();
    command(
        &mut s,
        "rename",
        Command::Rename {
            title: "Changed parent".into(),
        },
    );
    let start = command(
        &mut child,
        "child-send",
        send_message("child-send", DispatchMode::StartImmediately),
    );
    assert_eq!(child.inherited_items, history);
    let EffectBody::Provider(ProviderCommand::Start {
        context,
        native_thread,
        ..
    }) = &start.effects[0].body
    else {
        panic!()
    };
    assert!(context.is_none());
    assert_eq!(native_thread.as_deref(), Some("fork-native"));
    assert!(
        child
            .inherited_items
            .iter()
            .any(|i| i.text == "fork marker")
    );
    assert_eq!(child.thread.unwrap().title, "Thread fork");
}
#[test]
fn merge_back_supersedes_pending_delta_and_excludes_inherited_history() {
    let mut parent = state();
    let (run, a) = running(&mut parent, "parent-marker");
    finish(&mut parent, &a);
    let fork = command(
        &mut parent,
        "fork",
        Command::Fork {
            target: ThreadId::new("child").unwrap(),
            through_run: run,
            title: None,
        },
    );
    assert!(matches!(
        fork.effects[0].body,
        EffectBody::ForkNative { .. }
    ));
    let fork = result(
        &mut parent,
        "fork-result",
        EffectResult::NativeForked {
            command: CommandId::new("fork").unwrap(),
            native_thread: "fork-native".into(),
        },
    );
    let EffectBody::SendToThread {
        command: accept, ..
    } = &fork.effects[0].body
    else {
        panic!()
    };
    let mut child = State::default();
    command(&mut child, "accept", *accept.clone());
    let (_, b) = running(&mut child, "child-marker");
    finish(&mut child, &b);
    let merge = command(
        &mut child,
        "merge",
        Command::MergeBack {
            target: ThreadId::new("thread").unwrap(),
            through_run: None,
        },
    );
    let EffectBody::SendToThread {
        command: accept, ..
    } = &merge.effects[0].body
    else {
        panic!()
    };
    command(&mut parent, "accept-merge", *accept.clone());
    command(&mut parent, "duplicate", *accept.clone());
    assert_eq!(parent.transfers.len(), 1);
    assert!(render_history(&parent.transfers[0].history).contains("child-marker"));
    assert!(!render_history(&parent.transfers[0].history).contains("parent-marker"));
}
#[test]
fn compact_keeps_pending_handoff_for_the_next_real_prompt() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let mut claude = selection();
    claude.instance = "claude".into();
    claude.driver = Driver::Claude;
    command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: claude },
    );
    let compact = command(&mut s, "compact", Command::Compact);
    assert!(matches!(
        compact.effects[0].body,
        EffectBody::Provider(ProviderCommand::Compact { .. })
    ));
    assert!(s.transfers[0].delivery.is_none());
}
#[test]
fn provider_handoff_returns_only_runs_since_the_target_last_received_a_turn() {
    let mut s = state();
    let original_selection = selection();
    let (_, a) = running(&mut s, "original-native-history");
    finish(&mut s, &a);
    let mut target = original_selection.clone();
    target.instance = "other".into();
    command(
        &mut s,
        "switch-other",
        Command::SwitchProvider {
            selection: target.clone(),
        },
    );
    assert!(s.transfers.is_empty());
    let (_, b) = running(&mut s, "other-history");
    finish(&mut s, &b);
    command(
        &mut s,
        "switch-back",
        Command::SwitchProvider {
            selection: original_selection.clone(),
        },
    );
    let step = command(
        &mut s,
        "return",
        send_message("return", DispatchMode::StartImmediately),
    );
    let history = step
        .effects
        .iter()
        .find_map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Start {
                context: Some(history),
                ..
            }) => Some(history),
            _ => None,
        })
        .unwrap();
    assert!(render_history(history).contains("other-history"));
    assert!(!render_history(history).contains("original-native-history"));
    assert!(render_history(history).contains("delta_since_target_last_seen"));
    let returned = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "return-bound",
        &returned,
        ProviderEvent::SessionReady {
            native_thread: "native-thread".into(),
        },
    );
    provider(
        &mut s,
        "return-injected",
        &returned,
        ProviderEvent::ContextInjected,
    );
    provider(
        &mut s,
        "return-started",
        &returned,
        ProviderEvent::TurnStarted {
            native_turn: Some("return-turn".into()),
        },
    );
    finish(&mut s, &returned);

    // The queued run's selection is authoritative when it is promoted; the
    // thread may have changed selection again while it was waiting.
    let (_, active) = running(&mut s, "active-again");
    let mut queued = send_message("queued-other", DispatchMode::QueueAfterActive);
    if let Command::Send(message) = &mut queued {
        message.selection = Some(target);
    }
    command(&mut s, "queue-other", queued);
    let promoted = provider(
        &mut s,
        "active-finish",
        &active,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: None,
        },
    );
    let history = promoted
        .effects
        .iter()
        .find_map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Start {
                context: Some(history),
                ..
            }) => Some(history),
            _ => None,
        })
        .unwrap();
    assert!(render_history(history).contains("active-again"));
    assert!(!render_history(history).contains("other-history"));
    assert!(!render_history(history).contains("original-native-history"));
}
#[test]
fn plan_followup_preserves_attachments_and_consumes_the_proposal() {
    let mut s = state();
    let (_, a) = running(&mut s, "plan");
    provider(
        &mut s,
        "proposal",
        &a,
        ProviderEvent::Plan {
            kind: PlanKind::Proposed,
            key: "proposal".into(),
            markdown: "replay fixture plan".into(),
            steps: vec![],
        },
    );
    finish(&mut s, &a);
    let plan = s.plans[0].id.clone();
    let file = Attachment {
        kind: AttachmentKind::File,
        source: None,
        id: "image".into(),
        name: "image.png".into(),
        mime_type: "image/png".into(),
        path: "/attachments/image.png".into(),
        size: 5,
    };
    command(
        &mut s,
        "implement",
        Command::Send(SendMessage {
            created_by: MessageAuthor::User,
            creation_source: "client".into(),
            id: MessageId::new("implement").unwrap(),
            text: "Implement the plan".into(),
            attachments: vec![file.clone()],
            selection: None,
            mode: DispatchMode::StartImmediately,
            intent: None,
            source_plan: Some(plan),
            title_seed: None,
        }),
    );
    assert_eq!(s.plans[0].implemented_by, Some(s.runs[1].id.clone()));
    assert_eq!(s.messages.last().unwrap().attachments, [file]);
}
#[test]
fn approvals_resolve_once_and_questions_keep_attachment_answers() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    provider(
        &mut s,
        "approval",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "approval".into(),
            body: RequestBody::Approval {
                kind: "command".into(),
                title: "Bash".into(),
                detail: None,
                options: vec![
                    ApprovalOption {
                        label: "Allow once".into(),
                        decision: ApprovalDecision::Accept,
                    },
                    ApprovalOption {
                        label: "Decline".into(),
                        decision: ApprovalDecision::Decline,
                    },
                ],
                input: Json(serde_json::json!({})),
            },
            capability: ResponseCapability::Live,
        },
    );
    let id = s.requests[0].id.clone();
    let c = Command::Respond {
        request: id,
        decision: Some(ApprovalDecision::Accept),
        answers: None,
        attachments: BTreeMap::new(),
    };
    assert!(matches!(
        command(&mut s, "respond", c.clone()).reply,
        Reply::Request(_)
    ));
    assert!(matches!(
        command(&mut s, "respond-again", c).reply,
        Reply::Rejected { .. }
    ));
    provider(
        &mut s,
        "question",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "question".into(),
            body: RequestBody::Questions {
                questions: vec![Question {
                    required: true,
                    id: "choice".into(),
                    header: "Choose".into(),
                    question: "Which?".into(),
                    multiple: true,
                    options: vec![],
                }],
            },
            capability: ResponseCapability::Live,
        },
    );
    let id = s.requests[1].id.clone();
    let reply = command(
        &mut s,
        "answer",
        Command::Respond {
            request: id,
            decision: None,
            answers: Some(BTreeMap::from([(
                "choice".into(),
                Answer::Choices(vec!["One".into(), "Two".into()]),
            )])),
            attachments: BTreeMap::new(),
        },
    );
    assert!(matches!(
        reply.effects[0].body,
        EffectBody::Provider(ProviderCommand::Respond { .. })
    ));
}
#[test]
fn cancelled_delegated_wake_stays_disposed_after_reconciliation() {
    let mut s = state();
    let (_, a) = starting(&mut s, "first");
    let task = NodeId::new("task").unwrap();
    command(
        &mut s,
        "delegate",
        Command::Delegate {
            task: task.clone(),
            child: ThreadId::new("child").unwrap(),
            prompt: "task prompt".into(),
            selection: selection(),
            wake: CompletionWake::Always,
        },
    );
    let source_message = s
        .tasks
        .iter()
        .find(|candidate| candidate.id == task)
        .and_then(|task| task.original_message.clone());
    command(
        &mut s,
        "task-result",
        Command::TaskResult {
            generation: None,
            source_message,
            context: None,
            task: task.clone(),
            status: ItemStatus::Completed,
            result: "done".into(),
        },
    );
    command(
        &mut s,
        "wake",
        Command::AcceptTaskWake {
            task_ids: vec![task],
        },
    );
    let wake = s.runs[1].id.clone();
    command(&mut s, "cancel-wake", Command::CancelQueued { run: wake });
    finish(&mut s, &a);
    assert_eq!(s.tasks[0].delivery, DeliveryState::Disposed);
    assert_eq!(s.runs.len(), 2);
}
#[test]
fn native_subagent_followup_reopens_completed_task() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let start = ProviderEvent::SubagentStarted {
        background: false,
        native_thread: None,
        key: "child".into(),
        parent: None,
        prompt: "Hello".into(),
        model: None,
    };
    provider(&mut s, "spawn", &a, start.clone());
    provider(
        &mut s,
        "done",
        &a,
        ProviderEvent::SubagentFinished {
            key: "child".into(),
            status: ItemStatus::Completed,
            result: "Hello".into(),
        },
    );
    provider(&mut s, "reopen", &a, start);
    assert_eq!(s.tasks.len(), 1);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
}
// T3 client-runtime orchestrationV2Projection.test.ts: terminal updates retain usage.
#[test]
fn terminal_updates_keep_usage_and_streaming_appends_have_no_full_entity() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let usage = TokenUsage {
        input: 50_000,
        cached_input: 0,
        output: 0,
        reasoning_output: 0,
        total: 50_000,
        max: Some(200_000),
    };
    provider(&mut s, "usage", &a, ProviderEvent::Usage(usage.clone()));
    provider(
        &mut s,
        "delta-1",
        &a,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "Hello".into(),
        },
    );
    let delta = provider(
        &mut s,
        "delta-2",
        &a,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: " world".into(),
        },
    );
    assert_eq!(delta.facts.len(), 1);
    assert!(
        matches!(&delta.facts[0].body,FactBody::ItemTextAppended { offset:5,text,.. } if text==" world")
    );
    finish(&mut s, &a);
    assert_eq!(s.attempts[0].usage.as_ref(), Some(&usage));
    assert_eq!(s.messages[1].text, "Hello world");
}
#[test]
fn unread_uses_latest_completion_even_with_newer_queued_run() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let (run, _) = running(&mut s, "second");
    command(
        &mut s,
        "queue",
        send_message("queue", DispatchMode::QueueAfterActive),
    );
    command(
        &mut s,
        "stop",
        Command::Interrupt {
            run,
            hold_queue: true,
        },
    );
    assert_eq!(
        command(&mut s, "unread", Command::MarkUnread).reply,
        Reply::Accepted
    );
    assert_eq!(
        s.thread.unwrap().last_visited_at.unwrap().millis(),
        at().millis() - 1
    );
}
use proptest::prelude::*;
proptest! {
    #[test]
    fn repeated_restart_continuations_never_overlap_or_release_the_held_queue(queued in 0usize..6, restarts in 1usize..6) {
        let mut s = state();
        running(&mut s, "cut");
        for index in 0..queued { command(&mut s, &format!("queue-{index}"), send_message(&format!("queued-{index}"), DispatchMode::QueueAfterActive)); }
        for index in 0..restarts {
            let recovered = ThreadMachine::step(&s, &InputEnvelope { at: at(), key: format!("restart-{index}"), input: Input::Recover { trigger: RecoveryTrigger::Startup, continue_after_restart: true } });
            s = fold(&s, &recovered.facts).unwrap();
            for effect in recovered.effects { if let EffectBody::SendToThread { command: next, .. } = effect.body { command(&mut s, &format!("continue-{index}"), *next); } }
            prop_assert_eq!(s.runs.iter().filter(|run| run.status.blocking()).count(), 1);
            prop_assert_eq!(s.queued_runs().len(), queued);
            prop_assert!(s.queued_runs().iter().all(|run| run.queue_held));
            prop_assert!(s.runs.iter().filter(|run| run.status.terminal()).all(|run| run.completed_at.is_some()));
            prop_assert!(s.attempts.iter().all(|attempt| s.runs.iter().any(|run| run.id == attempt.run)));
            prop_assert!(s.items.windows(2).all(|pair| pair[0].ordinal < pair[1].ordinal));
        }
    }
    #[test]
    fn mixed_provider_commands_requests_and_restarts_preserve_attempt_ownership(ops in prop::collection::vec(0u8..12,0..100)) {
        let mut s = state(); let mut all_attempts = vec![];
        for (index,op) in ops.into_iter().enumerate() {
            let key = format!("mixed-{index}");
            let attempt = s.active_run().and_then(|r|r.attempt.clone());
            match (op,attempt) {
                (0|1,_) => { command(&mut s,&key,send_message(&key,DispatchMode::QueueAfterActive)); }
                (2,Some(a)) => { provider(&mut s,&key,&a,ProviderEvent::TurnStarted { native_turn:Some(key.clone()) }); }
                (3,Some(a)) => { provider(&mut s,&key,&a,ProviderEvent::TextDelta { key:"text".into(),kind:ProviderItem::Text,text:"日本語🙂".into() }); }
                (4,Some(a)) => { provider(&mut s,&key,&a,ProviderEvent::RequestOpened { owner_path:vec![], key:key.clone(),body:RequestBody::Questions { questions:vec![] },capability:ResponseCapability::Live }); }
                (5,_) => { if let Some(request) = s.requests.iter().find(|r|r.status == RequestStatus::Pending).cloned() { command(&mut s,&key,Command::Respond { request:request.id,answers:Some(BTreeMap::new()),decision:None,attachments:BTreeMap::new() }); } }
                (6,Some(a)) => { provider(&mut s,&key,&a,ProviderEvent::TurnFinished { status:RunStatus::Completed,native_head:Some(key.clone()) }); }
                (7,_) => { command(&mut s,&key,Command::Stop); }
                (8,_) => recover(&mut s),
                (9,_) => { command(&mut s,&key,Command::ResumeQueue); }
                (10,_) => { if let Some(run) = s.active_run().filter(|r|r.status == RunStatus::Running).map(|r|r.id.clone()) { command(&mut s,&key,send_message(&key,DispatchMode::RestartActive { run })); } }
                _ => { if let Some(a) = all_attempts.first().cloned() { let before=s.clone(); let stale=provider(&mut s,&key,&a,ProviderEvent::TextDelta { key:"late".into(),kind:ProviderItem::Text,text:"stale".into() }); if !before.runs.iter().any(|r|r.attempt.as_ref()==Some(&a) && matches!(r.status,RunStatus::Starting|RunStatus::Running)) { prop_assert!(stale.facts.is_empty()); prop_assert_eq!(&before,&s); } } }
            }
            all_attempts = s.attempts.iter().map(|a|a.id.clone()).collect();
            prop_assert!(s.runs.iter().filter(|r|r.status.blocking()).count() <= 1);
            prop_assert!(s.items.windows(2).all(|pair|pair[0].ordinal < pair[1].ordinal));
            prop_assert!(s.requests.iter().filter(|r|r.status != RequestStatus::Pending).all(|r|r.resolved_at.is_some()));
            prop_assert!(s.items.iter().filter(|i|i.status.terminal()).all(|i|i.completed_at.is_some()));
        }
    }
    #[test]
    fn arbitrary_command_sequences_preserve_single_writer_invariants(ops in prop::collection::vec(0u8..8,0..120)) {
        let mut s=state();
        for (index,op) in ops.into_iter().enumerate() {
            let key=format!("command-{index}");
            match op {
                0|1=>{ command(&mut s,&key,send_message(&key,DispatchMode::QueueAfterActive)); }
                2=>{ if let Some(r)=s.active_run().cloned() { command(&mut s,&key,Command::Interrupt { run:r.id,hold_queue:index%2==0 }); } }
                3=>{ command(&mut s,&key,Command::ResumeQueue); }
                4=>{ if let Some(run)=s.queued_runs().first().map(|r| r.id.clone()) { command(&mut s,&key,Command::CancelQueued { run }); } }
                5=>recover(&mut s),
                6=>{ if let Some(a)=s.active_run().and_then(|r| r.attempt.clone()) { provider(&mut s,&key,&a,ProviderEvent::TurnFinished { status:RunStatus::Completed,native_head:None }); } }
                _=>{ if let Some(run)=s.queued_runs().last().map(|r| r.id.clone()) { let before=s.queued_runs().first().map(|r| r.id.clone()); command(&mut s,&key,Command::ReorderQueued { run,before }); } }
            }
            prop_assert!(s.runs.iter().filter(|r| r.status.blocking()).count()<=1);
            prop_assert!(s.runs.iter().filter(|r| r.status.terminal()).all(|r| r.completed_at.is_some()));
            prop_assert!(s.requests.iter().filter(|r| r.status!=RequestStatus::Pending).all(|r| r.resolved_at.is_some()));
            let ordinals=s.items.iter().map(|i| i.ordinal).collect::<Vec<_>>();
            prop_assert!(ordinals.windows(2).all(|p| p[0]<p[1]));
        }
    }
    #[test]
    fn fold_matches_incremental_application_and_step_never_changes_its_input(parts in prop::collection::vec(".{0,30}",0..40)) {
        let mut s=state(); let (_,a)=running(&mut s,"first"); let initial=s.clone(); let mut facts=vec![];
        for (index,text) in parts.into_iter().enumerate() {
            let envelope=InputEnvelope { at:at(),key:index.to_string(),input:Input::Provider { attempt:a.clone(),event: Box::new(ProviderEvent::TextDelta { key:"text".into(),kind:ProviderItem::Text,text }),
} };
            let before=s.clone(); let step=ThreadMachine::step(&s,&envelope);
            prop_assert_eq!(&s,&before); prop_assert_eq!(&step,&ThreadMachine::step(&s,&envelope));
            s=fold(&s,&step.facts).unwrap(); facts.extend(step.facts);
        }
        prop_assert_eq!(s,fold(&initial,&facts).unwrap());
    }
    #[test]
    fn queue_is_fifo_unless_explicitly_reordered(count in 1usize..40) {
        let mut s=state(); let (_,a)=running(&mut s,"first"); let mut expected=vec![];
        for i in 0..count { let key=format!("queued-{i}"); let Reply::Run(id)=command(&mut s,&key,send_message(&key,DispatchMode::QueueAfterActive)).reply else { panic!() }; expected.push(id); }
        finish(&mut s,&a);
        for id in expected { prop_assert_eq!(&s.active_run().unwrap().id,&id); let attempt=s.active_run().unwrap().attempt.clone().unwrap(); finish(&mut s,&attempt); }
        prop_assert!(s.active_run().is_none());
    }
}
// T3 CommandPolicy.test.ts: unchanged capability and delivery precedence matrix.
#[test]
fn automatic_delivery_obeys_negotiated_turn_capabilities() {
    let run = RunId::new("command-policy-active-run").unwrap();
    let request = DispatchMode::StartImmediately;
    for (support, expected) in [
        (
            TurnSupport {
                steer: true,
                interrupt: true,
                restart: true,
                queue: true,
            },
            DispatchMode::SteerActive { run: run.clone() },
        ),
        (
            TurnSupport {
                steer: false,
                interrupt: true,
                restart: true,
                queue: true,
            },
            DispatchMode::QueueAfterActive,
        ),
        (
            TurnSupport {
                steer: false,
                interrupt: true,
                restart: true,
                queue: false,
            },
            DispatchMode::RestartActive { run: run.clone() },
        ),
    ] {
        assert_eq!(
            resolve_dispatch(
                Some((&run, RunStatus::Running)),
                &request,
                Some(DeliveryIntent::Auto),
                support
            ),
            expected
        );
        for status in [RunStatus::Preparing, RunStatus::Starting] {
            assert_eq!(
                resolve_dispatch(
                    Some((&run, status)),
                    &request,
                    Some(DeliveryIntent::Auto),
                    support
                ),
                DispatchMode::QueueAfterActive
            );
        }
        assert_eq!(
            resolve_dispatch(None, &request, Some(DeliveryIntent::Auto), support),
            request
        );
        assert_eq!(
            resolve_dispatch(
                Some((&run, RunStatus::Running)),
                &request,
                Some(DeliveryIntent::Steer),
                support
            ),
            DispatchMode::SteerActive { run: run.clone() }
        );
    }
}
#[test]
fn automatic_completion_delivery_precedes_visible_queued_messages() {
    let mut s = state();
    starting(&mut s, "parent");
    command(
        &mut s,
        "visible-first",
        send_message("visible-first", DispatchMode::QueueAfterActive),
    );
    command(
        &mut s,
        "visible-second",
        send_message("visible-second", DispatchMode::QueueAfterActive),
    );
    let task = NodeId::new("task:child").unwrap();
    command(
        &mut s,
        "delegate",
        Command::Delegate {
            task: task.clone(),
            child: ThreadId::new("child").unwrap(),
            prompt: "task".into(),
            selection: selection(),
            wake: CompletionWake::Always,
        },
    );
    let source_message = s
        .tasks
        .iter()
        .find(|candidate| candidate.id == task)
        .and_then(|task| task.original_message.clone());
    command(
        &mut s,
        "result",
        Command::TaskResult {
            generation: None,
            source_message,
            context: None,
            task: task.clone(),
            status: ItemStatus::Completed,
            result: "done".into(),
        },
    );
    command(
        &mut s,
        "automatic",
        Command::AcceptTaskWake {
            task_ids: vec![task],
        },
    );
    assert_eq!(
        s.queued_runs()
            .iter()
            .map(|r| r.ordinal)
            .collect::<Vec<_>>(),
        [4, 2, 3]
    );
}
// Client reducer's authoritative-order test, expressed through facts.
#[test]
fn visible_items_sort_by_authoritative_ordinal_and_keep_inherited_rows() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    let mut inherited = s.items[0].clone();
    inherited.id = TurnItemId::new("inherited").unwrap();
    inherited.ordinal = 0;
    s.inherited_items.push(inherited);
    for (id, ordinal) in [("queued-future", 300), ("active-assistant", 201)] {
        apply(
            &mut s,
            &Fact {
                at: at(),
                body: FactBody::ItemStarted {
                    id: TurnItemId::new(id).unwrap(),
                    run: Some(run.clone()),
                    attempt: Some(a.clone()),
                    native_key: id.into(),
                    ordinal,
                    kind: ItemKind::Reasoning,
                },
            },
        )
        .unwrap();
    }
    assert_eq!(
        s.visible_items()
            .iter()
            .map(|i| i.ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 201, 300]
    );
    apply(
        &mut s,
        &Fact {
            at: at(),
            body: FactBody::RunFinished {
                id: run,
                status: RunStatus::RolledBack,
            },
        },
    )
    .unwrap();
    assert_eq!(
        s.visible_items()
            .iter()
            .map(|i| i.id.as_str())
            .collect::<Vec<_>>(),
        ["inherited"]
    );
}

// T3 CheckpointCaptureService.test.ts: a capture without a readable workspace still settles the run.
#[test]
fn failed_capture_settles_the_run_and_stopped_capture_keeps_its_terminal_status() {
    let mut s = state();
    let (first, a) = running(&mut s, "baseline");
    finish(&mut s, &a);
    checkpoint(&mut s, &first, &a, "baseline-cp");
    let (second, b) = running(&mut s, "second");
    command(&mut s, "stop-second", Command::Stop);
    provider(
        &mut s,
        "stopped",
        &b,
        ProviderEvent::TurnFinished {
            status: RunStatus::Interrupted,
            native_head: Some("stopped-head".into()),
        },
    );
    assert_eq!(s.runs[1].status, RunStatus::Interrupted);
    assert_eq!(s.captures.get(&second), Some(&RunStatus::Interrupted));
    command(
        &mut s,
        "next",
        send_message("next", DispatchMode::StartImmediately),
    );
    assert_eq!(s.runs[2].status, RunStatus::Queued);
    result(
        &mut s,
        "capture-error",
        EffectResult::CheckpointCaptured {
            status: CheckpointStatus::Error,
            baselines: vec![],
            run: second.clone(),
            attempt: Some(b.clone()),
            checkpoint: CheckpointId::new("stopped-cp").unwrap(),
            file_ref: "stopped-cp".into(),
        },
    );
    assert!(s.captures.is_empty());
    assert_eq!(s.runs[1].status, RunStatus::Interrupted);
    assert_eq!(
        s.runs[1].checkpoint.as_ref().unwrap().as_str(),
        "stopped-cp"
    );
    assert_eq!(
        s.checkpoints.last().unwrap().status,
        CheckpointStatus::Error
    );
    assert_eq!(s.runs[2].status, RunStatus::Starting);
    let (third, c) = (s.runs[2].id.clone(), s.runs[2].attempt.clone().unwrap());
    provider(
        &mut s,
        "third-started",
        &c,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &c);
    result(
        &mut s,
        "not-git",
        EffectResult::CheckpointCaptured {
            status: CheckpointStatus::Missing,
            baselines: vec![],
            run: third,
            attempt: Some(c),
            checkpoint: CheckpointId::new("missing-cp").unwrap(),
            file_ref: String::new(),
        },
    );
    assert_eq!(s.runs[2].status, RunStatus::Completed);
    assert_eq!(
        command(
            &mut s,
            "rollback-missing",
            Command::Rollback {
                checkpoint: CheckpointId::new("missing-cp").unwrap(),
                restore_files: false,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "checkpoint-not-ready".into()
        }
    );
}
// T3 CheckpointRollbackService.ts: rolled-back captures are discarded and later checkpoints become stale.
#[test]
fn rollback_discards_pending_captures_and_invalidates_later_checkpoints() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    let later = checkpoint(&mut s, &second, &b, "cp-second");
    let (third, c) = running(&mut s, "third");
    command(&mut s, "stop-third", Command::Stop);
    provider(
        &mut s,
        "stopped",
        &c,
        ProviderEvent::TurnFinished {
            status: RunStatus::Interrupted,
            native_head: None,
        },
    );
    assert!(s.captures.contains_key(&third));
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    let rollback = command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: true,
        },
    );
    let [effect] = rollback.effects.as_slice() else {
        panic!("{:?}", rollback.effects)
    };
    let EffectBody::Rollback {
        command: id,
        providers,
        restore,
        stale_file_refs,
    } = &effect.body
    else {
        panic!()
    };
    assert_eq!(id.as_str(), "rollback");
    assert_eq!(providers.len(), 1);
    assert_eq!(restore.as_ref().unwrap().file_ref, "cp-first");
    assert_eq!(stale_file_refs, &vec!["cp-second".to_string()]);
    result(
        &mut s,
        "rolled-back",
        EffectResult::RollbackFinished {
            command: CommandId::new("rollback").unwrap(),
            bindings: vec![],
        },
    );
    assert!(s.captures.is_empty());
    assert_eq!(s.runs[1].status, RunStatus::RolledBack);
    assert_eq!(s.runs[2].status, RunStatus::RolledBack);
    assert_eq!(s.runs[3].status, RunStatus::Queued);
    assert_eq!(
        s.checkpoints.iter().find(|c| c.id == later).unwrap().status,
        CheckpointStatus::Stale
    );
    assert_eq!(
        command(
            &mut s,
            "rollback-stale",
            Command::Rollback {
                checkpoint: later,
                restore_files: true,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "checkpoint-not-ready".into()
        }
    );
    command(&mut s, "resume", Command::ResumeQueue);
    assert_eq!(s.runs[3].status, RunStatus::Starting);
    let _ = second;
}
#[test]
fn rollback_without_provider_rewind_or_file_restore_still_reports_one_result() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let step = command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: false,
        },
    );
    assert!(matches!(
        &step.effects[..],
        [Effect {
            body: EffectBody::Rollback {
                providers,
                restore: None,
                ..
            },
            ..
        }] if providers.is_empty()
    ));
    result(
        &mut s,
        "done",
        EffectResult::RollbackFinished {
            command: CommandId::new("rollback").unwrap(),
            bindings: vec![],
        },
    );
    assert!(s.rollback.is_none());
}

#[test]
fn native_text_and_plan_streams_append_without_entity_replacements() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    provider(
        &mut s,
        "text-start",
        &a,
        ProviderEvent::TextDelta {
            key: "text".into(),
            kind: ProviderItem::Text,
            text: "日本語".into(),
        },
    );
    let wrapped = provider(
        &mut s,
        "wrapped",
        &a,
        ProviderEvent::NativeOutput {
            echoed_prompts: vec![],
            acknowledged_prompt: None,
            root: true,
            result: None,
            events: vec![ProviderEvent::TextDelta {
                key: "text".into(),
                kind: ProviderItem::Text,
                text: "追記".into(),
            }],
        },
    );
    assert!(
        matches!(&wrapped.facts[..],[Fact { body:FactBody::ItemTextAppended { offset:9,text,.. },.. }] if text == "追記")
    );
    provider(
        &mut s,
        "plan-start",
        &a,
        ProviderEvent::PlanDelta {
            key: "proposal".into(),
            text: "計画".into(),
        },
    );
    let appended = provider(
        &mut s,
        "plan-delta",
        &a,
        ProviderEvent::PlanDelta {
            key: "proposal".into(),
            text: "追記".into(),
        },
    );
    assert!(matches!(
        &appended.facts[..],
        [
            Fact {
                body: FactBody::PlanMarkdownAppended { offset: 6, .. },
                ..
            },
            Fact {
                body: FactBody::ItemTextAppended { offset: 6, .. },
                ..
            }
        ]
    ));
    assert_eq!(s.plans[0].markdown, "計画追記");
    provider(
        &mut s,
        "todo-empty",
        &a,
        ProviderEvent::Plan {
            kind: PlanKind::Todo,
            key: "todo".into(),
            markdown: String::new(),
            steps: vec![],
        },
    );
    assert_eq!(s.plans[1].kind, PlanKind::Todo);
    assert!(
        s.items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::TodoList { .. }))
    );
}

#[test]
fn rollback_resets_post_boundary_sessions_and_uses_replacement_native_identity() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let mut later = selection();
    later.instance = "later-provider".into();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: later },
    );
    let Reply::Run(second) = command(
        &mut s,
        "second",
        send_message("second", DispatchMode::StartImmediately),
    )
    .reply
    else {
        panic!()
    };
    let b = s.runs[1].attempt.clone().unwrap();
    provider(
        &mut s,
        "later-session",
        &b,
        ProviderEvent::SessionReady {
            native_thread: "native-later".into(),
        },
    );
    provider(
        &mut s,
        "later-started",
        &b,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    let step = command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: false,
        },
    );
    let EffectBody::Rollback { providers, .. } = &step.effects[0].body else {
        panic!()
    };
    assert_eq!(
        providers,
        &vec![ProviderRollback {
            instance: "later-provider".into(),
            command: ProviderCommand::Rollback {
                native_thread: "native-later".into(),
                absolute_head: None,
            },
        }]
    );
    let binding = NativeBinding {
        instance: "later-provider".into(),
        thread: "native-replacement".into(),
        head: None,
    };
    assert_eq!(
        result(
            &mut s,
            "stale",
            EffectResult::RollbackFinished {
                command: CommandId::new("stale").unwrap(),
                bindings: vec![binding.clone()]
            }
        )
        .reply,
        Reply::Ignored
    );
    result(
        &mut s,
        "restored",
        EffectResult::RollbackFinished {
            command: CommandId::new("rollback").unwrap(),
            bindings: vec![binding],
        },
    );
    let step = command(
        &mut s,
        "continue",
        send_message("continue", DispatchMode::StartImmediately),
    );
    assert!(step.effects.iter().any(|effect| matches!(&effect.body, EffectBody::Provider(ProviderCommand::Start {native_thread:Some(thread),..}) if thread == "native-replacement")));
}

#[test]
fn async_question_answer_steers_the_current_turn_and_rejects_blank_answers_atomically() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    provider(
        &mut s,
        "question",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "question".into(),
            body: RequestBody::Questions {
                questions: vec![Question {
                    required: true,
                    id: "q".into(),
                    header: "Choice".into(),
                    question: "Which?".into(),
                    multiple: false,
                    options: vec![],
                }],
            },
            capability: ResponseCapability::Message,
        },
    );
    let request = s.requests[0].id.clone();
    let respond = |text: &str| Command::Respond {
        request: request.clone(),
        decision: None,
        answers: Some(Answers::from([("q".into(), Answer::Text(text.into()))])),
        attachments: BTreeMap::new(),
    };
    let original = s.clone();
    assert!(matches!(
        command(&mut s, "blank", respond(" ")).reply,
        Reply::Rejected { .. }
    ));
    assert_eq!(s, original);
    let step = command(&mut s, "answer", respond("  Option One  "));
    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.requests[0].status, RequestStatus::Resolved);
    assert!(step.effects.iter().any(|effect| matches!(&effect.body,EffectBody::Provider(ProviderCommand::Steer {text,..}) if text == "Which?\nOption One")));
    assert_eq!(s.messages.last().unwrap().creation_source, "server");
}

#[test]
// T3 ProviderTurnStartService.ts: an uncertain native delivery continues in a fresh native thread.
fn context_delivery_is_pending_until_acceptance_and_ambiguous_delivery_is_not_repeated() {
    let mut s = state();
    let (_, a) = running(&mut s, "original");
    finish(&mut s, &a);
    let mut target = selection();
    target.instance = "claude".into();
    target.driver = Driver::Claude;
    command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: target },
    );
    let step = command(
        &mut s,
        "offer",
        send_message("continue", DispatchMode::StartImmediately),
    );
    assert!(step.effects.iter().any(|effect|matches!(&effect.body,EffectBody::Provider(ProviderCommand::Start {context:Some(history),..}) if render_history(history).contains("original"))));
    assert_eq!(
        s.transfers[0].delivery.as_ref().unwrap().status,
        ContextDeliveryStatus::Pending
    );
    let a = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "bound",
        &a,
        ProviderEvent::SessionReady {
            native_thread: "new-native".into(),
        },
    );
    assert_eq!(
        s.transfers[0]
            .delivery
            .as_ref()
            .unwrap()
            .native_thread
            .as_deref(),
        Some("new-native")
    );
    result(
        &mut s,
        "lost",
        EffectResult::ProviderFailed {
            attempt: a,
            operation: ProviderOperation::Start,
            message: "connection lost".into(),
            message_id: None,
            turn_completed: false,
            session_lost: false,
        },
    );
    let retry = command(
        &mut s,
        "retry",
        send_message("retry", DispatchMode::StartImmediately),
    );
    assert!(retry.facts.iter().any(|fact| matches!(&fact.body, FactBody::NativeSessionCleared { instance } if instance == "claude")));
    let history = retry
        .effects
        .iter()
        .find_map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Start {
                native_thread: None,
                context: Some(history),
                ..
            }) => Some(render_history(history)),
            _ => None,
        })
        .unwrap();
    assert!(history.contains("original") && history.contains("continue"));
    assert_eq!(
        s.items
            .iter()
            .filter_map(|item| match &item.kind {
                ItemKind::Error { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["connection lost"]
    );
    let a = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "ready",
        &a,
        ProviderEvent::SessionReady {
            native_thread: "replacement-native".into(),
        },
    );
    provider(
        &mut s,
        "accepted",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    assert_eq!(
        s.transfers[0].delivery.as_ref().unwrap().status,
        ContextDeliveryStatus::Inline
    );
    finish(&mut s, &a);
    let next = command(
        &mut s,
        "next",
        send_message("next", DispatchMode::StartImmediately),
    );
    assert!(next.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::Provider(ProviderCommand::Start { context: None, .. })
    )));
}

#[test]
fn injected_context_is_durable_before_turn_start_and_budget_failure_preserves_input() {
    let mut s = state();
    let (_, a) = running(&mut s, "original");
    finish(&mut s, &a);
    let mut target = selection();
    target.instance = "target".into();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: target },
    );
    command(
        &mut s,
        "offer",
        send_message("continue", DispatchMode::StartImmediately),
    );
    let a = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "bound",
        &a,
        ProviderEvent::SessionReady {
            native_thread: "target-native".into(),
        },
    );
    provider(&mut s, "injected", &a, ProviderEvent::ContextInjected);
    assert_eq!(
        s.transfers[0].delivery.as_ref().unwrap().status,
        ContextDeliveryStatus::Injected
    );
    provider(
        &mut s,
        "accepted",
        &a,
        ProviderEvent::TurnStarted {
            native_turn: Some("turn".into()),
        },
    );
    assert_eq!(
        s.transfers[0].delivery.as_ref().unwrap().status,
        ContextDeliveryStatus::Injected
    );
    finish(&mut s, &a);
    let mut next = selection();
    next.instance = "small-window".into();
    command(&mut s, "small", Command::SwitchProvider { selection: next });
    let step = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "policy".into(),
            input: Input::HandoffPolicy {
                instance: "small-window".into(),
                model_window: Some(20_000),
                token_cap: 16_000,
            },
        },
    );
    s = fold(&s, &step.facts).unwrap();
    let text = "界".repeat(30_000);
    let mut send = send_message("large", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut send {
        message.text = text.clone();
    }
    let step = command(&mut s, "large", send);
    assert!(!step.effects.iter().any(|effect| matches!(
        effect.body,
        EffectBody::Provider(ProviderCommand::Start { .. })
    )));
    assert_eq!(s.messages.last().unwrap().text, text);
    assert_eq!(s.transfers.last().unwrap().delivery, None);
    assert!(s.items.iter().any(
        |item| matches!(&item.kind,ItemKind::Error {message,..} if message==HANDOFF_BUDGET_ERROR)
    ));
}

#[test]
fn late_native_usage_moves_the_baseline_without_billing_the_live_turn() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let counter = |input, cached_input, output| UsageCounters {
        input,
        cached_input,
        output,
        cache_creation: None,
        reasoning: 0,
    };
    provider(
        &mut s,
        "usage-first",
        &a,
        ProviderEvent::UsageTotals {
            native_thread: "native-thread".into(),
            native_turn: "first".into(),
            total: counter(80, 8, 16),
            last: counter(80, 8, 16),
        },
    );
    finish(&mut s, &a);
    let (_, b) = running(&mut s, "next");
    provider(
        &mut s,
        "late",
        &a,
        ProviderEvent::UsageTotals {
            native_thread: "native-thread".into(),
            native_turn: "first".into(),
            total: counter(100, 10, 20),
            last: counter(20, 2, 4),
        },
    );
    provider(
        &mut s,
        "next-turn",
        &b,
        ProviderEvent::TurnStarted {
            native_turn: Some("next-turn".into()),
        },
    );
    provider(
        &mut s,
        "usage-next",
        &b,
        ProviderEvent::UsageTotals {
            native_thread: "native-thread".into(),
            native_turn: "next-turn".into(),
            total: counter(104, 12, 21),
            last: counter(4, 2, 1),
        },
    );
    finish(&mut s, &b);
    let usage = s.attempts.last().unwrap().turn_usage.as_ref().unwrap();
    assert_eq!(usage.status, UsageStatus::Complete);
    assert_eq!(usage.input, Some(4));
    assert_eq!(usage.output, Some(1));
}

#[test]
fn delegated_notifications_report_the_original_count_labels_and_child_links() {
    let mut s = state();
    let (run, a) = starting(&mut s, "parent");
    let ids = ["a", "b", "c"].map(|id| NodeId::new(id).unwrap());
    for (index, prompt) in ["Review src/math.ts", "Write tests", "Update docs"]
        .into_iter()
        .enumerate()
    {
        command(
            &mut s,
            &format!("delegate-{index}"),
            Command::Delegate {
                task: ids[index].clone(),
                child: ThreadId::new(format!("thread:{}", ids[index])).unwrap(),
                prompt: prompt.into(),
                selection: selection(),
                wake: CompletionWake::Always,
            },
        );
    }
    for task in &ids[..2] {
        let source_message = s
            .tasks
            .iter()
            .find(|candidate| &candidate.id == task)
            .and_then(|task| task.original_message.clone());
        command(
            &mut s,
            &format!("finish-{task}"),
            Command::TaskResult {
                generation: None,
                source_message,
                context: None,
                task: task.clone(),
                status: ItemStatus::Completed,
                result: "Done".into(),
            },
        );
    }
    let notification = delegated_notification(&ids[..2], &run, &s.tasks);
    assert_eq!(
        notification.summary,
        "2 of 3 delegated tasks finished: Review src/math.ts, Write tests"
    );
    assert_eq!(
        notification.source,
        NotificationSource::Delegated {
            task_ids: ids[..2].to_vec()
        }
    );
    let only = delegated_notification(&ids[..1], &run, &s.tasks);
    assert_eq!(
        only.summary,
        "Delegated task \"Review src/math.ts\" finished"
    );
    assert_eq!(only.child_thread, Some(ThreadId::new("thread:a").unwrap()));
    let Reply::Run(delivery) = command(
        &mut s,
        "wake",
        Command::AcceptTaskWake {
            task_ids: ids[..2].to_vec(),
        },
    )
    .reply
    else {
        panic!()
    };
    let message = &s.messages[s.messages.len() - 1];
    assert_eq!(
        message.text,
        "Delegated tasks a, b reached terminal states. Use task_status with each taskId to read the results."
    );
    assert_eq!(message.notification, Some(notification.clone()));
    finish(&mut s, &a);
    assert_eq!(s.active_run().unwrap().id, delivery);
    assert!(s.activity_items().iter().any(|item|matches!(&item.kind,ItemKind::Notification {notification:recorded} if recorded==&notification)));
    assert!(
        s.items
            .iter()
            .all(|item| !matches!(item.kind, ItemKind::Notification { .. }))
    );
}

// T3 Orchestrator.ts:8722 and :7333: a queued sibling joins the parent run's wake, and
// cancelling that wake disposes the whole cohort.
#[test]
fn queued_siblings_share_one_wake_and_cancelling_it_disposes_the_cohort() {
    let mut s = state();
    let (_, a) = starting(&mut s, "parent");
    let mut deliveries = vec![];
    for id in ["a", "b"] {
        let task = NodeId::new(id).unwrap();
        command(
            &mut s,
            &format!("delegate-{id}"),
            Command::Delegate {
                task: task.clone(),
                child: ThreadId::new(format!("child-{id}")).unwrap(),
                prompt: id.into(),
                selection: selection(),
                wake: CompletionWake::Always,
            },
        );
        let source_message = s
            .tasks
            .iter()
            .find(|candidate| candidate.id == task)
            .and_then(|task| task.original_message.clone());
        command(
            &mut s,
            &format!("finish-{id}"),
            Command::TaskResult {
                generation: None,
                source_message,
                context: None,
                task: task.clone(),
                status: ItemStatus::Completed,
                result: "Done".into(),
            },
        );
        deliveries.push(
            command(
                &mut s,
                &format!("wake-{id}"),
                Command::AcceptTaskWake {
                    task_ids: vec![task],
                },
            )
            .reply,
        );
    }
    let Reply::Run(wake) = deliveries[0].clone() else {
        panic!("{deliveries:?}")
    };
    assert_eq!(deliveries[1], Reply::Accepted);
    assert_eq!(s.runs.len(), 2);
    let message = s.message(&s.runs[1].message).unwrap();
    assert_eq!(
        message.text,
        "Delegated tasks a, b reached terminal states. Use task_status with each taskId to read the results."
    );
    assert!(
        s.tasks
            .iter()
            .all(|task| task.delivery == DeliveryState::Claimed)
    );
    command(&mut s, "cancel", Command::CancelQueued { run: wake });
    assert!(
        s.tasks
            .iter()
            .all(|task| task.delivery == DeliveryState::Disposed)
    );
    assert!(
        s.tasks
            .iter()
            .all(|task| task.status == ItemStatus::Completed)
    );
    command(&mut s, "stop", Command::Stop);
    finish(&mut s, &a);
    let step = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "timer".into(),
            input: Input::Timer,
        },
    );
    assert!(step.effects.is_empty());
}

#[test]
fn first_scoped_capture_requires_a_baseline_and_late_capture_uses_its_original_scope() {
    let mut s = state();
    let scope = CheckpointScope {
        id: CheckpointScopeId::new("scope-one").unwrap(),
        cwd: "/workspace/one".into(),
    };
    let step = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "scope".into(),
            input: Input::CheckpointScope {
                run: None,
                attempt: None,
                scope: Some(scope.clone()),
            },
        },
    );
    s = fold(&s, &step.facts).unwrap();
    let (run, a) = running(&mut s, "first");
    let done = provider(
        &mut s,
        "finished",
        &a,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: Some("native-head".into()),
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Waiting);
    assert!(done.effects.iter().any(|effect|matches!(&effect.body,EffectBody::CaptureCheckpoint {scope:captured,native_baseline_heads,..} if captured==&scope && native_baseline_heads.is_empty())));
    let missing = EffectResult::CheckpointCaptured {
        status: CheckpointStatus::Ready,
        run: run.clone(),
        attempt: Some(a.clone()),
        checkpoint: CheckpointId::new("captured").unwrap(),
        file_ref: "after".into(),
        baselines: vec![],
    };
    assert_eq!(
        result(&mut s, "missing", missing.clone()).reply,
        Reply::Rejected {
            reason: "incomplete-checkpoint-baseline".into()
        }
    );
    assert!(s.captures.contains_key(&run));
    let next_scope = CheckpointScope {
        id: CheckpointScopeId::new("scope-two").unwrap(),
        cwd: "/workspace/two".into(),
    };
    let step = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "move".into(),
            input: Input::CheckpointScope {
                run: None,
                attempt: None,
                scope: Some(next_scope.clone()),
            },
        },
    );
    s = fold(&s, &step.facts).unwrap();
    let mut captured = missing;
    if let EffectResult::CheckpointCaptured { baselines, .. } = &mut captured {
        baselines.push(CapturedBaseline {
            status: CheckpointStatus::Ready,
            checkpoint: CheckpointId::new("initial").unwrap(),
            ordinal: 0,
            file_ref: "before".into(),
            native_heads: BTreeMap::new(),
        });
    }
    result(&mut s, "captured", captured.clone());
    assert_eq!(s.runs[0].status, RunStatus::Completed);
    assert_eq!(s.checkpoints[0].run_ordinal, 0);
    assert_eq!(s.checkpoints[1].scope, Some(scope));
    assert_eq!(s.checkpoint_scope, Some(next_scope));
    let original = s.clone();
    assert_eq!(result(&mut s, "duplicate", captured).reply, Reply::Ignored);
    assert_eq!(s, original);
    let rollback = command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: CheckpointId::new("initial").unwrap(),
            restore_files: true,
        },
    );
    assert!(rollback.effects.iter().any(|effect|matches!(&effect.body,EffectBody::Rollback {restore:Some(RestoreFiles {scope:Some(scope),file_ref,..}),..} if scope.cwd=="/workspace/one" && file_ref=="before")));
}

#[test]
fn interrupt_failure_keeps_the_root_and_children_live_until_provider_confirmation() {
    let mut s = state();
    let (run, attempt) = running(&mut s, "parent");
    provider(
        &mut s,
        "child",
        &attempt,
        ProviderEvent::SubagentStarted {
            background: true,
            native_thread: Some("native-child".into()),
            key: "child".into(),
            parent: None,
            prompt: "Continue working".into(),
            model: None,
        },
    );
    command(&mut s, "stop", Command::Stop);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
    assert_eq!(s.runs[0].status, RunStatus::Running);
    result(
        &mut s,
        "interrupt-failed",
        EffectResult::ProviderFailed {
            attempt: attempt.clone(),
            operation: ProviderOperation::Interrupt,
            message: "temporary RPC failure".into(),
            message_id: None,
            turn_completed: false,
            session_lost: false,
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Running);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
    provider(
        &mut s,
        "root-stopped",
        &attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Interrupted,
            native_head: None,
        },
    );
    assert_eq!(s.runs[0].status, RunStatus::Interrupted);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
    assert!(
        s.items
            .iter()
            .any(|item| matches!(item.kind, ItemKind::Subagent { .. })
                && item.status == ItemStatus::Running)
    );
    // A stable root alone cannot authorize restoring files still used by a child.
    let cp = checkpoint(&mut s, &run, &attempt, "root-cp");
    assert!(matches!(
        command(
            &mut s,
            "unsafe-rollback",
            Command::Rollback {
                checkpoint: cp,
                restore_files: true
            }
        )
        .reply,
        Reply::Rejected { .. }
    ));
}

#[test]
fn provider_selection_and_runtime_changes_are_blocked_during_rollback() {
    let mut s = state();
    let (run, attempt) = running(&mut s, "first");
    finish(&mut s, &attempt);
    let cp = checkpoint(&mut s, &run, &attempt, "first-cp");
    command(
        &mut s,
        "rollback",
        Command::Rollback {
            checkpoint: cp,
            restore_files: false,
        },
    );
    for (i, change) in [
        Command::SelectModel {
            selection: selection(),
        },
        Command::SwitchProvider {
            selection: selection(),
        },
        Command::RuntimeMode {
            mode: RuntimeMode::Auto,
        },
        Command::InteractionMode {
            mode: InteractionMode::Plan,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let before = s.clone();
        assert_eq!(
            command(&mut s, &format!("change-{i}"), change).reply,
            Reply::Rejected {
                reason: "rollback-pending".into()
            }
        );
        assert_eq!(s, before);
    }
}

proptest! {
    #[test]
    fn failed_control_operations_never_release_a_running_native_turn(op in 0usize..4, failures in 1usize..12) {
        let mut s=state();
        let (_,attempt)=running(&mut s,"run");
        let operation=[ProviderOperation::Steer,ProviderOperation::Interrupt,ProviderOperation::Respond,ProviderOperation::SetModel][op];
        for i in 0..failures {
            result(&mut s,&format!("failed-{i}"),EffectResult::ProviderFailed{attempt:attempt.clone(),operation,message:"RPC failed".into(),message_id:None,turn_completed:false,session_lost:false});
            prop_assert_eq!(s.runs[0].status,RunStatus::Running);
            prop_assert_eq!(s.attempts[0].status,AttemptStatus::Running);
        }
    }
}
fn round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value);
    let bytes = postcard::to_allocvec(value).unwrap();
    assert_eq!(&postcard::from_bytes::<T>(&bytes).unwrap(), value);
}
fn captured_image() -> Attachment {
    Attachment {
        kind: AttachmentKind::Image,
        source: Some(CapturedWindow {
            app_name: "Editor".into(),
            window_title: "main.rs".into(),
            accessible_text: Some("fn main".into()),
            accessibility: Some(Accessibility::ElementTree {
                coordinate_space: "window".into(),
                image_size: ImageSize {
                    width: 800,
                    height: 600,
                },
                truncated: false,
                root: Box::new(AccessibilityNode {
                    role: "window".into(),
                    name: Some("main.rs".into()),
                    value: None,
                    description: None,
                    bounds: Some(Bounds {
                        x: -4,
                        y: 0,
                        width: 800,
                        height: 600,
                    }),
                    state: Some(Json(
                        serde_json::json!({"focused":true,"items":[1,2.5,null]}),
                    )),
                    actions: vec!["press".into()],
                    children: vec![],
                }),
            }),
        }),
        id: "capture".into(),
        name: "capture.png".into(),
        mime_type: "image/png".into(),
        path: "/tmp/capture.png".into(),
        size: 10,
    }
}
#[test]
fn wire_encodings_round_trip_state_facts_commands_and_effects() {
    round_trip(&Accessibility::FlatText {
        text: "text".into(),
        truncated: true,
    });
    let mut s = state();
    let mut steps = vec![];
    let send = Command::Send(SendMessage {
        created_by: MessageAuthor::User,
        creation_source: "client".into(),
        id: MessageId::new("captured").unwrap(),
        text: "Look at this window".into(),
        attachments: vec![captured_image()],
        selection: None,
        mode: DispatchMode::StartImmediately,
        intent: Some(DeliveryIntent::Auto),
        source_plan: None,
        title_seed: None,
    });
    round_trip(&send);
    steps.push(command(&mut s, "captured", send));
    let attempt = s.runs[0].attempt.clone().unwrap();
    for (key, event) in [
        (
            "ready",
            ProviderEvent::SessionReady {
                native_thread: "native".into(),
            },
        ),
        (
            "turn",
            ProviderEvent::TurnStarted {
                native_turn: Some("turn".into()),
            },
        ),
        (
            "tool",
            ProviderEvent::ItemFinished {
                key: "tool".into(),
                kind: ProviderItem::Tool {
                    presentation: ToolPresentation {
                        title: Some("Read".into()),
                        source: Some(Json(serde_json::json!({"key":"mcp:x"}))),
                    },
                    name: "read".into(),
                    input: Json(serde_json::json!({"path":"a"})),
                    output: Some(Json(serde_json::json!([{"text":"b"}]))),
                },
                text: Some("b".into()),
                status: ItemStatus::Completed,
            },
        ),
        (
            "approval",
            ProviderEvent::RequestOpened {
                owner_path: vec![],
                key: "1".into(),
                body: RequestBody::Approval {
                    kind: "command".into(),
                    title: "ls".into(),
                    detail: None,
                    options: vec![ApprovalOption {
                        label: "Approve".into(),
                        decision: ApprovalDecision::Accept,
                    }],
                    input: Json(serde_json::json!({"command":"ls"})),
                },
                capability: ResponseCapability::Live,
            },
        ),
        (
            "plan",
            ProviderEvent::Plan {
                kind: PlanKind::Todo,
                key: "todo".into(),
                markdown: String::new(),
                steps: vec![PlanStep {
                    text: "step".into(),
                    status: "pending".into(),
                }],
            },
        ),
        (
            "child",
            ProviderEvent::SubagentStarted {
                background: true,
                native_thread: None,
                key: "agent".into(),
                parent: None,
                prompt: "child".into(),
                model: None,
            },
        ),
    ] {
        round_trip(&event);
        steps.push(provider(&mut s, key, &attempt, event));
    }
    finish(&mut s, &attempt);
    for step in &steps {
        round_trip(step);
        for fact in &step.facts {
            round_trip(fact);
        }
        for effect in &step.effects {
            round_trip(effect);
            if let EffectBody::SendToThread { command, .. } = &effect.body {
                round_trip(command.as_ref());
            }
        }
    }
    round_trip(&s);
    assert!(!s.items.is_empty() && !s.requests.is_empty() && !s.tasks.is_empty());
}
fn switch_instance(s: &mut State, key: &str, instance: &str) {
    let mut target = selection();
    target.instance = instance.into();
    command(s, key, Command::SwitchProvider { selection: target });
}
fn start_context(step: &Step) -> Option<(Option<String>, String)> {
    step.effects.iter().find_map(|effect| match &effect.body {
        EffectBody::Provider(ProviderCommand::Start {
            native_thread,
            context,
            ..
        }) => Some((
            native_thread.clone(),
            context.as_ref().map(render_history).unwrap_or_default(),
        )),
        _ => None,
    })
}
// T3 ProviderTurnStartService.ts: a failed native resume continues in a fresh session with full history.
#[test]
fn lost_native_session_restarts_the_attempt_with_portable_history() {
    let mut s = state();
    let (_, a) = running(&mut s, "earlier work");
    finish(&mut s, &a);
    command(
        &mut s,
        "resume",
        send_message("resume", DispatchMode::StartImmediately),
    );
    let first = s.active_run().unwrap().attempt.clone().unwrap();
    let step = result(
        &mut s,
        "lost",
        EffectResult::ProviderFailed {
            attempt: first.clone(),
            operation: ProviderOperation::Start,
            message: "thread not found".into(),
            message_id: None,
            turn_completed: false,
            session_lost: true,
        },
    );
    let (native, history) = start_context(&step).unwrap();
    assert_eq!(native, None);
    assert!(history.contains("earlier work") && history.contains("full_thread_summary"));
    let run = s.active_run().unwrap();
    assert_eq!(run.status, RunStatus::Starting);
    assert_ne!(run.attempt.as_ref(), Some(&first));
    assert_eq!(
        s.attempts.iter().find(|a| a.id == first).unwrap().status,
        AttemptStatus::Failed
    );
    assert!(
        !s.items
            .iter()
            .any(|i| matches!(i.kind, ItemKind::Error { .. }))
    );
    let again = s.active_run().unwrap().attempt.clone().unwrap();
    let step = result(
        &mut s,
        "lost-again",
        EffectResult::ProviderFailed {
            attempt: again,
            operation: ProviderOperation::Start,
            message: "spawn failed".into(),
            message_id: None,
            turn_completed: false,
            session_lost: true,
        },
    );
    assert!(step.effects.is_empty());
    assert_eq!(s.runs[1].status, RunStatus::Failed);
}
// T3 ContextHandoffDelivery.ts: only a delivery recorded for a concrete native thread is uncertain.
#[test]
fn a_handoff_that_failed_before_a_native_thread_existed_is_delivered_again() {
    let mut s = state();
    let (_, a) = running(&mut s, "original");
    finish(&mut s, &a);
    switch_instance(&mut s, "switch", "other");
    command(
        &mut s,
        "first",
        send_message("first", DispatchMode::StartImmediately),
    );
    let attempt = s.active_run().unwrap().attempt.clone().unwrap();
    result(
        &mut s,
        "spawn-failed",
        EffectResult::ProviderFailed {
            attempt,
            operation: ProviderOperation::Start,
            message: "spawn failed".into(),
            message_id: None,
            turn_completed: false,
            session_lost: false,
        },
    );
    assert_eq!(
        s.transfers[0].delivery.as_ref().unwrap().native_thread,
        None
    );
    let retry = command(
        &mut s,
        "retry",
        send_message("retry", DispatchMode::StartImmediately),
    );
    let (native, history) = start_context(&retry).unwrap();
    assert_eq!(native, None);
    assert!(history.contains("original"));
}
// T3 ProviderTurnStartService.ts: inputs the provider never accepted are handed to the same session.
#[test]
fn inputs_that_never_reached_the_native_session_are_handed_back_to_it() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    finish(&mut s, &a);
    command(
        &mut s,
        "Please rename the module",
        send_message("Please rename the module", DispatchMode::StartImmediately),
    );
    let attempt = s.active_run().unwrap().attempt.clone().unwrap();
    result(
        &mut s,
        "start-failed",
        EffectResult::ProviderFailed {
            attempt,
            operation: ProviderOperation::Start,
            message: "overloaded".into(),
            message_id: None,
            turn_completed: false,
            session_lost: false,
        },
    );
    let step = command(
        &mut s,
        "continue",
        send_message("continue", DispatchMode::StartImmediately),
    );
    let (native, history) = start_context(&step).unwrap();
    assert_eq!(native.as_deref(), Some("native-thread"));
    assert!(history.contains("Please rename the module"));
    assert!(history.contains("run-status=failed"));
    assert!(!history.contains("[Historical user; user_message; thread=thread; run=run:5:first"));
    let delivered = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "accepted",
        &delivered,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &delivered);
    let next = command(
        &mut s,
        "next",
        send_message("next", DispatchMode::StartImmediately),
    );
    assert_eq!(start_context(&next).unwrap().1, "");
}
// T3 ProviderTurnStartService.ts: without telemetry, prior native attachments count against the window.
#[test]
fn native_occupancy_estimate_counts_inputs_and_attachments_that_reached_the_session() {
    let outcome = |images: usize| {
        let mut s = state();
        let mut send = send_message("with images", DispatchMode::StartImmediately);
        if let Command::Send(message) = &mut send {
            message.attachments = (0..images)
                .map(|index| {
                    let mut image = captured_image();
                    image.id = index.to_string();
                    image.source = None;
                    image
                })
                .collect();
        }
        command(&mut s, "with images", send);
        let a = s.active_run().unwrap().attempt.clone().unwrap();
        provider(
            &mut s,
            "session",
            &a,
            ProviderEvent::SessionReady {
                native_thread: "native-thread".into(),
            },
        );
        provider(
            &mut s,
            "started",
            &a,
            ProviderEvent::TurnStarted { native_turn: None },
        );
        finish(&mut s, &a);
        switch_instance(&mut s, "away", "other");
        let (_, b) = running(&mut s, "elsewhere");
        finish(&mut s, &b);
        switch_instance(&mut s, "back", "codex");
        let step = ThreadMachine::step(
            &s,
            &InputEnvelope {
                at: at(),
                key: "policy".into(),
                input: Input::HandoffPolicy {
                    instance: "codex".into(),
                    model_window: Some(60_000),
                    token_cap: 16_000,
                },
            },
        );
        s = fold(&s, &step.facts).unwrap();
        let step = command(
            &mut s,
            "return",
            send_message("return", DispatchMode::StartImmediately),
        );
        start_context(&step).is_some()
    };
    assert!(outcome(0));
    assert!(!outcome(6));
}
fn accept_child(step: &Step) -> State {
    let accept = step
        .effects
        .iter()
        .find_map(|effect| match &effect.body {
            EffectBody::SendToThread { command, .. }
                if matches!(command.as_ref(), Command::AcceptFork { .. }) =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .unwrap();
    let mut child = State::default();
    command(&mut child, "accept", accept);
    child
}
// T3 ProjectionStore.ts: a fork of a fork keeps the inherited prefix and its message fields.
#[test]
fn forking_a_fork_keeps_the_ancestor_conversation_and_its_messages() {
    let mut s = state();
    let mut send = send_message("ancestor request", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut send {
        message.attachments = vec![captured_image()];
    }
    command(&mut s, "ancestor request", send);
    let a = s.active_run().unwrap().attempt.clone().unwrap();
    provider(
        &mut s,
        "started",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &a);
    let run = s.runs[0].id.clone();
    let fork = command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: run,
            title: None,
        },
    );
    let mut child = accept_child(&fork);
    let (child_run, b) = running(&mut child, "child request");
    finish(&mut child, &b);
    let grandchild = command(
        &mut child,
        "fork-again",
        Command::Fork {
            target: ThreadId::new("grandchild").unwrap(),
            through_run: child_run,
            title: None,
        },
    );
    let EffectBody::ForkNative { .. } = &grandchild.effects[0].body else {
        panic!("{:?}", grandchild.effects)
    };
    let forked = result(
        &mut child,
        "forked",
        EffectResult::NativeForked {
            command: CommandId::new("fork-again").unwrap(),
            native_thread: "grandchild-native".into(),
        },
    );
    let grandchild = accept_child(&forked);
    let texts = grandchild
        .visible_items()
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::UserMessage { .. }))
        .map(|item| item.text.clone())
        .collect::<Vec<_>>();
    assert_eq!(texts, ["ancestor request", "child request"]);
    assert_eq!(
        grandchild
            .visible_items()
            .iter()
            .filter(|item| matches!(item.kind, ItemKind::Fork { .. }))
            .count(),
        2
    );
    let ItemKind::UserMessage { message } = &grandchild.inherited_items[0].kind else {
        panic!()
    };
    let message = grandchild.message(message).unwrap();
    assert_eq!(message.attachments, vec![captured_image()]);
    assert_eq!(message.intent, InputIntent::TurnStart);
    assert_eq!(message.created_by, MessageAuthor::User);
    let Some(Transfer { history, .. }) = grandchild.transfers.first() else {
        panic!()
    };
    assert!(render_history(history).contains("ancestor request"));
}
// T3 CommandPolicy.ts: unsuccessful sources fork from bounded portable history; T3 ThreadForkService.ts statuses.
#[test]
fn unsuccessful_and_cancelled_runs_fork_from_bounded_portable_history() {
    let mut s = state();
    let (failed, a) = running(&mut s, "failed request");
    provider(
        &mut s,
        "failed",
        &a,
        ProviderEvent::TurnFinished {
            status: RunStatus::Failed,
            native_head: None,
        },
    );
    let (_, b) = running(&mut s, "later answer");
    finish(&mut s, &b);
    let (_, c) = running(&mut s, "still running");
    let fork = command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: failed,
            title: None,
        },
    );
    assert!(
        !fork
            .effects
            .iter()
            .any(|effect| matches!(effect.body, EffectBody::ForkNative { .. }))
    );
    let child = accept_child(&fork);
    let history = render_history(&child.transfers[0].history);
    assert!(history.contains("failed request") && !history.contains("later answer"));
    assert_eq!(child.transfers[0].native_fork, None);
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    let queued = s.runs[3].id.clone();
    command(
        &mut s,
        "cancel",
        Command::CancelQueued {
            run: queued.clone(),
        },
    );
    let _ = c;
    assert!(matches!(
        command(
            &mut s,
            "fork-cancelled",
            Command::Fork {
                target: ThreadId::new("fork-cancelled").unwrap(),
                through_run: queued,
                title: None,
            },
        )
        .reply,
        Reply::Thread(_)
    ));
}
#[test]
fn a_failed_native_fork_still_creates_the_fork_with_portable_history() {
    let mut s = state();
    let (run, a) = running(&mut s, "source");
    finish(&mut s, &a);
    command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: run,
            title: None,
        },
    );
    recover(&mut s);
    assert!(!s.pending_forks.is_empty());
    let failed = result(
        &mut s,
        "fork-failed",
        EffectResult::ForkFailed {
            command: CommandId::new("fork").unwrap(),
            message: "transcript unavailable".into(),
        },
    );
    assert!(s.pending_forks.is_empty());
    let child = accept_child(&failed);
    assert_eq!(child.transfers[0].native_fork, None);
    assert!(child.native_sessions.is_empty());
    assert!(render_history(&child.transfers[0].history).contains("source"));
    assert!(
        result(
            &mut s,
            "late-success",
            EffectResult::NativeForked {
                command: CommandId::new("fork").unwrap(),
                native_thread: "late".into(),
            },
        )
        .effects
        .is_empty()
    );
}
// T3 Orchestrator.ts merge-back admission.
#[test]
fn merge_back_requires_a_fork_of_the_target_and_a_finished_source() {
    let mut s = state();
    let (_, a) = running(&mut s, "plain");
    finish(&mut s, &a);
    assert_eq!(
        command(
            &mut s,
            "merge",
            Command::MergeBack {
                target: ThreadId::new("other").unwrap(),
                through_run: None,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "not-a-fork-of-target".into()
        }
    );
    let run = s.runs[0].id.clone();
    let fork = command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: run,
            title: None,
        },
    );
    let forked = result(
        &mut s,
        "forked",
        EffectResult::NativeForked {
            command: CommandId::new("fork").unwrap(),
            native_thread: "fork-native".into(),
        },
    );
    let _ = fork;
    let mut child = accept_child(&forked);
    let merge = |child: &mut State, key: &str, through_run| {
        command(
            child,
            key,
            Command::MergeBack {
                target: ThreadId::new("thread").unwrap(),
                through_run,
            },
        )
        .reply
    };
    assert_eq!(
        merge(&mut child, "empty", None),
        Reply::Rejected {
            reason: "no-stable-source-run".into()
        }
    );
    let (running_run, _) = running(&mut child, "unfinished");
    assert_eq!(
        merge(&mut child, "running", Some(running_run)),
        Reply::Rejected {
            reason: "merge-back-source-not-finished".into()
        }
    );
}
fn native_root(events: Vec<ProviderEvent>) -> ProviderEvent {
    ProviderEvent::NativeOutput {
        echoed_prompts: vec![],
        acknowledged_prompt: None,
        root: true,
        result: Some(NativeResult {
            origin: None,
            turn_count: 1,
        }),
        events,
    }
}
// T3 ClaudeAdapterV2.ts: a result after Stop finalizes the turn as interrupted.
#[test]
fn a_stopped_claude_turn_finishes_on_its_wrapped_result() {
    let mut s = state();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let (_, a) = running(&mut s, "first");
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    command(&mut s, "stop", Command::Stop);
    provider(
        &mut s,
        "result",
        &a,
        native_root(vec![
            ProviderEvent::TurnUsage(normalize_claude_turn_usage(
                "success",
                Some(&serde_json::json!({"input_tokens":4,"output_tokens":2})),
                RunStatus::Completed,
            )),
            ProviderEvent::TurnFinished {
                status: RunStatus::Completed,
                native_head: None,
            },
        ]),
    );
    assert_eq!(s.runs[0].status, RunStatus::Interrupted);
    assert_eq!(s.attempts[0].status, AttemptStatus::Interrupted);
    assert_eq!(
        s.attempts[0].turn_usage.as_ref().unwrap().status,
        UsageStatus::Partial
    );
    assert!(s.runs[1].queue_held);
    assert!(s.items.iter().any(
        |item| matches!(item.kind, ItemKind::RunInterruptResult { .. })
            && item.status == ItemStatus::Interrupted
    ));
}
fn fail_with(s: &mut State, attempt: &RunAttemptId, key: &str, class: &str) {
    provider(
        s,
        &format!("{key}-error"),
        attempt,
        ProviderEvent::ItemFinished {
            key: format!("{key}-error"),
            kind: ProviderItem::Error {
                message: class.into(),
                retrying: false,
                code: None,
                class: Some(class.into()),
                retryable: None,
            },
            text: None,
            status: ItemStatus::Failed,
        },
    );
    provider(
        s,
        &format!("{key}-failed"),
        attempt,
        ProviderEvent::TurnFinished {
            status: RunStatus::Failed,
            native_head: None,
        },
    );
}
// T3 runtimeLayer.test.ts "handles a queued message after a %s failure".
#[test]
fn provider_failures_hold_the_queue_and_usage_limits_block_it() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    fail_with(&mut s, &a, "first", "provider_error");
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    assert!(s.runs[1].queue_held);
    command(&mut s, "resume", Command::ResumeQueue);
    assert_eq!(s.runs[1].status, RunStatus::Starting);

    let mut s = state();
    let (_, a) = running(&mut s, "first");
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    fail_with(&mut s, &a, "first", "usage_limit");
    assert_eq!(s.runs[1].status, RunStatus::Queued);
    assert!(!s.runs[1].queue_held);
    assert_eq!(
        command(&mut s, "resume", Command::ResumeQueue).reply,
        Reply::Rejected {
            reason: "usage-limited".into()
        }
    );
    command(
        &mut s,
        "new",
        send_message("new", DispatchMode::StartImmediately),
    );
    assert_eq!(s.runs[2].status, RunStatus::Starting);
    assert_eq!(s.runs[1].status, RunStatus::Queued);
}
fn delegate(s: &mut State, key: &str) -> NodeId {
    let task = NodeId::new(key).unwrap();
    command(
        s,
        key,
        Command::Delegate {
            task: task.clone(),
            child: ThreadId::new(format!("child-{key}")).unwrap(),
            prompt: "task prompt".into(),
            selection: selection(),
            wake: CompletionWake::Always,
        },
    );
    task
}
fn complete_task(s: &mut State, key: &str, task: &NodeId) -> Step {
    let source_message = s
        .tasks
        .iter()
        .find(|candidate| &candidate.id == task)
        .and_then(|task| task.original_message.clone());
    command(
        s,
        key,
        Command::TaskResult {
            generation: None,
            source_message,
            context: None,
            task: task.clone(),
            status: ItemStatus::Completed,
            result: "done".into(),
        },
    )
}
// T3 Orchestrator.ts archive: queued work and completion delivery are cancelled, sessions detach.
#[test]
fn archive_cancels_queued_work_and_detaches_without_unarchive_resuming() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let task = delegate(&mut s, "task");
    command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    );
    let archive = command(&mut s, "archive", Command::Archive { archived: true });
    assert_eq!(s.runs[1].status, RunStatus::Cancelled);
    assert_eq!(s.runs[0].status, RunStatus::Running);
    assert_eq!(s.tasks[0].delivery, DeliveryState::Disposed);
    assert!(archive.effects.iter().any(|effect| effect.body
        == EffectBody::DetachSessions {
            reason: "Thread archived.".into(),
            revoke_credentials: true
        }));
    assert!(
        archive
            .effects
            .iter()
            .any(|effect| effect.body == EffectBody::CleanupTerminals)
    );
    assert_eq!(
        command(&mut s, "again", Command::Archive { archived: true }).reply,
        Reply::Rejected {
            reason: "thread-already-archived".into()
        }
    );
    finish(&mut s, &a);
    complete_task(&mut s, "task-done", &task);
    let unarchive = command(&mut s, "unarchive", Command::Archive { archived: false });
    assert!(unarchive.effects.is_empty());
    assert_eq!(s.runs.len(), 2);
}
// T3 runtimeLayer.test.ts settle cases.
#[test]
fn settle_rejects_blocked_work_and_cancels_automatic_deliveries() {
    let mut s = state();
    let (_, a) = starting(&mut s, "first");
    let settle = |s: &mut State, key: &str| {
        command(
            s,
            key,
            Command::Settle {
                settled: true,
                at: None,
            },
        )
    };
    assert_eq!(
        settle(&mut s, "active").reply,
        Reply::Rejected {
            reason: "thread-has-active-work".into()
        }
    );
    provider(
        &mut s,
        "question",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "async".into(),
            body: RequestBody::Questions {
                questions: vec![Question {
                    required: true,
                    id: "q".into(),
                    header: "Question".into(),
                    question: "Which?".into(),
                    multiple: false,
                    options: vec![],
                }],
            },
            capability: ResponseCapability::Message,
        },
    );
    let task = delegate(&mut s, "task");
    complete_task(&mut s, "task-done", &task);
    command(
        &mut s,
        "wake",
        Command::AcceptTaskWake {
            task_ids: vec![task],
        },
    );
    let wake = s.runs.last().unwrap().id.clone();
    recover(&mut s);
    let held = s.runs.iter().find(|run| run.id == wake).unwrap();
    assert!(held.status == RunStatus::Queued && held.queue_held);
    assert_eq!(s.requests[0].status, RequestStatus::Pending);
    let mut user = s.clone();
    command(
        &mut user,
        "user-queued",
        send_message("user", DispatchMode::QueueAfterActive),
    );
    assert_eq!(
        settle(&mut user, "blocked").reply,
        Reply::Rejected {
            reason: "thread-has-active-work".into()
        }
    );
    let settled = settle(&mut s, "settle");
    assert_eq!(settled.reply, Reply::Accepted);
    assert_eq!(
        s.runs.iter().find(|run| run.id == wake).unwrap().status,
        RunStatus::Cancelled
    );
    let request = &s.requests[0];
    assert_eq!(request.status, RequestStatus::Resolved);
    assert_eq!(request.decision, Some(ApprovalDecision::Cancel));
    assert!(s.items.iter().any(
        |item| matches!(item.kind, ItemKind::UserInputRequest { .. })
            && item.status == ItemStatus::Cancelled
    ));
    assert!(settled.effects.iter().any(|effect| effect.body
        == EffectBody::DetachSessions {
            reason: "Thread settled.".into(),
            revoke_credentials: false
        }));
    assert_eq!(s.thread.as_ref().unwrap().settled, Some(true));
}
// T3 Orchestrator.ts dispatchMessage: a message re-engages a settled or snoozed thread.
#[test]
fn sending_a_message_clears_settled_and_snoozed_state() {
    let mut s = state();
    command(
        &mut s,
        "settle",
        Command::Settle {
            settled: true,
            at: None,
        },
    );
    command(
        &mut s,
        "unsettle-snooze",
        Command::Settle {
            settled: false,
            at: None,
        },
    );
    command(
        &mut s,
        "snooze",
        Command::Snooze {
            until: Some(Timestamp::parse("2026-10-06T00:00:00Z").unwrap()),
        },
    );
    command(
        &mut s,
        "message",
        send_message("message", DispatchMode::StartImmediately),
    );
    let thread = s.thread.as_ref().unwrap();
    assert_eq!(thread.settled, None);
    assert_eq!(thread.settled_at, None);
    assert_eq!(thread.snoozed_until, None);
}
// T3 SteeringCompletion.integration.test.ts:563 and :638.
#[test]
fn dispatch_saves_the_requested_selection_and_late_steers_use_it() {
    let mut s = state();
    let mut other = selection();
    other.model = "gpt-6-sol".into();
    let (run, a) = running(&mut s, "first");
    let mut steer = send_message("steer", DispatchMode::SteerActive { run: run.clone() });
    if let Command::Send(message) = &mut steer {
        message.selection = Some(other.clone());
    }
    command(&mut s, "steer", steer);
    assert_eq!(s.runs[0].selection, selection());
    assert_eq!(s.thread.as_ref().unwrap().selection, other);
    finish(&mut s, &a);
    result(
        &mut s,
        "missed",
        EffectResult::ProviderFailed {
            attempt: a,
            operation: ProviderOperation::Steer,
            message: "turn already completed".into(),
            message_id: Some(MessageId::new("steer").unwrap()),
            turn_completed: true,
            session_lost: false,
        },
    );
    assert_eq!(s.runs[1].selection, other);
    assert_eq!(s.runs[1].status, RunStatus::Starting);
    let item = s
        .items
        .iter()
        .find(|item| matches!(&item.kind, ItemKind::UserMessage { message } if message.as_str() == "steer"))
        .unwrap();
    assert_eq!(item.run.as_ref(), Some(&s.runs[1].id));
    assert_eq!(
        s.messages
            .iter()
            .filter(|m| m.id.as_str() == "steer")
            .count(),
        1
    );
    rollback_free_check(&s);

    let mut s = state();
    let mut start = send_message("start", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut start {
        message.selection = Some(other.clone());
    }
    command(&mut s, "start", start);
    assert_eq!(s.thread.as_ref().unwrap().selection, other);
    let mut queued = send_message("queued", DispatchMode::QueueAfterActive);
    if let Command::Send(message) = &mut queued {
        message.selection = Some(selection());
    }
    command(&mut s, "queued", queued);
    assert_eq!(s.thread.as_ref().unwrap().selection, other);
    let a = s.runs[0].attempt.clone().unwrap();
    provider(
        &mut s,
        "started",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &a);
    assert_eq!(s.thread.as_ref().unwrap().selection, selection());
}
fn rollback_free_check(s: &State) {
    assert!(s.visible_items().iter().all(|item| {
        item.run
            .as_ref()
            .is_none_or(|run| s.runs.iter().any(|r| &r.id == run))
    }));
}
// T3 Orchestrator.ts:4609: a proposed plan is consumed when its implementation is accepted.
#[test]
fn a_proposed_plan_is_consumed_once_at_acceptance() {
    let mut s = state();
    let (_, a) = running(&mut s, "plan");
    provider(
        &mut s,
        "plan",
        &a,
        ProviderEvent::Plan {
            kind: PlanKind::Proposed,
            key: "proposal".into(),
            markdown: "Do it".into(),
            steps: vec![],
        },
    );
    provider(
        &mut s,
        "todo",
        &a,
        ProviderEvent::Plan {
            kind: PlanKind::Todo,
            key: "todo".into(),
            markdown: String::new(),
            steps: vec![],
        },
    );
    let proposal = s.plans[0].id.clone();
    let todo = s.plans[1].id.clone();
    let implement = |key: &str, plan: &PlanId| {
        let mut send = send_message(key, DispatchMode::QueueAfterActive);
        if let Command::Send(message) = &mut send {
            message.source_plan = Some(plan.clone());
        }
        send
    };
    assert_eq!(
        command(&mut s, "todo-plan", implement("todo-plan", &todo)).reply,
        Reply::Rejected {
            reason: "plan-not-found".into()
        }
    );
    command(&mut s, "first", implement("first", &proposal));
    assert!(s.plans[0].implemented_by.is_some());
    assert_eq!(
        command(&mut s, "second", implement("second", &proposal)).reply,
        Reply::Rejected {
            reason: "plan-not-active".into()
        }
    );
}
// T3 ThreadDeletion.test.ts: deletion cancels requests and queues session, terminal and attachment cleanup.
#[test]
fn deletion_cancels_pending_requests_and_releases_thread_resources() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    provider(
        &mut s,
        "approval",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "1".into(),
            body: RequestBody::Approval {
                kind: "command".into(),
                title: "ls".into(),
                detail: None,
                options: vec![],
                input: Json(serde_json::json!({})),
            },
            capability: ResponseCapability::Live,
        },
    );
    let task = delegate(&mut s, "task");
    let deleted = command(&mut s, "delete", Command::Delete);
    assert_eq!(s.requests[0].status, RequestStatus::Cancelled);
    assert_eq!(s.requests[0].capability, ResponseCapability::NotResumable);
    assert_eq!(
        s.tasks.iter().find(|t| t.id == task).unwrap().delivery,
        DeliveryState::Disposed
    );
    let kinds = deleted
        .effects
        .iter()
        .filter_map(|effect| match &effect.body {
            EffectBody::DetachSessions {
                reason,
                revoke_credentials: true,
            } => Some(reason.as_str()),
            EffectBody::CleanupTerminals => Some("terminals"),
            EffectBody::DeleteAttachments { .. } => Some("attachments"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["Thread deleted.", "terminals", "attachments"]);
}
// T3 ProviderEventIngestor.test.ts: only native questions are dismissed when their turn ends.
#[test]
fn message_capable_questions_stay_answerable_after_their_turn_ends() {
    for status in [
        RunStatus::Completed,
        RunStatus::Interrupted,
        RunStatus::Failed,
    ] {
        let mut s = state();
        let (_, a) = running(&mut s, "first");
        for (key, capability) in [
            ("message", ResponseCapability::Message),
            ("live", ResponseCapability::Live),
        ] {
            provider(
                &mut s,
                key,
                &a,
                ProviderEvent::RequestOpened {
                    owner_path: vec![],
                    key: key.into(),
                    body: RequestBody::Questions {
                        questions: vec![Question {
                            required: true,
                            id: "q".into(),
                            header: "Question".into(),
                            question: "Which?".into(),
                            multiple: false,
                            options: vec![],
                        }],
                    },
                    capability,
                },
            );
        }
        provider(
            &mut s,
            "end",
            &a,
            ProviderEvent::TurnFinished {
                status,
                native_head: None,
            },
        );
        assert_eq!(
            s.requests.iter().map(|r| r.status).collect::<Vec<_>>(),
            [RequestStatus::Pending, RequestStatus::Cancelled],
            "{status:?}"
        );
        let request = s.requests[0].id.clone();
        let answer = command(
            &mut s,
            "answer",
            Command::Respond {
                request,
                decision: None,
                answers: Some(Answers::from([("q".into(), Answer::Text("Blue".into()))])),
                attachments: BTreeMap::new(),
            },
        );
        assert!(matches!(answer.reply, Reply::Run(_)));
        assert_eq!(s.messages.last().unwrap().text, "Which?\nBlue");
    }
}
fn delegate_with(s: &mut State, key: &str, wake: CompletionWake) -> NodeId {
    let task = NodeId::new(key).unwrap();
    command(
        s,
        key,
        Command::Delegate {
            task: task.clone(),
            child: ThreadId::new(format!("child-{key}")).unwrap(),
            prompt: format!("prompt {key}"),
            selection: selection(),
            wake,
        },
    );
    task
}
// T3 Orchestrator.ts:4570: an always-wake completion steers into a running parent turn.
#[test]
fn always_completions_steer_into_the_running_parent_and_are_delivered_with_it() {
    let mut s = state();
    let (run, a) = running(&mut s, "parent");
    let task = delegate_with(&mut s, "task", CompletionWake::Always);
    let done = complete_task(&mut s, "done", &task);
    let Some(EffectBody::SendToThread { command: wake, .. }) =
        done.effects.iter().map(|e| &e.body).find(|body| {
            matches!(body, EffectBody::SendToThread { command, .. } if matches!(command.as_ref(), Command::AcceptTaskWake { .. }))
        })
    else {
        panic!("{:?}", done.effects)
    };
    let steered = command(&mut s, "wake", *wake.clone());
    assert_eq!(steered.reply, Reply::Run(run.clone()));
    assert!(steered.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::Provider(ProviderCommand::Steer { text, .. }) if text.starts_with("Delegated task task reached")
    )));
    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.tasks[0].delivery, DeliveryState::Claimed);
    let message = s.messages.last().unwrap();
    assert_eq!(message.intent, InputIntent::Steer);
    assert!(message.notification.is_some());
    finish(&mut s, &a);
    assert_eq!(s.tasks[0].delivery, DeliveryState::Delivered);
    assert_eq!(s.runs.len(), 1);
}
// T3 DelegatedCompletionDelivery.test.ts:1211: settled-only waits only for its spawning run.
#[test]
fn settled_only_completion_waits_only_for_its_spawning_run() {
    let mut s = state();
    let (_, a) = running(&mut s, "spawning");
    let task = delegate_with(&mut s, "task", CompletionWake::SettledOnly);
    let early = complete_task(&mut s, "early", &task);
    assert!(!early.effects.iter().any(|e| matches!(&e.body, EffectBody::SendToThread { command, .. } if matches!(command.as_ref(), Command::AcceptTaskWake { .. }))));
    let mut other = state();
    std::mem::swap(&mut other, &mut s);
    let mut s = other;
    command(
        &mut s,
        "queued",
        send_message("continuation", DispatchMode::QueueAfterActive),
    );
    let ended = provider(
        &mut s,
        "spawning-done",
        &a,
        ProviderEvent::TurnFinished {
            status: RunStatus::Completed,
            native_head: None,
        },
    );
    assert_eq!(s.runs[1].status, RunStatus::Starting);
    let wake = ended
        .effects
        .iter()
        .find_map(|e| match &e.body {
            EffectBody::SendToThread { command, .. }
                if matches!(command.as_ref(), Command::AcceptTaskWake { .. }) =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("the spawning run ended");
    command(&mut s, "wake", wake);
    assert_eq!(s.tasks[0].delivery, DeliveryState::Claimed);
    assert_eq!(s.runs.last().unwrap().status, RunStatus::Queued);
}
// T3 SubagentProjection.test.ts: the failure wins over progress messages.
#[test]
fn delegated_results_use_the_failure_or_latest_answer() {
    let mut s = state();
    let (run, a) = running(&mut s, "child task");
    for (key, text) in [("one", "Investigating…"), ("two", "Final answer")] {
        provider(
            &mut s,
            key,
            &a,
            ProviderEvent::ItemFinished {
                key: key.into(),
                kind: ProviderItem::Text,
                text: Some(text.into()),
                status: ItemStatus::Completed,
            },
        );
    }
    let record = s.runs[0].clone();
    let mut completed = record.clone();
    completed.status = RunStatus::Completed;
    assert_eq!(
        delegated_result(&completed, &s.items, &s.messages),
        "Final answer"
    );
    fail_with(&mut s, &a, "auth", "provider_error");
    let failed = s.runs.iter().find(|r| r.id == run).unwrap();
    assert_eq!(
        delegated_result(failed, &s.items, &s.messages),
        "provider_error"
    );
    let mut empty = state();
    let (_, b) = running(&mut empty, "quiet");
    finish(&mut empty, &b);
    assert_eq!(
        delegated_result(&empty.runs[0], &empty.items, &empty.messages),
        "Child task completed without an assistant result."
    );
    let mut stopped = empty.runs[0].clone();
    stopped.status = RunStatus::Interrupted;
    assert_eq!(
        delegated_result(&stopped, &[], &[]),
        "Child task ended with status interrupted."
    );
}
// T3 ClaudeAdapterV2.ts:4166 and :4315: a resumed native task reopens its card, and a
// result of the previous generation cannot complete it.
#[test]
fn a_resumed_native_task_reopens_its_card_and_rejects_stale_results() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let start = ProviderEvent::SubagentStarted {
        background: false,
        native_thread: None,
        key: "child".into(),
        parent: None,
        prompt: "Hello".into(),
        model: None,
    };
    provider(&mut s, "spawn", &a, start.clone());
    provider(
        &mut s,
        "done",
        &a,
        ProviderEvent::SubagentFinished {
            key: "child".into(),
            status: ItemStatus::Completed,
            result: "first".into(),
        },
    );
    finish(&mut s, &a);
    let (second, b) = running(&mut s, "second");
    let reopened = provider(&mut s, "reopen", &b, start);
    let card = s
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::Subagent { .. }))
        .unwrap();
    assert_eq!(card.status, ItemStatus::Running);
    assert_eq!(card.run.as_ref(), Some(&second));
    assert_eq!(card.completed_at, None);
    assert!(reopened.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::SendToThread { command, .. }
            if matches!(command.as_ref(), Command::BindNativeChild { generation: 1, .. })
    )));
    let task = s.tasks[0].id.clone();
    let stale = command(
        &mut s,
        "stale",
        Command::TaskResult {
            generation: Some(0),
            source_message: None,
            context: None,
            task: task.clone(),
            status: ItemStatus::Completed,
            result: "old".into(),
        },
    );
    assert_eq!(stale.reply, Reply::Ignored);
    assert_eq!(s.tasks[0].status, ItemStatus::Running);
    command(
        &mut s,
        "current",
        Command::TaskResult {
            generation: Some(1),
            source_message: None,
            context: None,
            task,
            status: ItemStatus::Completed,
            result: "second".into(),
        },
    );
    assert_eq!(s.tasks[0].status, ItemStatus::Completed);
    assert_eq!(s.tasks[0].result.as_deref(), Some("second"));
}
// T3 Orchestrator.ts:7402, :7228 and :7077.
#[test]
fn queued_edits_are_validated_and_automatic_deliveries_are_fixed() {
    let mut s = state();
    let (_, a) = starting(&mut s, "parent");
    command(
        &mut s,
        "user",
        send_message("user", DispatchMode::QueueAfterActive),
    );
    let user = s.runs[1].id.clone();
    let edit = |s: &mut State, key: &str, run: &RunId, text: &str| {
        command(
            s,
            key,
            Command::EditQueued {
                run: run.clone(),
                text: text.into(),
                attachments: None,
            },
        )
        .reply
    };
    assert_eq!(
        edit(&mut s, "blank", &user, "   "),
        Reply::Rejected {
            reason: "empty-message".into()
        }
    );
    let task = delegate_with(&mut s, "task", CompletionWake::Always);
    complete_task(&mut s, "done", &task);
    command(
        &mut s,
        "wake",
        Command::AcceptTaskWake {
            task_ids: vec![task],
        },
    );
    let wake = s.runs.last().unwrap().id.clone();
    assert_eq!(
        edit(&mut s, "edit-wake", &wake, "changed"),
        Reply::Rejected {
            reason: "automatic-delivery-not-editable".into()
        }
    );
    for (key, run, before, reason) in [
        (
            "move-wake",
            &wake,
            None,
            "automatic-delivery-not-reorderable",
        ),
        (
            "ahead",
            &user,
            Some(wake.clone()),
            "cannot-reorder-ahead-of-automatic-delivery",
        ),
    ] {
        assert_eq!(
            command(
                &mut s,
                key,
                Command::ReorderQueued {
                    run: run.clone(),
                    before,
                },
            )
            .reply,
            Reply::Rejected {
                reason: reason.into()
            }
        );
    }
    provider(
        &mut s,
        "accepted",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    let active = s.runs[0].id.clone();
    assert_eq!(
        command(
            &mut s,
            "promote",
            Command::PromoteToSteer {
                queued: wake,
                active,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "automatic-delivery-not-promotable".into()
        }
    );
}
// T3 Orchestrator.ts:6866 and :7022: cancel needs no answers and closes the card as cancelled.
#[test]
fn declined_requests_and_dismissed_questions_close_their_cards_as_cancelled() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let question = |capability, key: &str| ProviderEvent::RequestOpened {
        owner_path: vec![],
        key: key.into(),
        body: RequestBody::Questions {
            questions: vec![Question {
                required: true,
                id: "q".into(),
                header: "Question".into(),
                question: "Which?".into(),
                multiple: false,
                options: vec![],
            }],
        },
        capability,
    };
    provider(
        &mut s,
        "live",
        &a,
        question(ResponseCapability::Live, "live"),
    );
    provider(
        &mut s,
        "message",
        &a,
        question(ResponseCapability::Message, "message"),
    );
    provider(
        &mut s,
        "approval",
        &a,
        ProviderEvent::RequestOpened {
            owner_path: vec![],
            key: "approval".into(),
            body: RequestBody::Approval {
                kind: "command".into(),
                title: "rm".into(),
                detail: None,
                options: vec![],
                input: Json(serde_json::json!({})),
            },
            capability: ResponseCapability::Live,
        },
    );
    let ids = s.requests.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let cancel = command(
        &mut s,
        "cancel-live",
        Command::Respond {
            request: ids[0].clone(),
            decision: Some(ApprovalDecision::Cancel),
            answers: None,
            attachments: BTreeMap::new(),
        },
    );
    assert!(cancel.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::Provider(ProviderCommand::Respond {
            decision: Some(ApprovalDecision::Cancel),
            answers: None,
            ..
        })
    )));
    command(
        &mut s,
        "decline",
        Command::Respond {
            request: ids[2].clone(),
            decision: Some(ApprovalDecision::Decline),
            answers: None,
            attachments: BTreeMap::new(),
        },
    );
    assert_eq!(
        command(
            &mut s,
            "dismiss-live",
            Command::DismissQuestion {
                request: ids[0].clone()
            },
        )
        .reply,
        Reply::Rejected {
            reason: "question-already-answered".into()
        }
    );
    command(
        &mut s,
        "dismiss",
        Command::DismissQuestion {
            request: ids[1].clone(),
        },
    );
    assert!(
        s.requests
            .iter()
            .all(|r| r.status == RequestStatus::Resolved)
    );
    assert_eq!(s.requests[1].decision, Some(ApprovalDecision::Cancel));
    let cards = s
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ItemKind::UserInputRequest { .. } | ItemKind::ApprovalRequest { .. }
            )
        })
        .map(|item| item.status)
        .collect::<Vec<_>>();
    assert_eq!(cards, [ItemStatus::Cancelled; 3]);
    assert_eq!(s.messages.len(), 1);
}
// T3 orchestrationV2.ts:2878 and SubagentProjection.ts:28: a delegation needs a task.
#[test]
fn delegation_requires_a_task_and_titles_the_child_from_it() {
    let mut s = state();
    running(&mut s, "parent");
    let delegate = |s: &mut State, key: &str, prompt: &str| {
        command(
            s,
            key,
            Command::Delegate {
                task: NodeId::new(key).unwrap(),
                child: ThreadId::new(format!("child-{key}")).unwrap(),
                prompt: prompt.into(),
                selection: selection(),
                wake: CompletionWake::SettledOnly,
            },
        )
    };
    assert_eq!(
        delegate(&mut s, "blank", "  \n ").reply,
        Reply::Rejected {
            reason: "task-required".into()
        }
    );
    assert!(s.tasks.is_empty());
    let long = "x".repeat(80);
    let step = delegate(&mut s, "long", &format!("  {long}  "));
    let Some(Command::AcceptDelegation { title, message, .. }) =
        step.effects.iter().find_map(|e| match &e.body {
            EffectBody::SendToThread { command, .. } => Some(command.as_ref()),
            _ => None,
        })
    else {
        panic!()
    };
    assert_eq!(title, &format!("{}...", "x".repeat(69)));
    assert_eq!(message.text, long);
}
// The streaming shortcut applies the same ownership checks as ordinary output.
#[test]
fn stale_streaming_output_cannot_reach_a_newer_attempt() {
    let mut s = state();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let (_, a) = running(&mut s, "first");
    command(&mut s, "stop", Command::Stop);
    provider(
        &mut s,
        "stopped",
        &a,
        ProviderEvent::TurnFinished {
            status: RunStatus::Interrupted,
            native_head: None,
        },
    );
    let (_, b) = running(&mut s, "second");
    provider(
        &mut s,
        "open",
        &b,
        ProviderEvent::ItemStarted {
            key: "block".into(),
            kind: ProviderItem::Text,
        },
    );
    let before = s.clone();
    let stale = provider(
        &mut s,
        "stale",
        &a,
        ProviderEvent::NativeOutput {
            echoed_prompts: vec![],
            acknowledged_prompt: None,
            root: true,
            result: None,
            events: vec![ProviderEvent::TextDelta {
                key: "block".into(),
                kind: ProviderItem::Text,
                text: "stale".into(),
            }],
        },
    );
    assert_eq!(stale.reply, Reply::Ignored);
    assert_eq!(s, before);
}
fn title_effect(step: &Step) -> Option<(CommandId, Option<MessageId>)> {
    step.effects.iter().find_map(|effect| match &effect.body {
        EffectBody::GenerateTitle { request, message } => Some((request.clone(), message.clone())),
        _ => None,
    })
}
// T3 ThreadLaunchService.test.ts and ThreadTitleRegenerationService.test.ts.
#[test]
fn titles_are_generated_once_and_a_rename_supersedes_the_request() {
    let mut s = state();
    let mut first = send_message("first", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut first {
        message.title_seed = Some("Generate my title".into());
    }
    let step = command(&mut s, "first", first);
    let (request, message) = title_effect(&step).unwrap();
    assert_eq!(message.unwrap().as_str(), "first");
    assert_eq!(s.thread.as_ref().unwrap().title, "Generate my title");
    command(
        &mut s,
        "rename",
        Command::Rename {
            title: "Keep my title".into(),
        },
    );
    let before = s.clone();
    let stale = result(
        &mut s,
        "stale",
        EffectResult::TitleGenerated {
            request: request.clone(),
            title: Some("Stale generated title".into()),
        },
    );
    assert_eq!(stale.reply, Reply::Ignored);
    assert_eq!(s, before);
    let regenerate = command(&mut s, "regenerate", Command::RegenerateTitle);
    let (request, message) = title_effect(&regenerate).unwrap();
    assert_eq!(message, None);
    result(
        &mut s,
        "fallback",
        EffectResult::TitleGenerated {
            request,
            title: Some("New thread".into()),
        },
    );
    assert_eq!(s.thread.as_ref().unwrap().title, "Keep my title");
    assert_eq!(s.thread.as_ref().unwrap().title_request, None);
    let regenerate = command(&mut s, "again", Command::RegenerateTitle);
    let (request, _) = title_effect(&regenerate).unwrap();
    result(
        &mut s,
        "fresh",
        EffectResult::TitleGenerated {
            request,
            title: Some("Fresh title".into()),
        },
    );
    assert_eq!(s.thread.as_ref().unwrap().title, "Fresh title");
    let later = command(
        &mut s,
        "later",
        send_message("later", DispatchMode::QueueAfterActive),
    );
    assert_eq!(title_effect(&later), None);

    let mut s = state();
    let mut compact = send_message("compact", DispatchMode::StartImmediately);
    if let Command::Send(message) = &mut compact {
        message.text = "/compact".into();
    }
    assert_eq!(title_effect(&command(&mut s, "compact", compact)), None);
    let a = s.runs[0].attempt.clone().unwrap();
    provider(
        &mut s,
        "started",
        &a,
        ProviderEvent::TurnStarted { native_turn: None },
    );
    finish(&mut s, &a);
    let step = command(
        &mut s,
        "real",
        send_message("real", DispatchMode::StartImmediately),
    );
    assert!(title_effect(&step).is_some());
    command(&mut s, "archive", Command::Archive { archived: true });
    assert_eq!(s.thread.as_ref().unwrap().title_request, None);
}
fn import(thread: &str) -> Command {
    Command::Import {
        thread: ThreadId::new(thread).unwrap(),
        project: "project".into(),
        title: "  ".into(),
        selection: selection(),
        workspace: Some(Workspace {
            cwd: "/repo".into(),
            worktree_path: None,
            branch: Some("main".into()),
        }),
        created_at: Timestamp::parse("2026-01-01T00:00:00Z").unwrap(),
        updated_at: Timestamp::parse("2026-01-02T00:00:00Z").unwrap(),
        messages: vec![
            ImportedMessage {
                role: Role::User,
                text: "Fix it".into(),
                at: Timestamp::parse("2026-01-01T00:01:00Z").unwrap(),
            },
            ImportedMessage {
                role: Role::Assistant,
                text: "Fixed".into(),
                at: Timestamp::parse("2026-01-01T00:02:00Z").unwrap(),
            },
        ],
        native: NativeBinding {
            instance: "codex".into(),
            thread: "native-codex-thread".into(),
            head: None,
        },
    }
}
// T3 AgentSessionImporter.test.ts.
#[test]
fn imported_sessions_keep_message_times_and_resume_their_native_session() {
    let mut s = State::default();
    command(&mut s, "import", import("import:codex:session"));
    let thread = s.thread.clone().unwrap();
    assert_eq!(thread.title, "Untitled thread");
    assert_eq!(thread.settled, Some(true));
    assert_eq!(thread.created_at.as_str(), "2026-01-01T00:00:00.000Z");
    assert_eq!(thread.updated_at.as_str(), "2026-01-02T00:00:00.000Z");
    assert_eq!(thread.workspace.unwrap().branch.as_deref(), Some("main"));
    assert_eq!(
        s.messages
            .iter()
            .map(|m| (m.text.as_str(), m.created_at.as_str(), m.streaming))
            .collect::<Vec<_>>(),
        [
            ("Fix it", "2026-01-01T00:01:00.000Z", false),
            ("Fixed", "2026-01-01T00:02:00.000Z", false)
        ]
    );
    assert_eq!(
        s.messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["import:codex:session:000000", "import:codex:session:000001"]
    );
    assert_eq!(s.native_sessions["codex"], "native-codex-thread");
    let before = s.clone();
    assert_eq!(
        command(&mut s, "again", import("import:codex:session")).reply,
        Reply::Ignored
    );
    assert_eq!(s, before);
    let step = command(
        &mut s,
        "resume",
        send_message("next", DispatchMode::StartImmediately),
    );
    assert!(step.effects.iter().any(|effect| matches!(
        &effect.body,
        EffectBody::Provider(ProviderCommand::Start { native_thread: Some(native), context: None, .. }) if native == "native-codex-thread"
    )));
    let mut other = State::default();
    command(&mut other, "import", import("import:codex:other"));
    command(
        &mut other,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let step = command(
        &mut other,
        "elsewhere",
        send_message("elsewhere", DispatchMode::StartImmediately),
    );
    let (_, history) = start_context(&step).unwrap();
    assert!(history.contains("Fix it") && history.contains("Fixed"));
    let mut active = state();
    assert_eq!(
        command(&mut active, "import", import("thread")).reply,
        Reply::Rejected {
            reason: "thread-has-activity".into()
        }
    );
}
#[test]
fn workspace_bindings_are_recorded_and_inherited_by_forks() {
    let mut s = state();
    let workspace = Workspace {
        cwd: "/repo/.worktrees/one".into(),
        worktree_path: Some("/repo/.worktrees/one".into()),
        branch: Some("feature".into()),
    };
    let step = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "workspace".into(),
            input: Input::Workspace {
                workspace: Some(workspace.clone()),
            },
        },
    );
    s = fold(&s, &step.facts).unwrap();
    assert_eq!(
        s.thread.as_ref().unwrap().workspace,
        Some(workspace.clone())
    );
    let (run, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let fork = command(
        &mut s,
        "fork",
        Command::Fork {
            target: ThreadId::new("fork").unwrap(),
            through_run: run,
            title: None,
        },
    );
    let forked = result(
        &mut s,
        "forked",
        EffectResult::NativeForked {
            command: CommandId::new("fork").unwrap(),
            native_thread: "fork-native".into(),
        },
    );
    let _ = fork;
    let child = accept_child(&forked);
    assert_eq!(child.thread.unwrap().workspace, Some(workspace));
}
#[test]
fn large_text_is_split_across_facts_without_truncation() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    let huge = "界".repeat(MAX_FACT_TEXT);
    let step = provider(
        &mut s,
        "huge",
        &a,
        ProviderEvent::ItemFinished {
            key: "output".into(),
            kind: ProviderItem::Text,
            text: Some(huge.clone()),
            status: ItemStatus::Completed,
        },
    );
    assert!(
        step.facts
            .iter()
            .all(|fact| serde_json::to_vec(fact).unwrap().len() < MAX_FACT_TEXT + 4096)
    );
    assert!(step.facts.len() > 3);
    let item = s
        .items
        .iter()
        .find(|item| item.native_key == "output")
        .unwrap();
    assert_eq!(item.text, huge);
    let delta = provider(
        &mut s,
        "delta",
        &a,
        ProviderEvent::TextDelta {
            key: "stream".into(),
            kind: ProviderItem::Text,
            text: huge.clone(),
        },
    );
    assert!(delta.facts.len() > 3);
    let error = provider(
        &mut s,
        "error",
        &a,
        ProviderEvent::ItemFinished {
            key: "error".into(),
            kind: ProviderItem::Error {
                message: "x".repeat(5000),
                retrying: false,
                code: Some("c".repeat(200)),
                class: None,
                retryable: None,
            },
            text: None,
            status: ItemStatus::Failed,
        },
    );
    let _ = error;
    let ItemKind::Error { message, code, .. } = &s.items.last().unwrap().kind else {
        panic!()
    };
    assert_eq!(message.encode_utf16().count(), 4096);
    assert!(message.ends_with('…'));
    assert_eq!(code.as_ref().unwrap().encode_utf16().count(), 128);
}
#[test]
fn wire_encodings_round_trip_imports_titles_rollbacks_and_workspaces() {
    let mut s = State::default();
    let imported = import("import:codex:wire");
    round_trip(&imported);
    let step = command(&mut s, "import", imported);
    round_trip(&step);
    let regenerate = command(&mut s, "regenerate", Command::RegenerateTitle);
    round_trip(&regenerate);
    round_trip(&EffectResult::TitleGenerated {
        request: CommandId::new("regenerate").unwrap(),
        title: None,
    });
    round_trip(&Input::Workspace {
        workspace: s.thread.as_ref().unwrap().workspace.clone(),
    });
    round_trip(&EffectBody::Rollback {
        command: CommandId::new("rollback").unwrap(),
        providers: vec![ProviderRollback {
            instance: "codex".into(),
            command: ProviderCommand::Rollback {
                native_thread: "native".into(),
                absolute_head: Some("turn".into()),
            },
        }],
        restore: Some(RestoreFiles {
            scope: None,
            checkpoint: CheckpointId::new("cp").unwrap(),
            file_ref: "ref".into(),
        }),
        stale_file_refs: vec!["later".into()],
    });
    round_trip(&s);
    assert_eq!(STATE_FORMAT, 1);
}
