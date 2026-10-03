//! Host input-routing decisions. Clients submit intent without choosing native operations.
use agent_protocol::models::SessionStatus;

/// Pure execution decision used by the Host after reading native state and
/// overlaying its live execution. Client caches never choose an input route.
#[derive(Debug, PartialEq)]
pub(crate) enum SubmissionTarget<'a> {
    Steer(&'a str),
    Queue,
    Start { cwd: &'a str },
}
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionState {
    pub status: SessionStatus,
    pub running_turn: Option<agent_protocol::ids::TurnId>,
    pub accepts_steer: bool,
    pub accepts_queue: bool,
}
pub(super) fn submission_target<'a>(
    state: &'a SessionState,
    cwd: Option<&'a str>,
) -> Result<SubmissionTarget<'a>, &'static str> {
    if state.status == SessionStatus::Running {
        if state.accepts_steer
            && let Some(turn) = &state.running_turn
        {
            return Ok(SubmissionTarget::Steer(turn));
        }
        if state.accepts_queue {
            return Ok(SubmissionTarget::Queue);
        }
        return Err("this execution does not accept additional input");
    }
    Ok(SubmissionTarget::Start {
        cwd: cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .ok_or("thread working directory is unknown")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_requires_observed_execution_and_adapter_support() {
        for status in [
            SessionStatus::Idle,
            SessionStatus::Unknown,
            SessionStatus::Unavailable,
        ] {
            let state = SessionState {
                status,
                running_turn: Some("historical".into()),
                accepts_steer: true,
                accepts_queue: true,
            };
            assert_eq!(
                submission_target(&state, Some("/project")).unwrap(),
                SubmissionTarget::Start { cwd: "/project" }
            );
            assert!(submission_target(&state, Some(" ")).is_err());
        }
        for (steer, queue, turn, expected) in [
            (
                true,
                true,
                Some("live"),
                Some(SubmissionTarget::Steer("live")),
            ),
            (true, true, None, Some(SubmissionTarget::Queue)),
            (false, true, Some("live"), Some(SubmissionTarget::Queue)),
            (false, false, Some("live"), None),
            (true, false, None, None),
        ] {
            let state = SessionState {
                status: SessionStatus::Running,
                running_turn: turn.map(Into::into),
                accepts_steer: steer,
                accepts_queue: queue,
            };
            assert_eq!(submission_target(&state, None).ok(), expected);
        }
    }
    proptest::proptest! {
        #[test]
        fn additional_input_never_starts_a_second_running_turn(steer in proptest::bool::ANY, queue in proptest::bool::ANY, turn in proptest::option::of("[a-z]{1,24}")) {
            let state=SessionState {status:SessionStatus::Running,running_turn:turn.clone().map(Into::into),accepts_steer:steer,accepts_queue:queue};
            let route=submission_target(&state,Some("/project"));
            match route {
                Ok(SubmissionTarget::Steer(id))=>{proptest::prop_assert!(steer);proptest::prop_assert_eq!(Some(id),turn.as_deref());},
                Ok(SubmissionTarget::Queue)=>{proptest::prop_assert!(queue);proptest::prop_assert!(!steer||turn.is_none());},
                Err(_)=>proptest::prop_assert!(!queue&&(!steer||turn.is_none())),
                Ok(SubmissionTarget::Start {..})=>proptest::prop_assert!(false,"running execution must not start another turn"),
            }
        }
    }
}
