//! Thread list actions shown at once while their commands are pending. The
//! Host's row replaces the preview once the shell reaches the command's
//! sequence; a rejection removes it.
use super::build::LifecycleAction;
use agent_domain::{RunStatus, ThreadShell, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleOverlay {
    Settle,
    Unsettle,
    Snooze { until: Timestamp },
    Unsnooze,
    AutoSettle { enabled: bool },
    Pin { order: Option<String> },
    Unpin,
    ReorderPinned { order: String },
    ReorderActive { order: String },
}

impl LifecycleOverlay {
    pub fn of(action: &LifecycleAction) -> Option<Self> {
        Some(match action {
            LifecycleAction::Settle => Self::Settle,
            LifecycleAction::Unsettle => Self::Unsettle,
            LifecycleAction::Snooze { until } => Self::Snooze {
                until: until.clone(),
            },
            LifecycleAction::Unsnooze => Self::Unsnooze,
            LifecycleAction::AutoSettle { enabled } => Self::AutoSettle { enabled: *enabled },
            LifecycleAction::Pin { order } => Self::Pin {
                order: order.clone(),
            },
            LifecycleAction::Unpin => Self::Unpin,
            LifecycleAction::ReorderPinned { order } => Self::ReorderPinned {
                order: order.clone(),
            },
            LifecycleAction::ReorderActive { order } => Self::ReorderActive {
                order: order.clone(),
            },
            _ => return None,
        })
    }

    /// Before the Host accepts it, a settle or snooze the Host would refuse
    /// (pending request or active work) shows nothing.
    pub fn apply(&self, thread: &mut ThreadShell, now: &Timestamp, accepted: bool) {
        let busy = |statuses: &[RunStatus]| {
            thread.pending_request.is_some()
                || thread
                    .status
                    .is_some_and(|status| statuses.contains(&status))
        };
        match self {
            Self::Settle => {
                if !accepted
                    && busy(&[
                        RunStatus::Preparing,
                        RunStatus::Queued,
                        RunStatus::Starting,
                        RunStatus::Running,
                        RunStatus::Waiting,
                    ])
                {
                    return;
                }
                thread.pending_request = None;
                thread.settled_at = if thread.settled == Some(true) {
                    thread.settled_at.clone().or_else(|| Some(now.clone()))
                } else {
                    Some(now.clone())
                };
                thread.settled = Some(true);
                thread.active_order = None;
                thread.pinned_at = None;
                thread.pin_order = None;
                thread.snoozed_at = None;
                thread.snoozed_until = None;
            }
            Self::Unsettle => {
                thread.settled = Some(false);
                thread.settled_at = None;
            }
            Self::Snooze { until } => {
                if (!accepted
                    && busy(&[RunStatus::Preparing, RunStatus::Queued, RunStatus::Starting]))
                    || until <= now
                {
                    return;
                }
                thread.pending_request = None;
                thread.snoozed_at = if thread.snoozed_until.as_ref() == Some(until) {
                    thread.snoozed_at.clone().or_else(|| Some(now.clone()))
                } else {
                    Some(now.clone())
                };
                thread.snoozed_until = Some(until.clone());
            }
            Self::Unsnooze => {
                thread.snoozed_until = None;
                thread.snoozed_at = None;
            }
            Self::AutoSettle { enabled } => thread.auto_settle = *enabled,
            Self::Pin { order } => {
                if thread.pinned_at.is_none() {
                    thread.pin_order = order.clone();
                    thread.pinned_at = Some(now.clone());
                }
                if thread.settled == Some(true) {
                    thread.settled = Some(false);
                    thread.settled_at = None;
                }
                thread.snoozed_until = None;
                thread.snoozed_at = None;
            }
            Self::Unpin => {
                thread.pinned_at = None;
                thread.pin_order = None;
            }
            Self::ReorderPinned { order } => thread.pin_order = Some(order.clone()),
            Self::ReorderActive { order } => thread.active_order = Some(order.clone()),
        }
    }
}
