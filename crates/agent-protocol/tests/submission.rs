use agent_protocol::models::Turn;
use std::sync::Arc;

#[test]
fn host_submission_route_uses_current_execution_and_never_unfinished_idle_history() {
    use agent_protocol::{
        models::{ThreadStatus, ThreadStatusKind},
        session::{SubmissionTarget, submission_target},
    };
    let running = vec![Arc::new(Turn {
        id: "live".into(),
        status: Some("inProgress".into()),
        ..Default::default()
    })];
    let idle = ThreadStatus {
        kind: ThreadStatusKind::Idle,
    };
    let active = ThreadStatus {
        kind: ThreadStatusKind::Active,
    };
    let unloaded = ThreadStatus {
        kind: ThreadStatusKind::NotLoaded,
    };
    assert_eq!(
        submission_target(Some(&idle), &running, Some("/project")).unwrap(),
        SubmissionTarget::Start {
            cwd: "/project",
            resume: false
        }
    );
    assert_eq!(
        submission_target(Some(&active), &running, None).unwrap(),
        SubmissionTarget::Steer("live")
    );
    assert_eq!(
        submission_target(Some(&active), &[], None).unwrap(),
        SubmissionTarget::Queue
    );
    assert_eq!(
        submission_target(Some(&unloaded), &running, Some("/project")).unwrap(),
        SubmissionTarget::Start {
            cwd: "/project",
            resume: true
        }
    );
    assert!(submission_target(Some(&idle), &[], Some(" ")).is_err());
}
