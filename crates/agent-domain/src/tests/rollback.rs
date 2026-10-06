use super::*;

fn rollback(s: &mut State, key: &str, checkpoint: &CheckpointId) -> Step {
    command(
        s,
        key,
        Command::Rollback {
            checkpoint: checkpoint.clone(),
            restore_files: false,
            restore_refusal: None,
        },
    )
}
fn later_provider() -> ModelSelection {
    let mut later = selection();
    later.instance = "later-provider".into();
    later
}

// Rewinds the active provider thread, and later turns use the replacement
// native identity it reports.
#[test]
fn rollback_rewinds_the_active_provider_and_uses_its_replacement_identity() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    let step = rollback(&mut s, "rollback", &cp);
    let EffectBody::Rollback { providers, .. } = &step.effects[0].body else {
        panic!()
    };
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].instance, "codex");
    assert!(matches!(&providers[0].command,
        ProviderCommand::Rollback { native_thread, .. } if native_thread == "native-thread"));
    let binding = NativeBinding {
        instance: "codex".into(),
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
    assert_eq!(s.runs[1].status, RunStatus::RolledBack);
    let step = command(
        &mut s,
        "continue",
        send_message("continue", DispatchMode::StartImmediately),
    );
    assert!(step.effects.iter().any(|effect| matches!(&effect.body,
        EffectBody::Provider(ProviderCommand::Start { native_thread: Some(thread), .. })
            if thread == "native-replacement")));
}

// A rollback needs an active provider thread, and its target turn must
// belong to that thread.
#[test]
fn rollback_targets_must_belong_to_the_active_provider_thread() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    command(
        &mut s,
        "switch",
        Command::SwitchProvider {
            selection: later_provider(),
        },
    );
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    assert_eq!(
        rollback(&mut s, "rollback", &cp).reply,
        Reply::Rejected {
            reason: "rollback-provider-thread-mismatch".into()
        }
    );
    assert!(s.rollback.is_none());

    let mut empty = state();
    let checkpoint = CheckpointId::new("none").unwrap();
    assert_eq!(
        rollback(&mut empty, "empty", &checkpoint).reply,
        Reply::Rejected {
            reason: "no-active-provider-thread".into()
        }
    );
}

// Once the selection names another instance than the active provider thread,
// the rollback fails without rewinding.
#[test]
fn rollback_fails_when_the_selection_left_the_active_provider() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    command(
        &mut s,
        "switch",
        Command::SelectModel {
            selection: later_provider(),
        },
    );
    let step = rollback(&mut s, "rollback", &cp);
    assert_eq!(step.reply, Reply::Accepted);
    assert!(step.effects.is_empty());
    assert!(s.rollback.is_none());
    assert_eq!(
        s.rollback_failure.as_deref(),
        Some(
            "Active provider changed before rollback target cp-first could execute on thread thread."
        )
    );
    assert_eq!(s.runs[1].status, RunStatus::Completed);
}

// A file restore the Host found unsafe is rejected at admission, after the
// target checks; a rewind without files ignores it.
#[test]
fn a_restore_the_host_refused_is_rejected_at_admission() {
    let refusal = "File restore requires an isolated worktree.";
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let refused = |restore_files: bool, checkpoint: &CheckpointId| Command::Rollback {
        checkpoint: checkpoint.clone(),
        restore_files,
        restore_refusal: Some(refusal.into()),
    };
    let missing = CheckpointId::new("missing").unwrap();
    assert_eq!(
        command(&mut s, "missing", refused(true, &missing)).reply,
        Reply::Rejected {
            reason: "checkpoint-not-found".into()
        }
    );
    let step = command(&mut s, "refused", refused(true, &cp));
    assert_eq!(
        step.reply,
        Reply::Rejected {
            reason: refusal.into()
        }
    );
    assert!(step.facts.is_empty() && step.effects.is_empty());
    assert!(s.rollback.is_none());
    let step = command(&mut s, "conversation", refused(false, &cp));
    assert_eq!(step.reply, Reply::Accepted);
    assert!(matches!(
        &step.effects[0].body,
        EffectBody::Rollback { restore: None, .. }
    ));
}

// A resent rollback returns its first result whatever the Host resolves again.
#[test]
fn the_host_refusal_is_not_part_of_the_rollback_identity() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let rollback = |restore_refusal: Option<&str>| Command::Rollback {
        checkpoint: cp.clone(),
        restore_files: true,
        restore_refusal: restore_refusal.map(Into::into),
    };
    let first = command(&mut s, "rollback", rollback(Some("shared")));
    let replay = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "rollback".into(),
            input: Input::Command {
                id: CommandId::new("rollback").unwrap(),
                command: Box::new(rollback(None)),
                receipt: first.receipt.clone(),
            },
        },
    );
    assert_eq!(replay.reply, first.reply);
    assert_eq!(replay.receipt, first.receipt);
}

// Asks the provider to rewind whenever later runs exist: after a failed
// rewind dropped the native thread, a new rollback fails instead of hiding
// the later runs without rewinding anything.
#[test]
fn rollback_without_a_native_thread_to_rewind_fails() {
    let mut s = state();
    let (first, a) = running(&mut s, "first");
    finish(&mut s, &a);
    let cp = checkpoint(&mut s, &first, &a, "cp-first");
    let (second, b) = running(&mut s, "second");
    finish(&mut s, &b);
    checkpoint(&mut s, &second, &b, "cp-second");
    rollback(&mut s, "rollback", &cp);
    let started = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "rewinding".into(),
            input: Input::RollbackRewindStarted {
                command: CommandId::new("rollback").unwrap(),
                instances: vec!["codex".into()],
            },
        },
    );
    s = fold(&s, &started.facts).unwrap();
    result(
        &mut s,
        "failed",
        EffectResult::RollbackFailed {
            command: CommandId::new("rollback").unwrap(),
            message: ROLLBACK_FAILED_MESSAGE.into(),
        },
    );
    assert!(s.native_sessions.is_empty());
    let retry = rollback(&mut s, "retry", &cp);
    assert_eq!(retry.reply, Reply::Accepted);
    assert!(retry.effects.is_empty());
    assert!(s.rollback.is_none());
    assert_eq!(s.rollback_failure.as_deref(), Some(ROLLBACK_FAILED_MESSAGE));
    assert_eq!(s.runs[1].status, RunStatus::Completed);
}
