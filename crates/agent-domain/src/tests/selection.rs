use super::*;

fn detached(step: &Step) -> Vec<(Option<String>, String)> {
    step.effects
        .iter()
        .filter_map(|effect| match &effect.body {
            EffectBody::DetachSessions {
                instance, reason, ..
            } => Some((instance.clone(), reason.clone())),
            _ => None,
        })
        .collect()
}
fn provider_commands(step: &Step) -> usize {
    step.effects
        .iter()
        .filter(|effect| matches!(effect.body, EffectBody::Provider(_)))
        .count()
}

// T3 ProviderSessionTransitionPolicy.ts / ProviderSwitchService.ts: a model
// change applies on the next turn, and moving to another instance releases
// the sessions of the instances it leaves, even during a run.
#[test]
fn selection_changes_apply_next_turn_and_release_left_instances() {
    let mut s = state();
    running(&mut s, "first");
    let mut model = selection();
    model.model = "gpt-6-sol".into();
    let step = command(&mut s, "model", Command::SelectModel { selection: model });
    assert_eq!(step.reply, Reply::Accepted);
    assert_eq!(provider_commands(&step), 0);
    assert!(detached(&step).is_empty());

    let mut claude = claude_selection();
    claude.instance = "claude".into();
    let step = command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: claude },
    );
    assert_eq!(step.reply, Reply::Accepted);
    assert_eq!(provider_commands(&step), 0);
    assert_eq!(
        detached(&step),
        [(
            Some("codex".to_owned()),
            "Provider or model selection changed.".to_owned()
        )]
    );
    assert_eq!(s.thread.as_ref().unwrap().selection.instance, "claude");
}

// T3 Orchestrator.ts thread.runtime-mode.set detaches sessions that cannot
// switch modes in place (Claude); Codex takes the mode on its next turn.
#[test]
fn runtime_mode_changes_detach_only_claude_sessions() {
    let mut s = state();
    let (_, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let step = command(
        &mut s,
        "codex-mode",
        Command::RuntimeMode {
            mode: RuntimeMode::ApprovalRequired,
        },
    );
    assert!(detached(&step).is_empty());
    assert_eq!(provider_commands(&step), 0);

    let mut claude = claude_selection();
    claude.instance = "claude".into();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider { selection: claude },
    );
    running(&mut s, "second");
    let step = command(
        &mut s,
        "claude-mode",
        Command::RuntimeMode {
            mode: RuntimeMode::FullAccess,
        },
    );
    assert_eq!(
        detached(&step),
        [(
            Some("claude".to_owned()),
            "Runtime mode changed.".to_owned()
        )]
    );
    assert_eq!(provider_commands(&step), 0);
    assert_eq!(
        s.thread.as_ref().unwrap().runtime_mode,
        RuntimeMode::FullAccess
    );
}

// T3 Orchestrator.ts dispatchProviderSessionDetach.
#[test]
fn a_client_can_detach_a_session_the_thread_owns() {
    let mut s = state();
    assert_eq!(
        command(
            &mut s,
            "unknown",
            Command::DetachProviderSession {
                instance: "codex".into(),
                reason: None,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "provider-session-not-found".into()
        }
    );
    running(&mut s, "first");
    let step = command(
        &mut s,
        "detach",
        Command::DetachProviderSession {
            instance: "codex".into(),
            reason: Some("client-requested".into()),
        },
    );
    assert_eq!(
        detached(&step),
        [(Some("codex".to_owned()), "client-requested".to_owned())]
    );
}

const HANDOFF_DETACH: &str = "Provider thread handoff replaced this session binding.";

fn steer_on(s: &mut State, key: &str, mode: DispatchMode, selection: ModelSelection) -> Step {
    let mut message = send_message(key, mode);
    if let Command::Send(message) = &mut message {
        message.selection = Some(selection);
    }
    command(s, key, message)
}
struct Started {
    attempt: Option<RunAttemptId>,
    selection: ModelSelection,
    text: String,
    native_thread: Option<String>,
    context: Option<HistoricalContext>,
}
fn starts(step: &Step) -> Vec<Started> {
    step.effects
        .iter()
        .filter_map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Start {
                selection,
                text,
                native_thread,
                context,
                ..
            }) => Some(Started {
                attempt: effect.attempt.clone(),
                selection: selection.clone(),
                text: text.clone(),
                native_thread: native_thread.clone(),
                context: context.clone(),
            }),
            _ => None,
        })
        .collect()
}
fn effect_kinds(step: &Step) -> Vec<&'static str> {
    step.effects
        .iter()
        .map(|effect| match &effect.body {
            EffectBody::Provider(ProviderCommand::Interrupt { .. }) => "interrupt",
            EffectBody::Provider(ProviderCommand::Start { .. }) => "start",
            EffectBody::Provider(ProviderCommand::Steer { .. }) => "steer",
            EffectBody::DetachSessions { .. } => "detach",
            _ => "other",
        })
        .collect()
}
fn attempt_statuses(s: &State) -> Vec<AttemptStatus> {
    s.attempts.iter().map(|attempt| attempt.status).collect()
}
fn provider_handoffs(s: &State) -> usize {
    s.transfers
        .iter()
        .filter(|transfer| transfer.kind == TransferKind::ProviderHandoff)
        .count()
}

// T3 SelectionRestart.integration.test.ts "detaches the old provider session
// after an active provider handoff".
#[test]
fn detaches_the_old_provider_session_after_an_active_provider_handoff() {
    let mut s = state();
    let (run, _) = running(&mut s, "first");
    let step = steer_on(
        &mut s,
        "second",
        DispatchMode::RestartActive { run: run.clone() },
        claude_selection(),
    );
    assert_eq!(step.reply, Reply::Run(run.clone()));
    assert_eq!(
        detached(&step),
        [(Some("codex".to_owned()), HANDOFF_DETACH.to_owned())]
    );
    let next = s.runs[0].attempt.clone().unwrap();
    let started = starts(&step);
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].attempt.as_ref(), Some(&next));
    provider(
        &mut s,
        "handoff-session",
        &next,
        ProviderEvent::SessionReady {
            native_thread: "claude-thread".into(),
        },
    );
    provider(
        &mut s,
        "handoff-started",
        &next,
        ProviderEvent::TurnStarted {
            native_turn: Some("claude-turn".into()),
        },
    );
    finish(&mut s, &next);

    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.attempts.len(), 2);
    assert_eq!(
        attempt_statuses(&s),
        [AttemptStatus::Superseded, AttemptStatus::Completed]
    );
    assert_eq!(s.runs[0].selection.instance, "claude");
    assert_eq!(provider_handoffs(&s), 1);
}

// T3 Orchestrator.ts dispatchSteerIntoRun: a steer whose selection names another
// instance must apply now, so it restarts the run there with the thread's history
// through the running run, and the steer becomes the restarted turn's input.
#[test]
fn a_steer_onto_another_instance_restarts_the_run_with_a_full_handoff() {
    let mut s = state();
    let (run, first) = running(&mut s, "first");
    provider(
        &mut s,
        "partial",
        &first,
        ProviderEvent::TextDelta {
            key: "answer".into(),
            kind: ProviderItem::Text,
            text: "partial answer".into(),
        },
    );
    let step = steer_on(
        &mut s,
        "steer",
        DispatchMode::SteerActive { run: run.clone() },
        claude_selection(),
    );
    assert_eq!(step.reply, Reply::Run(run.clone()));
    assert_eq!(effect_kinds(&step), ["interrupt", "detach", "start"]);
    assert_eq!(step.effects[0].attempt.as_ref(), Some(&first));
    assert_eq!(
        detached(&step),
        [(Some("codex".to_owned()), HANDOFF_DETACH.to_owned())]
    );
    let message = s
        .messages
        .iter()
        .find(|m| m.id.as_str() == "steer")
        .unwrap();
    assert_eq!(message.intent, InputIntent::Steer);
    assert!(s.items.iter().any(|item| {
        item.run.as_ref() == Some(&run)
            && matches!(&item.kind, ItemKind::UserMessage { message } if message.as_str() == "steer")
    }));
    assert_eq!(s.thread.as_ref().unwrap().selection, claude_selection());
    assert_eq!(s.runs.len(), 1);
    assert_eq!(s.runs[0].status, RunStatus::Starting);
    assert_eq!(s.runs[0].selection, claude_selection());
    assert_eq!(
        attempt_statuses(&s),
        [AttemptStatus::Superseded, AttemptStatus::Pending]
    );

    let next = s.runs[0].attempt.clone().unwrap();
    let [started] = starts(&step).try_into().ok().unwrap();
    assert_eq!(started.attempt.as_ref(), Some(&next));
    assert_eq!(started.selection, claude_selection());
    assert_eq!(started.text, "steer");
    assert_eq!(started.native_thread, None);
    let context = started.context.unwrap();
    assert_eq!(
        context
            .messages
            .iter()
            .map(|message| (message.text.as_str(), message.run_status.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("first", Some("running")),
            ("partial answer", Some("running"))
        ]
    );
    assert!(context.context.contains("Covered app runs: 1-1."));
    assert!(render_history(&context).contains("full_thread_summary"));
    let [transfer] = s.transfers.as_slice() else {
        panic!()
    };
    assert_eq!(transfer.kind, TransferKind::ProviderHandoff);
    assert_eq!(transfer.instance.as_deref(), Some("claude"));
    let delivery = transfer.delivery.as_ref().unwrap();
    assert_eq!(delivery.attempt, next);
    assert_eq!(delivery.status, ContextDeliveryStatus::Pending);

    let before = s.clone();
    let late = provider(
        &mut s,
        "late",
        &first,
        ProviderEvent::TextDelta {
            key: "answer".into(),
            kind: ProviderItem::Text,
            text: " stale".into(),
        },
    );
    assert_eq!(late.reply, Reply::Ignored);
    assert_eq!(s, before);
}

// T3 CommandPolicy.ts decideSteeringExecution: the running session must restart
// to change instance, which Claude cannot do.
#[test]
fn a_claude_run_cannot_be_steered_onto_another_instance() {
    let mut s = state();
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let (run, _) = running(&mut s, "first");
    let before = s.clone();
    for mode in [
        DispatchMode::SteerActive { run: run.clone() },
        DispatchMode::RestartActive { run: run.clone() },
    ] {
        let step = steer_on(&mut s, "steer", mode, selection());
        assert_eq!(
            step.reply,
            Reply::Rejected {
                reason: "restart-unsupported".into()
            }
        );
        assert!(step.facts.is_empty() && step.effects.is_empty());
        assert_eq!(s, before);
    }
}

// T3 Orchestrator.ts dispatchQueuedMessagePromoteToSteer: promotion steers on the
// thread's selection, so after a provider switch it restarts the run there.
#[test]
fn promoting_to_steer_after_a_provider_switch_restarts_on_the_new_instance() {
    let mut s = state();
    let (active, first) = running(&mut s, "first");
    let Reply::Run(queued) = command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    )
    .reply
    else {
        panic!()
    };
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let step = command(
        &mut s,
        "promote",
        Command::PromoteToSteer {
            queued: queued.clone(),
            active: active.clone(),
        },
    );
    assert_eq!(step.reply, Reply::Run(active.clone()));
    assert_eq!(effect_kinds(&step), ["interrupt", "detach", "start"]);
    assert_eq!(step.effects[0].attempt.as_ref(), Some(&first));
    let queued_run = s.runs.iter().find(|run| run.id == queued).unwrap();
    assert_eq!(queued_run.status, RunStatus::Cancelled);
    let message = s
        .messages
        .iter()
        .find(|m| m.id.as_str() == "queued")
        .unwrap();
    assert_eq!(message.intent, InputIntent::PromotedQueuedToSteer);
    assert_eq!(message.run.as_ref(), Some(&active));
    let run = s.runs.iter().find(|run| run.id == active).unwrap();
    assert_eq!(run.status, RunStatus::Starting);
    assert_eq!(run.selection, claude_selection());
    let [started] = starts(&step).try_into().ok().unwrap();
    assert_eq!(started.attempt, run.attempt);
    assert_eq!(started.text, "queued");
    assert_eq!(
        started
            .context
            .unwrap()
            .messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["first"]
    );
    assert_eq!(provider_handoffs(&s), 1);

    let mut s = state();
    command(
        &mut s,
        "to-claude",
        Command::SwitchProvider {
            selection: claude_selection(),
        },
    );
    let (active, _) = running(&mut s, "first");
    let Reply::Run(queued) = command(
        &mut s,
        "queued",
        send_message("queued", DispatchMode::QueueAfterActive),
    )
    .reply
    else {
        panic!()
    };
    command(
        &mut s,
        "to-codex",
        Command::SwitchProvider {
            selection: selection(),
        },
    );
    let before = s.clone();
    assert_eq!(
        command(
            &mut s,
            "promote",
            Command::PromoteToSteer { queued, active }
        )
        .reply,
        Reply::Rejected {
            reason: "restart-unsupported".into()
        }
    );
    assert_eq!(s, before);
}
