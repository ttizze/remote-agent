//! Host input-routing decisions. Clients submit intent without choosing native operations.
use agent_protocol::models::SessionStatus;

/// Pure execution decision used by the Host after reading native state and
/// overlaying its live execution. Client caches never choose an input route.
#[derive(Debug, PartialEq)]
pub(crate) enum SubmissionTarget {
    Steer(String),
    Queue,
    Start { cwd: String },
}
pub(super) fn submission_target(
    status: SessionStatus,
    running_turn: Option<&str>,
    active_steering: bool,
    cwd: Option<&str>,
) -> Result<SubmissionTarget, &'static str> {
    if status == SessionStatus::Running {
        if let Some(turn) =
            agent_protocol::queue::steering_turn(status, running_turn, active_steering)
        {
            return Ok(SubmissionTarget::Steer(turn.into()));
        }
        return Ok(SubmissionTarget::Queue);
    }
    Ok(SubmissionTarget::Start {
        cwd: cwd
            .filter(|cwd| !cwd.trim().is_empty())
            .ok_or("thread working directory is unknown")?
            .into(),
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
            for mode in [true, false] {
                assert_eq!(
                    submission_target(status, Some("historical"), mode, Some("/project")).unwrap(),
                    SubmissionTarget::Start {
                        cwd: "/project".into()
                    }
                );
                assert!(submission_target(status, Some("historical"), mode, Some(" ")).is_err());
            }
        }
        for (mode, turn, expected) in [
            (true, Some("live"), SubmissionTarget::Steer("live".into())),
            (true, None, SubmissionTarget::Queue),
            (false, Some("live"), SubmissionTarget::Queue),
            (false, None, SubmissionTarget::Queue),
        ] {
            assert_eq!(
                submission_target(SessionStatus::Running, turn, mode, None).unwrap(),
                expected
            );
        }
    }
    proptest::proptest! {
        #[test]
        fn additional_input_never_starts_a_second_running_turn(steer in proptest::bool::ANY, turn in proptest::option::of("[a-z]{1,24}")) {
            let mode = steer;
            let route=submission_target(SessionStatus::Running,turn.as_deref(),mode,Some("/project"));
            match route {
                Ok(SubmissionTarget::Steer(id))=>{proptest::prop_assert!(steer);proptest::prop_assert_eq!(Some(id.as_str()),turn.as_deref());},
                Ok(SubmissionTarget::Queue)=>{proptest::prop_assert!(!steer||turn.is_none());},
                Err(_)=>proptest::prop_assert!(false,"running execution accepts additional input"),
                Ok(SubmissionTarget::Start {..})=>proptest::prop_assert!(false,"running execution must not start another turn"),
            }
        }
    }
}
