//! Host input-routing decisions. Clients submit intent without choosing native operations.
use agent_protocol::models::{SessionStatus, Turn, TurnStatus};
use std::sync::Arc;

/// Pure execution decision used by the Host after reading native state and
/// overlaying its live execution. Client caches never choose an input route.
#[derive(Debug, PartialEq)]
pub(super) enum SubmissionTarget<'a> {
    Steer(&'a str),
    Queue,
    Start { cwd: &'a str },
}
pub(super) fn submission_target<'a>(
    status: SessionStatus,
    turns: &'a [Arc<Turn>],
    cwd: Option<&'a str>,
) -> Result<SubmissionTarget<'a>, &'static str> {
    if status == SessionStatus::Unavailable {
        return Err("provider execution is unavailable");
    }
    if status == SessionStatus::Running {
        if let Some(turn) = turns
            .iter()
            .rev()
            .find(|turn| turn.status == TurnStatus::Running && !turn.id.trim().is_empty())
        {
            return Ok(SubmissionTarget::Steer(&turn.id));
        }
        return Ok(SubmissionTarget::Queue);
    }
    Ok(SubmissionTarget::Start {
        cwd: cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .ok_or("thread working directory is unknown")?,
    })
}

#[cfg(test)]
mod tests {
    use agent_protocol::models::Turn;
    use std::sync::Arc;

    #[test]
    fn host_submission_route_uses_current_execution_and_never_unfinished_idle_history() {
        use super::{SubmissionTarget, submission_target};
        use agent_protocol::models::SessionStatus;
        let running = vec![Arc::new(Turn {
            id: "live".into(),
            status: agent_protocol::execution::TurnStatus::Running,
            ..Default::default()
        })];
        let idle = SessionStatus::Idle;
        let active = SessionStatus::Running;
        let unloaded = SessionStatus::Unknown;
        assert_eq!(
            submission_target(idle, &running, Some("/project")).unwrap(),
            SubmissionTarget::Start { cwd: "/project" }
        );
        assert_eq!(
            submission_target(active, &running, None).unwrap(),
            SubmissionTarget::Steer("live")
        );
        assert_eq!(
            submission_target(active, &[], None).unwrap(),
            SubmissionTarget::Queue
        );
        for turn in [
            Turn {
                id: " ".into(),
                status: agent_protocol::execution::TurnStatus::Running,
                ..Default::default()
            },
            Turn {
                id: "finished".into(),
                status: agent_protocol::execution::TurnStatus::Completed,
                ..Default::default()
            },
        ] {
            assert_eq!(
                submission_target(active, &[Arc::new(turn)], None).unwrap(),
                SubmissionTarget::Queue
            );
        }
        assert_eq!(
            submission_target(unloaded, &running, Some("/project")).unwrap(),
            SubmissionTarget::Start { cwd: "/project" }
        );
        assert!(submission_target(idle, &[], Some(" ")).is_err());
    }
}
