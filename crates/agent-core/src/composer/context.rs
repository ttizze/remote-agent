use crate::state::Snapshot;
use agent_protocol::context::{AssistantCitation, ContextContent, MessageContext};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ContextAction {
    Add { context: Box<MessageContext> },
    Remove { id: String },
    Comment { id: String, text: String },
}

pub(crate) fn change(
    current: &[MessageContext],
    action: ContextAction,
) -> Result<Vec<MessageContext>, &'static str> {
    let mut next = current.to_vec();
    match action {
        ContextAction::Add { context } => {
            if let Some(index) = next.iter().position(|item| item.id == context.id) {
                next[index] = *context;
            } else {
                next.push(*context);
            }
        }
        ContextAction::Remove { id } => {
            next.retain(|item| item.id != id);
            return Ok(next);
        }
        ContextAction::Comment { id, text } => {
            let item = next
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or("citation is no longer in the draft")?;
            let ContextContent::Citation { citation } = &mut item.content else {
                return Err("context is not a citation");
            };
            citation.comment = text;
            citation.validate()?;
        }
    }
    agent_protocol::context::validate_contexts(&next)?;
    Ok(next)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn make_citation(
    source: agent_protocol::context::CitationSource,
    rendered_text: String,
    range: agent_protocol::context::TextRange,
) -> Result<AssistantCitation, crate::client::AgentError> {
    AssistantCitation::from_selection(source, &rendered_text, range).map_err(crate::client::error)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn locate_citation(
    citation: AssistantCitation,
    rendered_text: String,
) -> Option<agent_protocol::context::TextRange> {
    citation.locate(&rendered_text)
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn citation_source(
        &self,
        host: String,
        session: agent_protocol::session::SessionRef,
        item_id: agent_protocol::ids::ItemId,
    ) -> Option<agent_protocol::context::CitationSource> {
        let thread = self.sessions.conversations.get(&session)?;
        let mut turns = thread.turns.as_ref()?.iter().filter(|turn| {
            turn.items.iter().flatten().any(|item| {
                item.id == item_id
                    && matches!(
                        item.body(),
                        agent_protocol::items::ItemBody::AssistantText { .. }
                    )
            })
        });
        let turn = turns.next()?;
        if turns.next().is_some() {
            return None;
        }
        let source = agent_protocol::context::CitationSource {
            host,
            storage_scope: self.connection.storage_scope.clone(),
            session,
            turn_id: turn.id.clone(),
            item_id,
        };
        if source.host.trim().is_empty() || source.storage_scope.is_empty() {
            return None;
        }
        Some(source)
    }
}

/// Context remains typed in the projection. This text also keeps its content
/// readable in plain-text copy/export and clients without the specialized chip.
pub(crate) fn display_text(context: &MessageContext) -> String {
    match &context.content {
        ContextContent::Citation { citation } => {
            let quote = citation
                .text
                .lines()
                .map(|line| format!("> {}", literal(line)))
                .collect::<Vec<_>>()
                .join("\n");
            if citation.comment.is_empty() {
                quote
            } else {
                format!("{quote}\n\n{}", literal(&citation.comment))
            }
        }
        ContextContent::Terminal {
            terminal_label,
            line_start,
            line_end,
            text,
            ..
        } => format!(
            "{} ({line_start}–{line_end})\n\n{}",
            literal(terminal_label),
            code(text, "text")
        ),
        ContextContent::Element { element } => format!(
            "{}\n\n{}\n\n{}",
            literal(&context.label),
            code(&element.html, "html"),
            code(&element.styles, "css")
        ),
        ContextContent::Review { review } => format!(
            "{} ({})\n\n{}\n\n{}",
            literal(&context.label),
            literal(&review.range_label),
            literal(&review.text),
            code(&review.diff, "diff")
        ),
        ContextContent::Preview { preview } => format!(
            "{}\n\n{}",
            literal(&context.label),
            literal(&preview.comment)
        ),
        _ => literal(&context.label),
    }
}

fn literal(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            result.push('\\');
        }
        result.push(character);
    }
    result
}

fn code(text: &str, language: &str) -> String {
    let width = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default()
        .saturating_add(1)
        .max(3);
    let fence = "`".repeat(width);
    format!("{fence}{language}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        context::{CitationSource, TextRange},
        session::{ProviderKind, SessionRef},
    };

    fn citation() -> MessageContext {
        MessageContext {
            id: "quote".into(),
            label: "引用".into(),
            content: ContextContent::Citation {
                citation: AssistantCitation::from_selection(
                    CitationSource {
                        host: "host".into(),
                        storage_scope: "scope".into(),
                        session: SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap(),
                        turn_id: "turn".into(),
                        item_id: "item".into(),
                    },
                    "引用する",
                    TextRange { start: 0, end: 2 },
                )
                .unwrap(),
            },
        }
    }

    #[test]
    fn edits_preserve_identity_order_and_source_and_reject_invalid_comments_atomically() {
        let current = change(
            &[],
            ContextAction::Add {
                context: Box::new(citation()),
            },
        )
        .unwrap();
        let updated = change(
            &current,
            ContextAction::Comment {
                id: "quote".into(),
                text: "説明して".into(),
            },
        )
        .unwrap();
        assert_eq!(current[0], citation());
        assert_eq!(display_text(&updated[0]), "> 引用\n\n説明して");
        assert!(
            change(
                &updated,
                ContextAction::Comment {
                    id: "quote".into(),
                    text: "x".repeat(8001)
                }
            )
            .is_err()
        );
        assert_eq!(
            change(
                &updated,
                ContextAction::Add {
                    context: Box::new(citation())
                }
            )
            .unwrap()
            .len(),
            1
        );
        assert!(
            change(&updated, ContextAction::Remove { id: "quote".into() })
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_restored_context_can_be_removed_and_terminal_data_stays_literal() {
        let mut malformed = citation();
        if let ContextContent::Citation { citation } = &mut malformed.content {
            citation.comment = "x".repeat(8001);
        }
        let mut other = citation();
        other.id = "other".into();
        let remaining = change(
            &[malformed, other],
            ContextAction::Remove { id: "other".into() },
        )
        .unwrap();
        assert!(remaining[0].validate().is_err());
        assert!(
            change(&remaining, ContextAction::Remove { id: "quote".into() })
                .unwrap()
                .is_empty()
        );
        let literal =
            "```\n![data](https://example.com/data.png)\n<details>literal</details>\n$HOME";
        let context = MessageContext {
            id: "terminal".into(),
            label: "Build".into(),
            content: ContextContent::Terminal {
                terminal_id: "id".into(),
                terminal_label: "Build".into(),
                line_start: 1,
                line_end: 2,
                text: literal.into(),
            },
        };
        let markdown = display_text(&context);
        assert!(crate::presentation::markdown::markdown_images(&markdown).is_empty());
        let blocks = crate::presentation::markdown::markdown_blocks(markdown);
        let code = blocks
            .iter()
            .find_map(|block| match block {
                crate::presentation::markdown::MarkdownBlock::Paragraph { runs, style }
                    if style.code =>
                {
                    Some(runs)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            code.iter().map(|run| run.text.as_str()).collect::<String>(),
            literal
        );
        let context = MessageContext {
            label: "![caption](https://example.com/image.png) <details> $value".into(),
            content: ContextContent::File {
                path: "/file".into(),
            },
            ..context
        };
        let text = display_text(&context);
        assert!(crate::presentation::markdown::markdown_images(&text).is_empty());
        let blocks = crate::presentation::markdown::markdown_blocks(text);
        assert_eq!(
            crate::presentation::markdown::markdown_runs(&blocks)
                .into_iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            context.label
        );
    }

    #[test]
    fn citation_source_uses_the_loaded_scope_and_rejects_ambiguous_or_non_response_ids() {
        use crate::models::{Item, ItemBody, ItemStatus, Thread, Turn};
        use std::sync::Arc;
        let session = SessionRef::new(ProviderKind::Codex, "thread".into()).unwrap();
        let response = Arc::new(Item::new(
            "answer".into(),
            ItemStatus::Completed,
            ItemBody::AssistantText {
                text: "answer".into(),
                phase: Default::default(),
            },
        ));
        let user = Arc::new(Item::new(
            "user".into(),
            ItemStatus::Completed,
            ItemBody::UserMessage {
                text: Some("prompt".into()),
                content: vec![],
            },
        ));
        let turn = Arc::new(Turn {
            id: "turn".into(),
            items: Some(vec![user, response]),
            ..Default::default()
        });
        let mut state = crate::state::Snapshot::default();
        state.connection.storage_scope = "scope".into();
        Arc::make_mut(&mut state.sessions.conversations).insert(
            session.clone(),
            Arc::new(Thread {
                id: Some(session.clone()),
                turns: Some(vec![turn.clone()]),
                ..Default::default()
            }),
        );
        let source = state
            .citation_source("host".into(), session.clone(), "answer".into())
            .unwrap();
        assert_eq!(source.storage_scope, "scope");
        assert_eq!(source.turn_id.as_str(), "turn");
        assert!(
            state
                .citation_source("host".into(), session.clone(), "user".into())
                .is_none()
        );
        let thread = Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&session)
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().push(Arc::new(Turn {
            id: "other".into(),
            ..turn.as_ref().clone()
        }));
        assert!(
            state
                .citation_source("host".into(), session, "answer".into())
                .is_none()
        );
    }
}
