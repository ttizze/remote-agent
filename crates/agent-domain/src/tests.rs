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
                event,
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
#[test]
fn restart_supersedes_attempt_and_interrupts_native_children() {
    let mut s = state();
    let (run, a) = running(&mut s, "first");
    provider(
        &mut s,
        "child",
        &a,
        ProviderEvent::SubagentStarted {
            key: "child".into(),
            parent: None,
            prompt: "child prompt".into(),
            model: None,
        },
    );
    command(
        &mut s,
        "restart",
        send_message("restart", DispatchMode::RestartActive { run }),
    );
    assert_eq!(s.attempts[0].status, AttemptStatus::Superseded);
    assert_eq!(s.tasks[0].status, ItemStatus::Interrupted);
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
        EffectBody::Provider(ProviderCommand::Interrupt)
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
    result(
        s,
        key,
        EffectResult::CheckpointCaptured {
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
    let EffectBody::Provider(ProviderCommand::Start { context, .. }) = &start.effects[0].body
    else {
        panic!()
    };
    assert!(context.contains("fork marker"));
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
    assert!(parent.transfers[0].text.contains("child-marker"));
    assert!(!parent.transfers[0].text.contains("parent-marker"));
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
    assert!(s.transfers[0].consumed_by.is_none());
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
            key: "proposal".into(),
            markdown: "replay fixture plan".into(),
            steps: vec![],
        },
    );
    finish(&mut s, &a);
    let plan = s.plans[0].id.clone();
    let file = Attachment {
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
            key: "question".into(),
            body: RequestBody::Questions {
                questions: vec![Question {
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
                vec!["One".into(), "Two".into()],
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
    let (_, a) = running(&mut s, "first");
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
    command(
        &mut s,
        "task-result",
        Command::TaskResult {
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
            let envelope=InputEnvelope { at:at(),key:index.to_string(),input:Input::Provider { attempt:a.clone(),event:ProviderEvent::TextDelta { key:"text".into(),kind:ProviderItem::Text,text } } };
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
    running(&mut s, "parent");
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
    command(
        &mut s,
        "result",
        Command::TaskResult {
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
