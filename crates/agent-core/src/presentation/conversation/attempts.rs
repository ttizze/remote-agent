//! Fold superseded work while preserving user input, complete replies and resources.
use agent_protocol::conversation::AttemptState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptFold {
    pub id: uuid::Uuid,
    pub by_steer: bool,
    pub items: Vec<agent_protocol::ids::ItemId>,
    pub deferred: Vec<agent_protocol::ids::ItemId>,
}

fn foldable(body: &crate::models::ItemBody) -> bool {
    use crate::models::{AssistantPhase, ItemBody::*};
    matches!(
        body,
        Reasoning { .. }
            | CommandExecution { .. }
            | FileChange { .. }
            | WebSearch { .. }
            | AssistantText {
                phase: AssistantPhase::Commentary,
                ..
            }
            | ToolCall {
                resource_uri: None,
                ..
            }
    )
}

pub fn attempt_folds(turn: &crate::models::Turn) -> Vec<AttemptFold> {
    turn.attempts
        .iter()
        .filter_map(|attempt| {
            let AttemptState::Superseded { by_steer } = attempt.state else {
                return None;
            };
            let items = turn.items.as_deref().unwrap_or_default();
            let complete_reply = items.iter().any(|item| {
                item.attempt == Some(attempt.id)
                    && item.status == crate::models::ItemStatus::Completed
                    && matches!(
                        item.body(),
                        crate::models::ItemBody::AssistantText {
                            phase: crate::models::AssistantPhase::Final
                                | crate::models::AssistantPhase::Unknown,
                            ..
                        }
                    )
            });
            if complete_reply {
                return None;
            }
            let items = items
                .iter()
                .filter(|item| {
                    item.attempt == Some(attempt.id)
                        && item.status != crate::models::ItemStatus::Running
                        && foldable(item.body())
                })
                .collect::<Vec<_>>();
            if items.is_empty() {
                return None;
            }
            Some(AttemptFold {
                id: attempt.id,
                by_steer,
                deferred: items
                    .iter()
                    .filter(|item| item.is_deferred())
                    .map(|item| item.id.clone())
                    .collect(),
                items: items.iter().map(|item| item.id.clone()).collect(),
            })
        })
        .collect()
}

pub fn deferred_attempt_reads(
    thread: &crate::models::Thread,
    id: uuid::Uuid,
) -> Vec<agent_protocol::operations::ReadItem> {
    let Some(session) = &thread.id else {
        return Vec::new();
    };
    thread
        .turns
        .iter()
        .flatten()
        .flat_map(|turn| {
            attempt_folds(turn)
                .into_iter()
                .filter(|fold| fold.id == id)
                .flat_map(|fold| {
                    fold.deferred
                        .into_iter()
                        .map(|item_id| agent_protocol::operations::ReadItem {
                            thread_id: session.clone(),
                            turn_id: turn.id.clone(),
                            item_id,
                        })
                        .collect::<Vec<_>>()
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AssistantPhase, Item, ItemBody, ItemStatus, Turn};
    use agent_protocol::conversation::ExecutionAttempt;
    use std::sync::Arc;

    #[test]
    fn superseded_attempts_fold_only_old_work_and_lazy_reads_keep_the_original_item_addresses() {
        let id = uuid::Uuid::new_v4();
        let work = |name: &str, body| {
            let mut item = Item::new(name.into(), ItemStatus::Interrupted, body);
            item.attempt = Some(id);
            Arc::new(item)
        };
        let user = work(
            "user",
            ItemBody::UserMessage {
                text: Some("prompt".into()),
                content: vec![],
            },
        );
        let mut command = work(
            "command",
            ItemBody::CommandExecution {
                output_omitted: false,
                command: "echo hi".into(),
                cwd: None,
                output: String::new(),
                exit_code: None,
            },
        );
        Arc::make_mut(&mut command).defer();
        let image = work(
            "image",
            ItemBody::ImageView {
                path: "/image.png".into(),
            },
        );
        let turn = Turn {
            id: "turn".into(),
            attempts: vec![ExecutionAttempt {
                id,
                execution: id,
                native_turn: None,
                started_at_ms: 0,
                completed_at_ms: Some(1),
                state: AttemptState::Superseded { by_steer: true },
            }],
            items: Some(vec![user, command, image]),
            ..Default::default()
        };
        let folds = attempt_folds(&turn);
        assert_eq!(folds.len(), 1);
        assert!(folds[0].by_steer);
        assert_eq!(folds[0].items, vec!["command".into()]);
        let session =
            agent_protocol::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string())
                .unwrap();
        let mut thread = crate::models::Thread {
            id: Some(session.clone()),
            turns: Some(vec![Arc::new(turn)]),
            ..Default::default()
        };
        let reads = deferred_attempt_reads(&thread, id);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].thread_id, session);
        assert_eq!(reads[0].turn_id.as_str(), "turn");
        assert_eq!(reads[0].item_id.as_str(), "command");
        assert!(deferred_attempt_reads(&thread, uuid::Uuid::new_v4()).is_empty());
        let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0]);
        let mut answer = Item::new(
            "answer".into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "done".into(),
                phase: AssistantPhase::Final,
            },
        );
        answer.attempt = Some(id);
        turn.items.as_mut().unwrap().push(Arc::new(answer));
        assert!(
            attempt_folds(turn).is_empty(),
            "complete replies keep their entire attempt visible"
        );
    }
}
