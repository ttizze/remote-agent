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
    active_turn: bool,
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
            let targeting_preparation = !active_turn || turn.is_some();
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
        return Ok(Some((None, saved.project_cwd.clone())));
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
