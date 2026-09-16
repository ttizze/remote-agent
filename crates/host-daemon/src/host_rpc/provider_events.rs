//! Provider protocol decoding belongs to the Host boundary, never the Store.
use agent_core::{
    models::{Item, ThreadStatus, Turn},
    session::{SessionChange, TextField},
};
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationParams {
    thread_id: String,
    turn_id: Option<String>,
    turn: Option<Turn>,
    item: Option<Item>,
    item_id: Option<String>,
    status: Option<ThreadStatus>,
    delta: Option<String>,
    error: Option<Value>,
    #[serde(default)]
    will_retry: bool,
    review_id: Option<String>,
    review: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// Decode the existing adapters' notification vocabulary once, at the boundary.
/// Unknown notifications remain available to their non-conversation owners.
pub(super) fn notification_change(
    method: &str,
    value: Value,
) -> Result<Option<(String, SessionChange)>, serde_json::Error> {
    if !matches!(
        method,
        "thread/status/changed"
            | "turn/started"
            | "turn/completed"
            | "item/started"
            | "item/completed"
            | "item/autoApprovalReview/started"
            | "item/autoApprovalReview/completed"
            | "item/agentMessage/delta"
            | "item/reasoning/textDelta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "error"
    ) {
        return Ok(None);
    }
    let params: NotificationParams = serde_json::from_value(value)?;
    if method == "thread/status/changed" {
        return Ok(params
            .status
            .map(|status| (params.thread_id, SessionChange::Status { status })));
    }
    let Some(turn_id) = params
        .turn
        .as_ref()
        .map(|turn| &turn.id)
        .or(params.turn_id.as_ref())
        .filter(|id| !id.is_empty())
        .cloned()
    else {
        return Ok(None);
    };
    let change = match method {
        "turn/started" | "turn/completed" => params.turn.map(|turn| SessionChange::Turn {
            turn,
            completed: method == "turn/completed",
        }),
        "item/started" | "item/completed" => params.item.map(|item| SessionChange::Item {
            turn_id: turn_id.clone(),
            item,
        }),
        "item/autoApprovalReview/started" | "item/autoApprovalReview/completed" => {
            params.review_id.map(|id| {
                if params
                    .review
                    .as_ref()
                    .is_some_and(|review| review["status"] == "approved")
                {
                    SessionChange::RemoveItem {
                        turn_id: turn_id.clone(),
                        item_id: id,
                    }
                } else {
                    let mut extra = params.extra;
                    extra.insert("threadId".into(), Value::String(params.thread_id.clone()));
                    extra.insert("turnId".into(), Value::String(turn_id.clone()));
                    if let Some(review) = params.review {
                        extra.insert("review".into(), review);
                    }
                    SessionChange::Item {
                        turn_id: turn_id.clone(),
                        item: Item {
                            id,
                            kind: Some("automaticApprovalReview".into()),
                            extra,
                            ..Default::default()
                        },
                    }
                }
            })
        }
        "item/agentMessage/delta"
        | "item/reasoning/textDelta"
        | "item/reasoning/summaryTextDelta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta" => {
            params
                .item_id
                .zip(params.delta)
                .map(|(item_id, delta)| SessionChange::Text {
                    turn_id: turn_id.clone(),
                    item_id,
                    delta,
                    field: match method {
                        "item/agentMessage/delta" => TextField::Message,
                        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                            TextField::Reasoning
                        }
                        "item/commandExecution/outputDelta" => TextField::CommandOutput,
                        "item/fileChange/outputDelta" => TextField::FileChange,
                        _ => unreachable!(),
                    },
                })
        }
        "error" => Some(SessionChange::Error {
            turn_id: turn_id.clone(),
            error: params.error.unwrap_or(Value::Null),
            will_retry: params.will_retry,
        }),
        _ => None,
    };
    Ok(change.map(|change| (params.thread_id, change)))
}
