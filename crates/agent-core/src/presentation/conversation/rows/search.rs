//! Search uses the same scoped identities and disclosure paths as visible rows.
use super::*;
use crate::presentation::conversation::turn_message_search_rows;

type SearchEntry = (Arc<VisibleConversationRow>, Vec<(String, String)>);

pub(super) fn messages(
    style: super::super::TimelineStyle,
    source: &RenderedConversation,
    scope: &PanelScope,
) -> Vec<SearchEntry> {
    let prefix = serde_json::to_string(&(&scope.host, &scope.storage, &scope.draft))
        .expect("conversation scope contains strings");
    let previous = HashMap::new();
    let mut messages = Vec::new();
    for turn in &source.turns {
        let origin = turn_origin(
            turn.key(),
            turn.source.id.clone(),
            turn.source.provider,
            source.source.id.as_ref().and_then(|id| id.provider),
            turn.source.started_at,
        );
        let prefix = format!("{prefix}:{}", turn.key());
        for (timeline, disclosures) in turn_message_search_rows(style, turn) {
            let data = super::timeline::data(&timeline, &turn.source.id, &turn.rows, 0);
            let row = project_row(
                &prefix,
                &origin,
                RowInput {
                    data: &data,
                    expanded: false,
                    progress: None,
                    timeline: Some(timeline),
                },
                false,
                &previous,
            );
            let disclosures = disclosures
                .into_iter()
                .map(|choice| {
                    (
                        row_id(&prefix, &origin, &choice.id),
                        choice.status.unwrap_or_else(|| "open".into()),
                    )
                })
                .collect();
            messages.push((row, disclosures));
        }
    }
    for row in &source.session_rows {
        if matches!(
            &row.content,
            ConversationRowContent::User { .. } | ConversationRowContent::Response { .. }
        ) {
            messages.push((
                project_row(
                    &prefix,
                    &ConversationRowOrigin::Session,
                    RowInput {
                        data: row,
                        expanded: false,
                        progress: None,
                        timeline: None,
                    },
                    false,
                    &previous,
                ),
                Vec::new(),
            ));
        }
    }
    messages
}
