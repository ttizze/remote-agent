//! Messages the device sent and the thread has not folded, drawn after the
//! presented feed in send order.
use super::mobile::FeedRow;
use crate::commands::outbox::PendingMessage;
use std::collections::BTreeSet;

/// Appends the undelivered messages after all presented rows until the
/// thread folds each one. A send waiting behind the active run belongs to the
/// queue, not the feed.
pub fn append_pending_messages(
    mut presented: Vec<FeedRow>,
    feed: &[FeedRow],
    pending: &[PendingMessage],
) -> Vec<FeedRow> {
    // Folded messages count as delivered even when the presentation hides them.
    let delivered: BTreeSet<&str> = feed
        .iter()
        .filter_map(|row| match row {
            FeedRow::Message { message, .. } => Some(message.id.as_str()),
            _ => None,
        })
        .collect();
    presented.extend(
        pending
            .iter()
            .filter(|message| !message.queued && !delivered.contains(message.id.as_str()))
            .cloned()
            .map(FeedRow::PendingMessage),
    );
    presented
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::outbox::Phase;
    use crate::view::timeline::entries::ChatMessage;
    use agent_domain::{CommandId, MessageContext, MessageId, Role, ThreadId, Timestamp};

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
    }

    fn pending(id: &str) -> PendingMessage {
        PendingMessage {
            command: CommandId::new(id).unwrap(),
            thread: ThreadId::new("thread").unwrap(),
            id: MessageId::new(id).unwrap(),
            text: id.into(),
            attachments: vec![],
            context: None,
            queued: false,
            created_at: at("2026-09-06T10:00:00.000Z"),
            phase: Phase::Queued,
        }
    }

    fn delivered(message: &PendingMessage) -> FeedRow {
        FeedRow::Message {
            id: message.id.to_string(),
            created_at: message.created_at.clone(),
            message: ChatMessage {
                id: message.id.clone(),
                role: Role::User,
                text: message.text.clone(),
                attachments: vec![],
                context: None,
                run: None,
                streaming: false,
                created_by: None,
                creation_source: None,
                created_at: message.created_at.clone(),
                updated_at: message.created_at.clone(),
                input_intent: None,
            },
            item: None,
        }
    }

    #[test]
    fn retains_context_records_while_a_message_is_waiting_for_delivery() {
        let context = MessageContext {
            version: 1,
            records: vec![agent_domain::Json(serde_json::json!({
                "version": 1, "kind": "mention", "contextId": "setup-file",
                "label": "Checkout.tsx", "path": "src/Checkout.tsx",
            }))],
        };
        let text = "[Checkout.tsx](context://v1/mention/setup-file)";
        let message = PendingMessage {
            text: text.into(),
            context: Some(context.clone()),
            ..pending("context")
        };
        let rows = append_pending_messages(vec![], &[], &[message]);
        let [FeedRow::PendingMessage(row)] = rows.as_slice() else {
            panic!("expected a pending message: {rows:?}");
        };
        assert_eq!(row.text, text);
        assert_eq!(row.context, Some(context));
    }

    #[test]
    fn keeps_pending_messages_after_newer_agent_activity_in_queue_order() {
        let activity = FeedRow::Thinking {
            id: "thinking".into(),
            created_at: at("2026-09-06T11:00:00.000Z"),
            run: None,
            continues_work_log: false,
        };
        let rows =
            append_pending_messages(vec![activity], &[], &[pending("first"), pending("second")]);
        assert_eq!(
            rows.iter().map(FeedRow::id).collect::<Vec<_>>(),
            ["thinking", "first", "second"]
        );
        assert!(matches!(&rows[1], FeedRow::PendingMessage(message) if message.text == "first"));
    }

    #[test]
    fn reuses_the_message_id_and_suppresses_the_pending_copy_when_delivery_appears() {
        let queued = pending("sent");
        let optimistic = append_pending_messages(vec![], &[], std::slice::from_ref(&queued));
        assert_eq!(optimistic[0].id(), "sent");
        let delivered = delivered(&queued);
        assert_eq!(
            append_pending_messages(
                vec![delivered.clone()],
                std::slice::from_ref(&delivered),
                std::slice::from_ref(&queued)
            ),
            vec![delivered.clone()]
        );
        // Folded messages still count as delivered even when absent from the presented rows.
        assert_eq!(append_pending_messages(vec![], &[delivered], &[queued]), []);
    }

    #[test]
    fn leaves_sends_waiting_behind_the_active_run_to_the_queue() {
        let queued = PendingMessage {
            queued: true,
            ..pending("queued")
        };
        assert_eq!(append_pending_messages(vec![], &[], &[queued]), []);
    }
}
