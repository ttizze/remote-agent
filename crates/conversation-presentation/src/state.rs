//! Shared event interpretation and state-transition policy. Native owners apply
//! the returned mutation to their own bodies; no history or body crosses FFI.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    #[default]
    Unknown,
    TurnStarted,
    TurnCompleted,
    ItemStarted,
    ItemCompleted,
    AgentMessageDelta,
    ReasoningDelta,
    ReasoningSummaryDelta,
    CommandOutputDelta,
    FileChangeOutputDelta,
    Error,
    RequestStarted,
    RequestResolved,
    ThreadStatusChanged,
    GuardianReviewChanged,
}

pub fn classify_event(method: &str, is_request: bool) -> EventKind {
    use EventKind::*;
    if is_request {
        return RequestStarted;
    }
    match method {
        "turn/started" => TurnStarted,
        "turn/completed" => TurnCompleted,
        "item/started" => ItemStarted,
        "item/completed" => ItemCompleted,
        "item/agentMessage/delta" => AgentMessageDelta,
        "item/reasoning/textDelta" => ReasoningDelta,
        "item/reasoning/summaryTextDelta" => ReasoningSummaryDelta,
        "item/commandExecution/outputDelta" => CommandOutputDelta,
        "item/fileChange/outputDelta" => FileChangeOutputDelta,
        "error" => Error,
        "serverRequest/resolved" => RequestResolved,
        "thread/status/changed" => ThreadStatusChanged,
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
            GuardianReviewChanged
        }
        _ => Unknown,
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMetadata<'a> {
    pub kind: EventKind,
    #[serde(default)]
    pub status: &'a str,
    #[serde(default)]
    pub has_error: bool,
    #[serde(default)]
    pub will_retry: bool,
    #[serde(default)]
    pub empty_delta: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMetadata<'a> {
    pub turn_status: Option<&'a str>,
    pub item_type: Option<&'a str>,
    #[serde(default)]
    pub retrying_error: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Mutation {
    #[default]
    Ignore,
    ThreadStatus,
    Turn,
    Item,
    RemoveItem,
    Append,
    Error,
    Request,
    ResolveRequest,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub action: Mutation,
    pub status: Option<&'static str>,
    pub field: Option<&'static str>,
    pub clear_error: bool,
}

fn turn_status(status: &str) -> &'static str {
    match status {
        "inProgress" => "inProgress",
        "interrupted" => "interrupted",
        "failed" => "failed",
        _ => "completed",
    }
}

pub fn transition(event: &EventMetadata<'_>, current: &CurrentMetadata<'_>) -> Transition {
    use EventKind::*;
    let action = match event.kind {
        Unknown => Mutation::Ignore,
        ThreadStatusChanged => Mutation::ThreadStatus,
        RequestResolved => Mutation::ResolveRequest,
        RequestStarted => Mutation::Request,
        TurnStarted | TurnCompleted => {
            let incoming = if event.kind == TurnStarted {
                "inProgress"
            } else {
                turn_status(event.status)
            };
            let status = match current.turn_status {
                Some(previous) if previous != "inProgress" && incoming == "inProgress" => {
                    turn_status(previous)
                }
                _ => incoming,
            };
            return Transition {
                action: Mutation::Turn,
                status: Some(status),
                clear_error: !event.has_error
                    && (incoming == "completed"
                        || incoming != "inProgress" && current.retrying_error),
                ..Default::default()
            };
        }
        // Item events cannot manufacture a turn outside the loaded history.
        _ if current.turn_status.is_none() => Mutation::Ignore,
        ItemStarted | ItemCompleted => Mutation::Item,
        GuardianReviewChanged => {
            if event.status == "approved" {
                Mutation::RemoveItem
            } else {
                Mutation::Item
            }
        }
        Error => {
            if event.will_retry && current.turn_status != Some("inProgress") {
                Mutation::Ignore
            } else {
                Mutation::Error
            }
        }
        AgentMessageDelta
        | ReasoningDelta
        | ReasoningSummaryDelta
        | CommandOutputDelta
        | FileChangeOutputDelta => {
            let (kind, field) = match event.kind {
                AgentMessageDelta => ("agentMessage", "text"),
                ReasoningDelta | ReasoningSummaryDelta => ("reasoning", "summary"),
                CommandOutputDelta => ("commandExecution", "aggregatedOutput"),
                FileChangeOutputDelta => ("fileChange", "diff"),
                _ => unreachable!(),
            };
            return Transition {
                action: if !event.empty_delta && current.item_type == Some(kind) {
                    Mutation::Append
                } else {
                    Mutation::Ignore
                },
                field: Some(field),
                ..Default::default()
            };
        }
    };
    Transition {
        action,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_precedence_and_transient_errors_are_consistent() {
        let current = CurrentMetadata {
            turn_status: Some("completed"),
            retrying_error: true,
            ..Default::default()
        };
        let late_start = transition(
            &EventMetadata {
                kind: EventKind::TurnStarted,
                ..Default::default()
            },
            &current,
        );
        assert_eq!(late_start.status, Some("completed"));
        let late_error = transition(
            &EventMetadata {
                kind: EventKind::Error,
                will_retry: true,
                ..Default::default()
            },
            &current,
        );
        assert_eq!(late_error.action, Mutation::Ignore);
        let completed = transition(
            &EventMetadata {
                kind: EventKind::TurnCompleted,
                status: "completed",
                ..Default::default()
            },
            &current,
        );
        assert!(completed.clear_error);
        let failed = transition(
            &EventMetadata {
                kind: EventKind::TurnCompleted,
                status: "failed",
                has_error: true,
                ..Default::default()
            },
            &current,
        );
        assert!(!failed.clear_error);
    }
    #[test]
    fn item_updates_require_a_known_turn_and_deltas_require_the_right_item() {
        let event = EventMetadata {
            kind: EventKind::ItemStarted,
            ..Default::default()
        };
        assert_eq!(
            transition(&event, &CurrentMetadata::default()).action,
            Mutation::Ignore
        );
        let current = CurrentMetadata {
            turn_status: Some("inProgress"),
            item_type: Some("reasoning"),
            ..Default::default()
        };
        let event = EventMetadata {
            kind: EventKind::AgentMessageDelta,
            ..Default::default()
        };
        assert_eq!(transition(&event, &current).action, Mutation::Ignore);
        let event = EventMetadata {
            kind: EventKind::ReasoningSummaryDelta,
            ..Default::default()
        };
        assert_eq!(transition(&event, &current).action, Mutation::Append);
    }
    #[test]
    fn automatic_review_rejections_stay_visible_and_approval_removes_them() {
        let current = CurrentMetadata {
            turn_status: Some("inProgress"),
            ..Default::default()
        };
        for method in [
            "item/autoApprovalReview/started",
            "item/autoApprovalReview/completed",
        ] {
            let event = EventMetadata {
                kind: classify_event(method, false),
                status: "denied",
                ..Default::default()
            };
            assert_eq!(transition(&event, &current).action, Mutation::Item);
            assert_eq!(
                transition(
                    &EventMetadata {
                        status: "approved",
                        ..event
                    },
                    &current
                )
                .action,
                Mutation::RemoveItem
            );
        }
    }
}
