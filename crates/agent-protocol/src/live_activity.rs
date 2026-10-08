//! Shared task presentation and authenticated Live Activity registration.
use crate::execution::TurnStatus;
use serde::{Deserialize, Serialize};

pub const TASK_ACTIVITY_PUSH_FRESHNESS_SECONDS: u32 = 10 * 60;
pub const TASK_ACTIVITY_BACKGROUND_FRESHNESS_SECONDS: u32 = 30;
pub const TASK_ACTIVITY_DISMISS_SECONDS: u32 = 60;

pub fn task_activity_update_is_urgent(urgent: bool, total: u32, previous_total: u32) -> bool {
    urgent || total < previous_total
}

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

    pub fn display(self) -> TaskActivityDisplay {
        let total = self.running + self.waiting + self.unknown;
        let mut parts = Vec::new();
        if self.waiting > 0 {
            parts.push(format!("確認待ち {}件", self.waiting));
        }
        if self.running > 0 {
            parts.push(format!("実行中 {}件", self.running));
        }
        if self.unknown > 0 {
            parts.push(format!("状態確認中 {}件", self.unknown));
        }
        let mut icons = Vec::new();
        for (count, kind, label) in [
            (self.waiting, TaskActivityIconKind::Waiting, "確認待ち"),
            (self.running, TaskActivityIconKind::Running, "実行中"),
            (self.unknown, TaskActivityIconKind::Unknown, "状態確認中"),
        ] {
            for _ in 0..count.min(12 - icons.len() as u32) {
                icons.push(TaskActivityIcon {
                    kind,
                    label: label.into(),
                });
            }
        }
        let overflow = total.saturating_sub(icons.len() as u32);
        if total == 0 {
            icons.push(TaskActivityIcon {
                kind: TaskActivityIconKind::Finished,
                label: "すべてのタスクが終了".into(),
            });
        }
        let current = TaskActivityView {
            total,
            label: if total == 0 {
                "すべてのタスクが終了".into()
            } else {
                parts.join(" · ")
            },
            icons,
            overflow,
        };
        let stale = if total == 0 {
            current.clone()
        } else {
            TaskActivityView {
                total,
                label: "更新待ち".into(),
                icons: current
                    .icons
                    .iter()
                    .map(|_| TaskActivityIcon {
                        kind: TaskActivityIconKind::Unknown,
                        label: "更新待ち".into(),
                    })
                    .collect(),
                overflow,
            }
        };
        TaskActivityDisplay {
            current,
            stale,
            can_start: self.running + self.waiting > 0,
            ongoing: total > 0,
            urgent: total == 0 || self.waiting > 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskActivityIconKind {
    Running,
    Waiting,
    Unknown,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityIcon {
    pub kind: TaskActivityIconKind,
    pub label: String,
}

/// One bounded presentation for local updates and APNs. Native views only render it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityDisplay {
    pub current: TaskActivityView,
    pub stale: TaskActivityView,
    pub can_start: bool,
    pub ongoing: bool,
    pub urgent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskActivityView {
    pub total: u32,
    pub label: String,
    pub icons: Vec<TaskActivityIcon>,
    pub overflow: u32,
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
    fn finishing_one_task_is_immediate_while_stable_and_growing_counts_are_regular() {
        assert!(task_activity_update_is_urgent(false, 3, 4));
        assert!(!task_activity_update_is_urgent(false, 3, 3));
        assert!(!task_activity_update_is_urgent(false, 4, 3));
        assert!(task_activity_update_is_urgent(true, 4, 3));
    }
    #[test]
    fn fresh_unknown_tasks_do_not_hide_running_or_waiting_tasks() {
        let display = TaskActivitySummary {
            running: 2,
            waiting: 1,
            unknown: 1,
        }
        .display();
        assert!(display.ongoing && display.can_start && display.urgent);
        assert_eq!(
            display.current.label,
            "確認待ち 1件 · 実行中 2件 · 状態確認中 1件"
        );
        assert_eq!(
            display
                .current
                .icons
                .iter()
                .map(|icon| icon.kind)
                .collect::<Vec<_>>(),
            [
                TaskActivityIconKind::Waiting,
                TaskActivityIconKind::Running,
                TaskActivityIconKind::Running,
                TaskActivityIconKind::Unknown
            ]
        );
        assert_eq!(display.stale.label, "更新待ち");
        assert!(
            display
                .stale
                .icons
                .iter()
                .all(|icon| icon.label == "更新待ち")
        );
        let uncertain = TaskActivitySummary {
            unknown: 1,
            ..Default::default()
        }
        .display();
        assert!(uncertain.ongoing);
        assert!(!uncertain.can_start);
    }

    #[test]
    fn completion_remains_visible_even_if_the_system_marks_the_activity_stale() {
        let display = TaskActivitySummary::default().display();
        assert!(!display.ongoing && !display.can_start && display.urgent);
        assert_eq!(display.current.label, "すべてのタスクが終了");
        assert_eq!(
            display.current.icons[0].kind,
            TaskActivityIconKind::Finished
        );
        assert_eq!(display.current.icons[0].label, display.current.label);
        assert_eq!(display.stale, display.current);
    }
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
        fn display_is_bounded_and_accounts_for_every_task(running in 0u32..500, waiting in 0u32..500, unknown in 0u32..500) {
            let display = TaskActivitySummary { running, waiting, unknown }.display();
            let total = running + waiting + unknown;
            proptest::prop_assert_eq!(display.current.total, total);
            proptest::prop_assert_eq!(display.ongoing, total > 0);
            proptest::prop_assert_eq!(display.can_start, running > 0 || waiting > 0);
            proptest::prop_assert_eq!(display.urgent, waiting > 0 || total == 0);
            proptest::prop_assert!(display.current.icons.len() <= 12);
            proptest::prop_assert_eq!(display.current.overflow, total.saturating_sub(12));
            proptest::prop_assert_eq!(display.stale.total, total);
            proptest::prop_assert_eq!(display.stale.overflow, display.current.overflow);
            if total > 0 {
                proptest::prop_assert_eq!(display.current.icons.len() as u32 + display.current.overflow, total);
            } else {
                proptest::prop_assert_eq!(display.current.icons.len(), 1);
                proptest::prop_assert_eq!(&display.current, &display.stale);
            }
        }
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
