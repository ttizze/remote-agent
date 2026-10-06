use super::*;

fn rollback(s: &mut State, key: &str, checkpoint: &CheckpointId) -> Step {
    command(
        s,
        key,
        Command::Rollback {
            checkpoint: checkpoint.clone(),
            restore_files: false,
        },
    )
}
fn later_provider() -> ModelSelection {
    let mut later = selection();
    later.instance = "later-provider".into();
    later
}

// T3 CheckpointRollbackService.ts rewinds the active provider thread, and
// later turns use the replacement native identity it reports.
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

// T3 Orchestrator.ts dispatchCheckpointRollback: a rollback needs an active
// provider thread, and its target turn must belong to that thread.
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

// T3 CheckpointRollbackService.ts: once the selection names another instance
// than the active provider thread, the rollback fails without rewinding.
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
