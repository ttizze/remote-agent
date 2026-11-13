//! Workspace admission is part of the pure conversation decision. The Host
//! supplies persisted preparation facts and applies strategy changes before I/O.
use super::records::WorkspacePreparation;
use super::{self as decision, *};
use crate::workspace::{SetupPhase, SetupStageId, SetupStageStatus};
/// Setup admission shares the writer transaction with AgentStarted, so Stop
/// cannot cross the point where provider creation is no longer cancellable.
pub(super) fn command(
    saved: Option<&WorkspacePreparation>,
    command: &decision::Command,
    inputs: &std::collections::BTreeMap<ClientInputId, decision::PendingInput>,
    cwd: Option<&str>,
    bindings_empty: bool,
    active_turn: Option<&TurnId>,
) -> Result<Option<(Option<crate::workspace::WorkspaceStrategy>, String)>, &'static str> {
    match command {
        decision::Command::RetryInput { id, work_locally } => retry(
            saved,
            inputs.get(id).map(|input| &input.state),
            cwd,
            bindings_empty,
            *work_locally,
        ),
        decision::Command::Interrupt { turn } => {
            let started = saved
                .and_then(|workspace| workspace.setup.as_ref())
                .is_some_and(|setup| !setup.can_cancel && setup.phase == SetupPhase::Running);
            let targeting_preparation = !commands::targets_active(active_turn, turn.as_ref());
            if started
                && targeting_preparation
                && inputs.values().any(|input| {
                    matches!(
                        input.state,
                        decision::InputState::Preparing {
                            stage: crate::session::PreparationStage::Provider,
                            ..
                        }
                    ) && turn
                        .as_ref()
                        .is_none_or(|turn| turn.as_str() == input.id.as_str())
                })
            {
                Err("The agent has already started")
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// A failed or interrupted script re-enters setup automatically. Working locally is an
/// explicit disposition of an early, unbound workspace preparation.
fn retry(
    saved: Option<&WorkspacePreparation>,
    input: Option<&decision::InputState>,
    cwd: Option<&str>,
    bindings_empty: bool,
    work_locally: bool,
) -> Result<Option<(Option<crate::workspace::WorkspaceStrategy>, String)>, &'static str> {
    let Some(saved) = saved else {
        return if work_locally {
            Err("workspace setup is unavailable")
        } else {
            Ok(None)
        };
    };
    if work_locally {
        if !bindings_empty
            || !matches!(
                input,
                Some(
                    decision::InputState::Failed(_)
                        | decision::InputState::Preparing {
                            stage: crate::session::PreparationStage::Workspace
                                | crate::session::PreparationStage::Worktree
                                | crate::session::PreparationStage::SetupScript,
                            ..
                        }
                )
            )
        {
            return Err("the agent has already started");
        }
        return Ok(Some((
            Some(crate::workspace::WorkspaceStrategy::Root),
            saved.project_cwd.clone(),
        )));
    }
    if saved.created_worktree
        && saved
            .setup
            .as_ref()
            .is_some_and(|setup| setup.phase == SetupPhase::Cancelled)
    {
        return Ok(Some((saved.strategy.clone(), saved.project_cwd.clone())));
    }
    let failed_script = saved.setup.as_ref().is_some_and(|setup| {
        setup.stages.iter().any(|stage| {
            stage.id == SetupStageId::SetupScript
                && (stage.status == SetupStageStatus::Failed
                    || setup.phase == SetupPhase::Cancelled
                        && stage.status == SetupStageStatus::Skipped
                        && stage.started_at_ms.is_some())
        })
    });
    Ok(
        (failed_script && matches!(input, Some(decision::InputState::Failed(_))))
            .then(|| (None, cwd.unwrap_or(&saved.project_cwd).to_owned())),
    )
}

// Only the fresh checkout owned by this cancelled attempt is discarded. Reused
// and user-selected worktrees remain available after a failed preparation.
pub(super) fn cancelled(
    saved: Option<&WorkspacePreparation>,
) -> Option<(Uuid, String, crate::workspace::CreatedWorktree)> {
    let saved = saved.filter(|saved| saved.created_worktree)?;
    let setup = saved.setup.as_ref()?;
    let checkout = crate::workspace::CreatedWorktree {
        path: setup.worktree_path.clone()?,
        branch: setup.branch.clone()?,
    };
    Some((saved.generation, saved.project_cwd.clone(), checkout))
}

pub(super) fn retain_root_source(
    saved: Option<&WorkspacePreparation>,
    current: Option<&str>,
    cwd: &str,
) -> bool {
    current == Some(cwd)
        && saved.is_none_or(|saved| !saved.created_worktree && saved.setup.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session::PreparationStage,
        workspace::{WorkspaceSetup, WorkspaceStrategy},
    };
    fn saved() -> WorkspacePreparation {
        WorkspacePreparation {
            project_cwd: "/project".into(),
            strategy: Some(WorkspaceStrategy::Worktree {
                base_ref: "main".into(),
                start_from_origin: false,
            }),
            generation: Uuid::from_u128(1),
            created_worktree: false,
            setup: Some(WorkspaceSetup::new(Some("main".into()), 0)),
            visible: true,
        }
    }

    #[test]
    fn stop_observes_agent_start_through_the_public_decision() {
        use crate::execution::ExecutionError;
        // Expected actions are specified per scenario, not recomputed from the
        // admission expression. A Stop may target the active turn independently.
        let cases = [
            (
                "untargeted stop prefers preparing input",
                SetupPhase::Running,
                false,
                true,
                None,
                "refuse",
            ),
            (
                "cancellable setup",
                SetupPhase::Running,
                true,
                false,
                None,
                "cancel",
            ),
            (
                "agent creating",
                SetupPhase::Running,
                false,
                false,
                None,
                "refuse",
            ),
            (
                "explicit creating input",
                SetupPhase::Running,
                false,
                false,
                Some("preparing"),
                "refuse",
            ),
            (
                "completed card",
                SetupPhase::Done,
                false,
                false,
                Some("preparing"),
                "cancel",
            ),
            (
                "cancelled card",
                SetupPhase::Cancelled,
                false,
                false,
                Some("preparing"),
                "cancel",
            ),
            (
                "stop active turn",
                SetupPhase::Running,
                false,
                true,
                Some("running"),
                "interrupt",
            ),
            (
                "target creating input beside turn",
                SetupPhase::Running,
                false,
                true,
                Some("preparing"),
                "refuse",
            ),
            (
                "target cancellable input beside turn",
                SetupPhase::Running,
                true,
                true,
                Some("preparing"),
                "cancel",
            ),
        ];
        for (name, phase, can_cancel, active, target, expected) in cases {
            let mut saved = saved();
            let setup = saved.setup.as_mut().unwrap();
            setup.phase = phase;
            setup.can_cancel = can_cancel;
            let mut state = State::new(Thread {
                name: String::new(),
                workspace: Workspace::Ready("/project".into()),
                provider: ProviderKind::Codex,
                queue_held: false,
            });
            state.inputs.insert(
                "preparing".into(),
                PendingInput {
                    id: "preparing".into(),
                    position: 1,
                    content: InputContent {
                        parts: vec![],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                    state: InputState::Preparing {
                        stage: PreparationStage::Provider,
                        action: Uuid::nil(),
                        mode: DeliveryMode::Direct,
                    },
                },
            );
            if active {
                state.turns.insert(
                    "running".into(),
                    Turn {
                        id: "running".into(),
                        ordinal: 1,
                        provider: ProviderKind::Codex,
                        queued: false,
                        observed: false,
                        started_at: None,
                        completed_at: None,
                        duration_ms: None,
                        retry_count: 0,
                        compaction: false,
                        error: None,
                        state: TurnState::Running { interrupt: None },
                    },
                );
                state.attempts.insert(
                    "running".into(),
                    Attempt {
                        id: Uuid::from_u128(2),
                        turn: "running".into(),
                        provider: ProviderKind::Codex,
                        source: "native".into(),
                    },
                );
            }
            let result = step(
                &state,
                Event::Command {
                    id: OperationId::Client(Uuid::from_u128(3)),
                    command: Command::Interrupt {
                        turn: target.map(TurnId::from),
                    },
                    receipt: None,
                },
                Context {
                    preparation: Some(&saved),
                    rewind_binding: None,
                    imported_turns: &[],
                    external_busy: false,
                    now: 100,
                    seed: Uuid::from_u128(3),
                    steers: true,
                    steer_ready: true,
                },
            );
            match expected {
                "refuse" => {
                    assert!(matches!(result.outcome, Outcome::Rejected(_)), "{name}");
                    assert!(result.actions.is_empty(), "{name}");
                }
                "cancel" => {
                    assert_eq!(result.outcome, Outcome::Accepted, "{name}");
                    assert!(
                        matches!(&result.actions[..], [Action::CancelPreparation { .. }]),
                        "{name}"
                    );
                    assert!(
                        matches!(
                            state.apply(&result.changes).inputs[&ClientInputId::from("preparing")]
                                .state,
                            InputState::Failed(ExecutionError { .. })
                        ),
                        "{name}"
                    );
                }
                "interrupt" => {
                    assert_eq!(result.outcome, Outcome::Accepted, "{name}");
                    assert!(
                        matches!(&result.actions[..], [Action::Interrupt { .. }]),
                        "{name}"
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn work_locally_requires_an_early_unbound_input() {
        let saved = saved();
        for (stage, early) in [
            (PreparationStage::Workspace, true),
            (PreparationStage::Worktree, true),
            (PreparationStage::SetupScript, true),
            (PreparationStage::Provider, false),
        ] {
            let input = decision::InputState::Preparing {
                stage,
                action: Uuid::from_u128(2),
                mode: decision::DeliveryMode::Direct,
            };
            for unbound in [false, true] {
                let result = retry(Some(&saved), Some(&input), Some("/checkout"), unbound, true);
                if early && unbound {
                    let (workspace, cwd) = result.unwrap().unwrap();
                    assert_eq!(workspace, Some(WorkspaceStrategy::Root));
                    assert_eq!(cwd, "/project");
                } else {
                    assert!(result.is_err(), "{stage:?}, unbound={unbound}");
                }
            }
        }
        let failed = decision::InputState::Failed(Default::default());
        assert!(
            retry(Some(&saved), Some(&failed), Some("/checkout"), true, true)
                .unwrap()
                .is_some()
        );
        assert!(retry(Some(&saved), Some(&failed), Some("/checkout"), false, true).is_err());
        assert!(retry(None, Some(&failed), Some("/checkout"), true, true).is_err());
    }

    #[test]
    fn working_locally_retries_in_the_project_root_through_the_public_decision() {
        let saved = saved();
        let input = PendingInput {
            id: "failed".into(),
            position: 1,
            content: InputContent {
                parts: vec![InputPart::Text {
                    text: "work".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
            state: InputState::Failed(Default::default()),
        };
        let mut state = State::new(Thread {
            name: String::new(),
            workspace: Workspace::Folder("/checkout".into()),
            provider: crate::session::ProviderKind::Codex,
            queue_held: false,
        });
        state.inputs.insert(input.id.clone(), input);
        let result = decision::step(
            &state,
            Event::Command {
                id: OperationId::Client(Uuid::from_u128(10)),
                command: Command::RetryInput {
                    id: "failed".into(),
                    work_locally: true,
                },
                receipt: None,
            },
            Context {
                preparation: Some(&saved),
                rewind_binding: None,
                imported_turns: &[],
                external_busy: false,
                now: 100,
                seed: Uuid::from_u128(11),
                steers: false,
                steer_ready: true,
            },
        );
        assert_eq!(result.outcome, Outcome::Accepted);
        assert_eq!(
            state.apply(&result.changes).thread.workspace,
            Workspace::Folder("/project".into())
        );
        assert!(
            matches!(&result.actions[..], [Action::Prepare { cwd: Some(cwd), .. }] if cwd == "/project")
        );
    }

    #[test]
    fn retry_reenters_only_a_failed_script_and_preserves_the_checkout() {
        let mut saved = saved();
        let failed = decision::InputState::Failed(Default::default());
        let queued = decision::InputState::Queued { steer: None };
        assert!(
            retry(Some(&saved), Some(&failed), Some("/checkout"), true, false)
                .unwrap()
                .is_none()
        );
        saved
            .setup
            .as_mut()
            .unwrap()
            .stage(SetupStageId::SetupScript, SetupStageStatus::Failed, 1);
        assert!(
            retry(Some(&saved), Some(&queued), Some("/checkout"), true, false)
                .unwrap()
                .is_none()
        );
        let (workspace, cwd) = retry(Some(&saved), Some(&failed), Some("/checkout"), true, false)
            .unwrap()
            .unwrap();
        assert_eq!(workspace, None);
        assert_eq!(cwd, "/checkout");
    }
}
