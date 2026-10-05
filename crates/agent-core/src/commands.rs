//! Pure conversion of native actions into T3 domain commands.
use crate::state::{Draft, QuestionAnswer, ThreadAction};
use orchestration::*;

pub fn thread_action(action: &ThreadAction, now: &Timestamp) -> Result<CommandBody, String> {
    Ok(match action {
        ThreadAction::Pin => CommandBody::ThreadPin { order_key: None },
        ThreadAction::Unpin => CommandBody::ThreadUnpin,
        ThreadAction::Settle => CommandBody::ThreadSettle {
            settled_at: Some(now.clone()),
        },
        ThreadAction::Unsettle => CommandBody::ThreadUnsettle,
        ThreadAction::Snooze { until } => CommandBody::ThreadSnooze {
            snoozed_until: Timestamp::parse(until).map_err(|e| e.to_string())?,
        },
        ThreadAction::Unsnooze => CommandBody::ThreadUnsnooze,
        ThreadAction::Rename { title } => CommandBody::ThreadMetadataUpdate {
            title: title.clone(),
        },
        ThreadAction::MarkUnread => CommandBody::ThreadMarkUnread,
        ThreadAction::AutoSettle { enabled } => {
            CommandBody::ThreadAutoSettleSet { enabled: *enabled }
        }
        ThreadAction::Archive => CommandBody::ThreadArchive,
        ThreadAction::Unarchive => CommandBody::ThreadUnarchive,
        ThreadAction::Delete => CommandBody::ThreadDelete,
        ThreadAction::PinReorder { order_key } => CommandBody::ThreadPinReorder {
            order_key: order_key.clone(),
        },
        ThreadAction::ActiveReorder { order_key } => CommandBody::ThreadActiveReorder {
            order_key: order_key.clone(),
        },
    })
}
pub fn runtime_mode(value: &str) -> Result<RuntimeMode, String> {
    match value {
        "" | "full-access" => Ok(RuntimeMode::FullAccess),
        "approval-required" => Ok(RuntimeMode::ApprovalRequired),
        "auto-accept-edits" => Ok(RuntimeMode::AutoAcceptEdits),
        "auto" => Ok(RuntimeMode::Auto),
        _ => Err("Unknown runtime mode".into()),
    }
}
pub fn interaction_mode(value: &str) -> Result<InteractionMode, String> {
    match value {
        "" | "default" => Ok(InteractionMode::Default),
        "plan" => Ok(InteractionMode::Plan),
        _ => Err("Unknown interaction mode".into()),
    }
}
pub fn approval_decision(value: &str) -> Result<ApprovalDecision, String> {
    match value {
        "accept" => Ok(ApprovalDecision::Accept),
        "acceptForSession" => Ok(ApprovalDecision::AcceptForSession),
        "acceptAlways" => Ok(ApprovalDecision::AcceptAlways),
        "decline" => Ok(ApprovalDecision::Decline),
        "cancel" => Ok(ApprovalDecision::Cancel),
        _ => Err("Unknown approval decision".into()),
    }
}
pub fn question_answers(answers: &[QuestionAnswer]) -> Answers {
    answers
        .iter()
        .map(|a| {
            (
                a.question_id.clone(),
                Json(if a.values.len() == 1 {
                    serde_json::Value::String(a.values[0].clone())
                } else {
                    serde_json::Value::Array(
                        a.values
                            .iter()
                            .cloned()
                            .map(serde_json::Value::String)
                            .collect(),
                    )
                }),
            )
        })
        .collect()
}
pub fn message(
    draft: &Draft,
    message_id: MessageId,
    dispatch_mode: DispatchMode,
    source: CreationSource,
) -> Result<MessageDispatch, String> {
    if draft.text.trim().is_empty() {
        return Err("Enter a message".into());
    }
    Ok(MessageDispatch {
        native_continuation: None,
        delegated_completion: None,
        source_plan_ref: None,
        created_by: CreatedBy::User,
        creation_source: source,
        message_id,
        text: draft.text.clone(),
        context: None,
        attachments: vec![],
        model_selection: Some(draft.selection()?),
        delivery_intent: None,
        dispatch_mode,
    })
}

pub fn plan_follow_up(
    draft_text: &str,
    markdown: &str,
    new_thread: bool,
) -> (String, InteractionMode, bool) {
    if new_thread || draft_text.trim().is_empty() {
        (
            format!("PLEASE IMPLEMENT THIS PLAN:\n{}", markdown.trim()),
            InteractionMode::Default,
            true,
        )
    } else {
        (draft_text.trim().into(), InteractionMode::Plan, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plan_follow_up_matches_t3_implement_refine_and_new_thread() {
        assert_eq!(
            plan_follow_up(" ", " # Plan ", false),
            (
                "PLEASE IMPLEMENT THIS PLAN:\n# Plan".into(),
                InteractionMode::Default,
                true
            )
        );
        assert_eq!(
            plan_follow_up(" Add tests ", "# Plan", false),
            ("Add tests".into(), InteractionMode::Plan, false)
        );
        assert_eq!(
            plan_follow_up("draft", "# Plan", true),
            (
                "PLEASE IMPLEMENT THIS PLAN:\n# Plan".into(),
                InteractionMode::Default,
                true
            )
        );
    }
}
