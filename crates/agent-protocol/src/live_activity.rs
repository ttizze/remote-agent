//! Shared task presentation and authenticated Live Activity registration.
use crate::execution::TurnStatus;
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
    pub activity_id: String,
    #[serde(with = "crate::protocol::bytes")]
    pub token: Vec<u8>,
    pub environment: PushEnvironment,
}
impl std::fmt::Debug for RegisterLiveActivity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisterLiveActivity")
            .field("activity_id", &self.activity_id)
            .field("environment", &self.environment)
            .field("token", &"[redacted]")
            .finish()
    }
}
impl RegisterLiveActivity {
    pub fn validate(&self) -> Result<(), &'static str> {
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

/// Counts keep the system surface bounded even with hundreds of active tasks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActivitySummary {
    pub running: u32,
    pub waiting: u32,
    pub unknown: u32,
}
impl TaskActivitySummary {
    pub fn from_statuses<'a>(statuses: impl IntoIterator<Item = &'a str>) -> Self {
        let mut summary = Self::default();
        for status in statuses {
            match status {
                "running" | "finishing" => summary.running += 1,
                "waiting" => summary.waiting += 1,
                "unknown" => summary.unknown += 1,
                _ => {}
            }
        }
        summary
    }
    pub fn ongoing(self) -> bool {
        self.running + self.waiting + self.unknown > 0
    }
}

/// Unknown state cannot certify completion; a running turn outlives its idle notification.
pub fn task_phase(
    known: bool,
    active: bool,
    waiting: bool,
    turn: Option<TurnStatus>,
) -> (&'static str, bool) {
    if waiting {
        return ("waiting", true);
    }
    if !known {
        return ("unknown", false);
    }
    if active {
        return ("running", true);
    }
    match turn {
        Some(TurnStatus::Completed) => ("completed", false),
        Some(TurnStatus::Failed) => ("failed", false),
        Some(TurnStatus::Interrupted) => ("interrupted", false),
        Some(TurnStatus::Running) => ("finishing", true),
        _ => ("finished", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_single_waiting_or_uncertain_task_keeps_the_activity_alive() {
        for (status, ongoing) in [
            ("running", true),
            ("finishing", true),
            ("waiting", true),
            ("unknown", true),
            ("completed", false),
            ("failed", false),
        ] {
            assert_eq!(
                TaskActivitySummary::from_statuses([status]).ongoing(),
                ongoing
            );
        }
    }
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
                assert_eq!(task_phase(false, active, false, turn), ("unknown", false));
                for known in [false, true] {
                    assert_eq!(task_phase(known, active, true, turn), ("waiting", true));
                }
            }
        }
    }
    #[test]
    fn execution_and_final_results_have_distinct_phases() {
        let cases = [
            (None, ("finished", false)),
            (Some(TurnStatus::Unknown), ("finished", false)),
            (Some(TurnStatus::Running), ("finishing", true)),
            (Some(TurnStatus::Completed), ("completed", false)),
            (Some(TurnStatus::Failed), ("failed", false)),
            (Some(TurnStatus::Interrupted), ("interrupted", false)),
        ];
        for (turn, expected) in cases {
            assert_eq!(task_phase(true, false, false, turn), expected);
            assert_eq!(task_phase(true, true, false, turn), ("running", true));
        }
    }
    proptest::proptest! {
        #[test]
        fn task_summary_counts_every_ongoing_state_and_is_order_independent(states in proptest::collection::vec(0u8..7, 0..500)) {
            let names = ["running", "finishing", "waiting", "unknown", "completed", "failed", "finished"];
            let summary = TaskActivitySummary::from_statuses(states.iter().map(|state| names[usize::from(*state)]));
            let reversed = TaskActivitySummary::from_statuses(states.iter().rev().map(|state| names[usize::from(*state)]));
            proptest::prop_assert_eq!(summary, reversed);
            proptest::prop_assert_eq!(summary.running, states.iter().filter(|state| **state < 2).count() as u32);
            proptest::prop_assert_eq!(summary.waiting, states.iter().filter(|state| **state == 2).count() as u32);
            proptest::prop_assert_eq!(summary.unknown, states.iter().filter(|state| **state == 3).count() as u32);
            proptest::prop_assert_eq!(summary.ongoing(), states.iter().any(|state| *state < 4));
        }
        #[test]
        fn registration_bounds_are_enforced(activity in ".{0,140}", token in proptest::collection::vec(proptest::num::u8::ANY, 0..270)) {
            let valid = !activity.is_empty() && activity.len() <= 128 && !token.is_empty() && token.len() <= 256;
            let params = RegisterLiveActivity { activity_id: activity, token, environment:PushEnvironment::Sandbox };
            proptest::prop_assert_eq!(params.validate().is_ok(), valid);
        }
    }
}
