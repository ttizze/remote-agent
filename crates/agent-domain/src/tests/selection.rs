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
