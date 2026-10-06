use super::*;

fn accepted(step: &Step) -> Command {
    step.effects
        .iter()
        .find_map(|effect| match &effect.body {
            EffectBody::SendToThread { command, .. }
                if matches!(command.as_ref(), Command::AcceptDelegation { .. }) =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .unwrap()
}

// The child runs with the requested modes, takes the title, and copies the
// parent's sidebar arrangement.
#[test]
fn a_delegated_child_uses_the_requested_modes_and_title() {
    let mut s = state();
    command(
        &mut s,
        "pin",
        Command::Pin {
            pinned: true,
            order: Some("a0".into()),
        },
    );
    running(&mut s, "parent");
    let step = command(
        &mut s,
        "delegate",
        Command::Delegate {
            task: NodeId::new("task").unwrap(),
            child: ThreadId::new("child").unwrap(),
            prompt: "  Review the change  ".into(),
            title: Some(" Review ".into()),
            selection: selection(),
            runtime_mode: RuntimeMode::ApprovalRequired,
            interaction_mode: InteractionMode::Plan,
            wake: CompletionWake::Always,
        },
    );
    let Command::AcceptDelegation {
        title,
        runtime_mode,
        interaction_mode,
        arrangement,
        message,
        ..
    } = accepted(&step)
    else {
        unreachable!()
    };
    assert_eq!(title, "Review");
    assert_eq!(runtime_mode, RuntimeMode::ApprovalRequired);
    assert_eq!(interaction_mode, InteractionMode::Plan);
    assert_eq!(message.text, "Review the change");
    assert_eq!(message.creation_source, "mcp");
    assert_eq!(arrangement.pin_order.as_deref(), Some("a0"));
    assert!(arrangement.pinned_at.is_some());
    assert_eq!(s.tasks[0].title.as_deref(), Some("Review"));

    let mut child = State::default();
    command(&mut child, "accept", accepted(&step));
    let thread = child.thread.as_ref().unwrap();
    assert_eq!(thread.runtime_mode, RuntimeMode::ApprovalRequired);
    assert_eq!(thread.interaction_mode, InteractionMode::Plan);
    assert_eq!(thread.pin_order.as_deref(), Some("a0"));
    assert_eq!(child.runs[0].status, RunStatus::Starting);
}

// Decodes the title as a trimmed non-empty string and falls back to the
// prompt.
#[test]
fn a_blank_title_is_rejected_and_an_untitled_child_takes_the_prompt() {
    let mut s = state();
    let (_, attempt) = running(&mut s, "parent");
    provider(
        &mut s,
        "native",
        &attempt,
        ProviderEvent::SubagentStarted {
            background: false,
            native_thread: None,
            key: "native".into(),
            parent: None,
            prompt: "Native work".into(),
            model: None,
        },
    );
    assert_eq!(
        command(
            &mut s,
            "blank-title",
            Command::Delegate {
                task: NodeId::new("blank").unwrap(),
                child: ThreadId::new("child-blank").unwrap(),
                prompt: "Work".into(),
                title: Some("  ".into()),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                wake: CompletionWake::Always,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "title-required".into()
        }
    );
    let step = command(
        &mut s,
        "delegate",
        Command::Delegate {
            task: NodeId::new("task").unwrap(),
            child: ThreadId::new("child").unwrap(),
            prompt: "Work".into(),
            title: None,
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            wake: CompletionWake::Always,
        },
    );
    let Command::AcceptDelegation { title, .. } = accepted(&step) else {
        unreachable!()
    };
    assert_eq!(title, "Work");
    assert_eq!(s.tasks.len(), 2);
}
