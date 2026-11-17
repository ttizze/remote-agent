//! The same timeline decisions used by the desktop, exported to native bindings.
use super::*;
use crate::presentation::conversation::{RenderedTurn, turn_rows};

pub(super) fn project_timeline(
    style: super::super::TimelineStyle,
    turn: &RenderedTurn,
    origin: &ConversationRowOrigin,
    prefix: &str,
    expansion: &HashMap<String, String>,
    previous: &PreviousRows<'_>,
) -> Vec<Arc<VisibleConversationRow>> {
    let prefix = format!("{prefix}:{}", turn.key());
    let choice_prefix = super::row_id(&prefix, origin, "");
    let expansion: HashMap<_, _> = expansion
        .iter()
        .filter_map(|(id, status)| {
            id.strip_prefix(&choice_prefix)
                .map(|id| (id.to_owned(), status.clone()))
        })
        .collect();
    let groups = expansion
        .iter()
        .filter(|(_, value)| value.as_str() == "open")
        .map(|(id, _)| id.clone())
        .collect();
    let timeline = turn_rows(style, turn, &expansion, &groups);
    let last = timeline.len().checked_sub(1);
    timeline
        .into_iter()
        .enumerate()
        .map(|(index, timeline)| {
            let data = data(&timeline, &turn.source.id, &turn.rows, index);
            let expanded = matches!(
                &timeline,
                TurnRow::Fold { open: true, .. }
                    | TurnRow::Group { open: true, .. }
                    | TurnRow::Live { open: true, .. }
            );
            let progress = matches!(timeline, TurnRow::Working).then(|| turn.work_progress());
            project_row(
                &prefix,
                origin,
                RowInput {
                    data: &data,
                    expanded,
                    progress,
                    timeline: Some(timeline),
                },
                Some(index) == last,
                previous,
            )
        })
        .collect()
}

pub(super) fn data(
    timeline: &TurnRow,
    turn: &TurnId,
    source: &[ConversationRow],
    position: usize,
) -> ConversationRow {
    let (id, content) = match timeline {
        TurnRow::Row(row) | TurnRow::Work { row, .. } => {
            return row.clone();
        }
        TurnRow::Live { row, group_id, .. } => {
            return ConversationRow {
                id: group_id.clone(),
                ..row.clone()
            };
        }
        TurnRow::Actions(row) => {
            return ConversationRow {
                id: format!("actions:{}", row.id),
                ..row.clone()
            };
        }
        TurnRow::Commentary(item) => {
            return source.iter().find(|row| matches!(&row.content,
                ConversationRowContent::Activity { item: original, .. } if std::sync::Arc::ptr_eq(original, item)))
                .cloned().expect("timeline commentary originates in this turn's canonical rows");
        }
        TurnRow::Fold { activity, .. } => (
            activity.id.clone(),
            ConversationRowContent::ActivityHeader {
                activity: activity.clone(),
            },
        ),
        TurnRow::Working => (
            "working".into(),
            ConversationRowContent::InProgress {
                turn_id: turn.clone(),
            },
        ),
        TurnRow::Group { id, summary, .. } => (
            id.clone(),
            ConversationRowContent::ProviderHandoff {
                text: summary.clone(),
            },
        ),
        TurnRow::Subagent { id, agent, .. } => (
            id.clone(),
            ConversationRowContent::ProviderHandoff {
                text: agent.detail(agent.status).unwrap_or_default(),
            },
        ),
        TurnRow::Divider { id, label } => (
            id.clone(),
            ConversationRowContent::ProviderHandoff {
                text: label.clone(),
            },
        ),
        TurnRow::Plan { id, .. } => {
            return source
                .iter()
                .find(|row| {
                    matches!(&row.content,
                ConversationRowContent::Activity { item, .. } if &item.data.id == id)
                })
                .cloned()
                .expect("plan rows retain their source document");
        }
        TurnRow::Subagents { id, label, .. } => (
            id.clone(),
            ConversationRowContent::ProviderHandoff {
                text: label.clone(),
            },
        ),
        TurnRow::Thinking => (
            "thinking".into(),
            ConversationRowContent::ProviderHandoff {
                text: String::new(),
            },
        ),
        TurnRow::Requested { label, .. } => (
            format!("request:{position}"),
            ConversationRowContent::ProviderHandoff {
                text: label.clone(),
            },
        ),
        TurnRow::Retrying { label } => (
            "retry".into(),
            ConversationRowContent::ProviderHandoff {
                text: label.clone(),
            },
        ),
        TurnRow::Interrupted => (
            "interrupted".into(),
            ConversationRowContent::ProviderHandoff {
                text: String::new(),
            },
        ),
    };
    ConversationRow {
        id,
        content,
        timestamp_ms: None,
        steer: false,
        queued: false,
        edit_turn_id: None,
        fresh_context: false,
    }
}
