//! Translation of T3 4ee6bfd, orchestration-v2/QueuedRunOrder.ts.
use crate::contracts::*;
use std::collections::BTreeSet;

pub fn is_automatic_completion_run(messages: &[ConversationMessage], run: &Run) -> bool {
    messages
        .iter()
        .any(|message| message.id == run.user_message_id && message.delegated_completion.is_some())
}
pub fn queued_runs_in_delivery_order<'a>(
    runs: &'a [Run],
    messages: &[ConversationMessage],
) -> Vec<&'a Run> {
    let automatic_completion_message_ids: BTreeSet<_> = messages
        .iter()
        .filter(|message| message.delegated_completion.is_some())
        .map(|message| &message.id)
        .collect();
    let mut queued: Vec<_> = runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .collect();
    queued.sort_by_key(|run| {
        (
            !automatic_completion_message_ids.contains(&run.user_message_id),
            run.queue_position.unwrap_or(run.ordinal),
            run.ordinal,
        )
    });
    queued
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    // T3 QueuedRunOrder.test.ts: keeps automatic completion delivery ahead of visible queued messages.
    #[test]
    fn keeps_automatic_completion_delivery_ahead_of_visible_queued_messages() {
        let mut p = running();
        let base_run = p.runs[0].clone();
        let base_message = p.messages[0].clone();
        p.runs.clear();
        p.messages.clear();
        for (name, ordinal, position, automatic) in [
            ("visible-first", 2, 1, false),
            ("automatic", 4, 3, true),
            ("visible-second", 3, 2, false),
        ] {
            let mut run = base_run.clone();
            run.id = RunId::new(format!("run:{name}")).unwrap();
            run.ordinal = ordinal;
            run.queue_position = Some(position);
            run.status = RunStatus::Queued;
            run.user_message_id = MessageId::new(format!("message:{name}")).unwrap();
            let mut message = base_message.clone();
            message.id = run.user_message_id.clone();
            message.delegated_completion = automatic.then(|| {
                Box::new(DelegatedCompletion {
                    generation: 1,
                    parent_run_id: RunId::new("run:parent").unwrap(),
                    task_ids: vec![NodeId::new("task:child").unwrap()],
                })
            });
            p.runs.push(run);
            p.messages.push(message);
        }
        assert_eq!(
            queued_runs_in_delivery_order(&p.runs, &p.messages)
                .iter()
                .map(|run| run.id.as_str())
                .collect::<Vec<_>>(),
            ["run:automatic", "run:visible-first", "run:visible-second"]
        );
    }
}
