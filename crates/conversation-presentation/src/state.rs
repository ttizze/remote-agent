//! Shared event interpretation and state-transition policy. Native owners apply
//! the returned mutation to their own bodies; no history or body crosses FFI.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum EventKind {
    #[default]
    Unknown = 0,
    TurnStarted = 1,
    TurnCompleted = 2,
    ItemStarted = 3,
    ItemCompleted = 4,
    AgentMessageDelta = 5,
    ReasoningDelta = 6,
    ReasoningSummaryDelta = 7,
    CommandOutputDelta = 8,
    FileChangeOutputDelta = 9,
    Error = 10,
    RequestStarted = 11,
    RequestResolved = 12,
    ThreadStatusChanged = 13,
    GuardianReviewChanged = 14,
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

#[derive(Default)]
pub struct EventMetadata<'a> {
    pub kind: EventKind,
    pub status: &'a str,
    pub has_error: bool,
    pub will_retry: bool,
    pub empty_delta: bool,
}

#[derive(Default)]
pub struct CurrentMetadata<'a> {
    pub turn_status: Option<&'a str>,
    pub item_type: Option<&'a str>,
    pub retrying_error: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum Mutation {
    #[default]
    Ignore = 0,
    ThreadStatus = 1,
    Turn = 2,
    Item = 3,
    RemoveItem = 4,
    Append = 5,
    Error = 6,
    Request = 7,
    ResolveRequest = 8,
}

#[derive(Debug, Default)]
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

/// Compact C/JNI transition ABI. Inputs: event kind; status (0 absent, 1 active,
/// 2 completed, 3 failed, 4 interrupted, 5 approved); item (0 absent/unknown,
/// 1 agentMessage, 2 reasoning, 3 commandExecution, 4 fileChange). Flags:
/// 1 hasError, 2 willRetry, 4 emptyDelta, 8 current retryingError.
/// Output: action in bits 0..7, status in 8..15, clearError in bit 16.
pub fn transition_code(kind: u32, status: u32, current_status: u32, item: u32, flags: u32) -> u32 {
    const KINDS: [EventKind; 15] = [
        EventKind::Unknown,
        EventKind::TurnStarted,
        EventKind::TurnCompleted,
        EventKind::ItemStarted,
        EventKind::ItemCompleted,
        EventKind::AgentMessageDelta,
        EventKind::ReasoningDelta,
        EventKind::ReasoningSummaryDelta,
        EventKind::CommandOutputDelta,
        EventKind::FileChangeOutputDelta,
        EventKind::Error,
        EventKind::RequestStarted,
        EventKind::RequestResolved,
        EventKind::ThreadStatusChanged,
        EventKind::GuardianReviewChanged,
    ];
    let status_text = |code| match code {
        1 => "inProgress",
        2 => "completed",
        3 => "failed",
        4 => "interrupted",
        5 => "approved",
        _ => "",
    };
    let decision = transition(
        &EventMetadata {
            kind: KINDS.get(kind as usize).copied().unwrap_or_default(),
            status: status_text(status),
            has_error: flags & 1 != 0,
            will_retry: flags & 2 != 0,
            empty_delta: flags & 4 != 0,
        },
        &CurrentMetadata {
            turn_status: (1..=4)
                .contains(&current_status)
                .then(|| status_text(current_status)),
            item_type: match item {
                1 => Some("agentMessage"),
                2 => Some("reasoning"),
                3 => Some("commandExecution"),
                4 => Some("fileChange"),
                _ => None,
            },
            retrying_error: flags & 8 != 0,
        },
    );
    let status = match decision.status {
        Some("inProgress") => 1,
        Some("completed") => 2,
        Some("failed") => 3,
        Some("interrupted") => 4,
        _ => 0,
    };
    decision.action as u32 | status << 8 | u32::from(decision.clear_error) << 16
}

/// Moves turn values through the shared lifecycle reducer. FFI adapters supply
/// item identity/source tokens instead of bodies; desktop supplies owned items.
pub fn merge_lifecycle(
    mut previous: serde_json::Value,
    mut incoming: serde_json::Value,
    kind: EventKind,
) -> serde_json::Value {
    use serde_json::{Value, json};
    let decision = transition(
        &EventMetadata {
            kind,
            status: crate::text(&incoming, "status"),
            has_error: incoming.get("error").is_some_and(|error| !error.is_null()),
            ..Default::default()
        },
        &CurrentMetadata {
            turn_status: previous["status"].as_str(),
            retrying_error: previous["error"]["willRetry"] == true,
            ..Default::default()
        },
    );
    if !previous.is_object() {
        previous = json!({"items":[]});
    }
    let preserve = matches!(crate::text(&incoming, "itemsView"), "summary" | "notLoaded")
        || crate::array(&incoming["items"]).is_empty();
    let items = incoming
        .as_object_mut()
        .and_then(|fields| fields.remove("items"));
    if let Value::Object(fields) = incoming {
        for (key, value) in fields {
            if key != "status"
                && !(value.is_null()
                    && matches!(
                        key.as_str(),
                        "error" | "startedAt" | "completedAt" | "durationMs"
                    ))
            {
                previous[key] = value;
            }
        }
    }
    if let Some(Value::Array(items)) = items {
        if let Some(deferred) = previous
            .get_mut("deferredItemIds")
            .and_then(Value::as_array_mut)
        {
            deferred.retain(|id| !items.iter().any(|item| item["id"] == *id));
        }
        if preserve {
            if !previous["items"].is_array() {
                previous["items"] = json!([]);
            }
            let existing = previous["items"].as_array_mut().unwrap();
            for item in items {
                if let Some(old) = existing.iter_mut().find(|old| old["id"] == item["id"]) {
                    *old = item;
                } else {
                    existing.push(item);
                }
            }
        } else {
            previous["items"] = Value::Array(items);
        }
    }
    previous["status"] = json!(decision.status.expect("lifecycle event"));
    if decision.clear_error {
        previous.as_object_mut().unwrap().remove("error");
    }
    previous
}

#[derive(Debug, serde::Serialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SendPlan {
    Steer { turn_id: String },
    Queue,
    Start { cwd: String, resume: bool },
    Reject { message: &'static str },
}

/// Agent routing uses the loaded snapshot first, then catalogue metadata. A
/// globally active thread without a loaded active turn queues the message.
pub fn plan_send(snapshot: &serde_json::Value, listed: &serde_json::Value) -> SendPlan {
    use crate::{array, text};
    if let Some(turn) = array(&snapshot["turns"])
        .iter()
        .rev()
        .find(|turn| turn["status"] == "inProgress" && !text(turn, "id").trim().is_empty())
    {
        return SendPlan::Steer {
            turn_id: text(turn, "id").to_owned(),
        };
    }
    if snapshot["status"]["type"] == "active" || listed["status"]["type"] == "active" {
        return SendPlan::Queue;
    }
    let cwd = [snapshot, listed]
        .into_iter()
        .map(|thread| text(thread, "cwd"))
        .find(|cwd| !cwd.trim().is_empty());
    match cwd {
        Some(cwd) => SendPlan::Start {
            cwd: cwd.to_owned(),
            resume: snapshot.is_null() || snapshot["status"]["type"] == "notLoaded",
        },
        None => SendPlan::Reject {
            message: "タスクの作業ディレクトリが不明です。タスク一覧を更新してください",
        },
    }
}

/// Only an unloaded persisted conversation needs the Host filesystem watch.
pub fn watch_path(thread: &serde_json::Value) -> Option<&str> {
    (thread["status"]["type"] == "notLoaded")
        .then(|| crate::text(thread, "path"))
        .filter(|path| !path.trim().is_empty())
}
