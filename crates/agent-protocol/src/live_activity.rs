//! Shared task presentation and authenticated Live Activity registration.
use crate::{execution::TurnStatus, session::SessionRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PushEnvironment {
    Sandbox,
    Production,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterLiveActivity {
    pub session: SessionRef,
    pub activity_id: String,
    #[serde(with = "crate::protocol::bytes")]
    pub token: Vec<u8>,
    pub environment: PushEnvironment,
}
impl std::fmt::Debug for RegisterLiveActivity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisterLiveActivity")
            .field("session", &self.session)
            .field("activity_id", &self.activity_id)
            .field("environment", &self.environment)
            .field("token", &"[redacted]")
            .finish()
    }
}
impl RegisterLiveActivity {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.session.validate()?;
        if self.activity_id.is_empty()
            || self.activity_id.len() > 128
            || self.token.is_empty()
            || self.token.len() > 256
        {
            return Err("invalid Live Activity registration");
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnregisterLiveActivity {
    pub activity_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveActivityRegistration {
    pub enabled: bool,
}

/// Unknown state cannot certify completion; a running turn outlives its idle notification.
pub fn task_phase(
    known: bool,
    active: bool,
    waiting: bool,
    turn: Option<TurnStatus>,
) -> (&'static str, &'static str, bool) {
    if waiting {
        return ("waiting", "確認待ち", true);
    }
    if !known {
        return ("unknown", "更新待ち", false);
    }
    if active {
        return ("running", "実行中", true);
    }
    match turn {
        Some(TurnStatus::Completed) => ("completed", "完了", false),
        Some(TurnStatus::Failed) => ("failed", "失敗", false),
        Some(TurnStatus::Interrupted) => ("interrupted", "中断", false),
        Some(TurnStatus::Running) => ("finishing", "結果を確認中", true),
        _ => ("finished", "終了", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_state_and_pending_approval_cannot_complete_a_task() {
        for turn in [
            None,
            Some(TurnStatus::Unknown),
            Some(TurnStatus::Running),
            Some(TurnStatus::Completed),
            Some(TurnStatus::Failed),
            Some(TurnStatus::Interrupted),
        ] {
            for active in [false, true] {
                assert_eq!(
                    task_phase(false, active, false, turn),
                    ("unknown", "更新待ち", false)
                );
                for known in [false, true] {
                    assert_eq!(
                        task_phase(known, active, true, turn),
                        ("waiting", "確認待ち", true)
                    );
                }
            }
        }
    }
    #[test]
    fn execution_and_final_results_have_distinct_system_labels() {
        let cases = [
            (None, ("finished", "終了", false)),
            (Some(TurnStatus::Unknown), ("finished", "終了", false)),
            (
                Some(TurnStatus::Running),
                ("finishing", "結果を確認中", true),
            ),
            (Some(TurnStatus::Completed), ("completed", "完了", false)),
            (Some(TurnStatus::Failed), ("failed", "失敗", false)),
            (
                Some(TurnStatus::Interrupted),
                ("interrupted", "中断", false),
            ),
        ];
        for (turn, expected) in cases {
            assert_eq!(task_phase(true, false, false, turn), expected);
            assert_eq!(
                task_phase(true, true, false, turn),
                ("running", "実行中", true)
            );
        }
    }
    proptest::proptest! {
        #[test]
        fn registration_bounds_are_enforced(activity in ".{0,140}", token in proptest::collection::vec(proptest::num::u8::ANY, 0..270)) {
            let valid = !activity.is_empty() && activity.len() <= 128 && !token.is_empty() && token.len() <= 256;
            let params = RegisterLiveActivity { session: SessionRef { provider: crate::session::ProviderKind::Codex, id: "task".into() }, activity_id: activity, token, environment:PushEnvironment::Sandbox };
            proptest::prop_assert_eq!(params.validate().is_ok(), valid);
        }
    }
}
