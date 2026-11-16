//! Paged excerpts of saved conversation history for the history reader.
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;

const EXCERPT_CHARS: usize = 4_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadHistory {
    pub session: crate::session::SessionRef,
    pub item_id: Option<String>,
    pub offset: usize,
    pub limit: usize,
    pub text_offset: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Excerpt {
    pub position: usize,
    pub message: Message,
    pub next_text_offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page {
    pub history_read_state: Option<crate::session::HistoryReadState>,
    pub items: Vec<Excerpt>,
    pub next_offset: Option<usize>,
}

pub fn page(
    messages: impl IntoIterator<Item = impl Borrow<Message>>,
    offset: usize,
    limit: usize,
    text_offset: usize,
) -> Page {
    let mut messages = messages.into_iter().enumerate().skip(offset);
    let items = messages
        .by_ref()
        .take(limit.clamp(1, 20))
        .map(|(position, message)| {
            let message = message.borrow();
            let start = if position == offset { text_offset } else { 0 };
            let mut chars = message.text.chars().skip(start);
            let text = chars.by_ref().take(EXCERPT_CHARS).collect();
            // A further character means this excerpt consumed the full limit.
            let next_text_offset = chars.next().map(|_| start.saturating_add(EXCERPT_CHARS));
            Excerpt {
                position,
                message: Message {
                    role: message.role,
                    text,
                    source: message.source.clone(),
                },
                next_text_offset,
            }
        })
        .collect();
    let next_offset = messages.next().map(|(position, _)| position);
    Page {
        history_read_state: None,
        items,
        next_offset,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieval_pages_long_unicode_text_without_losing_boundaries() {
        let text = "日本語🦀".repeat(1700);
        let messages = vec![
            message(Role::User, &text),
            message(Role::Assistant, "answer"),
        ];
        let first = page(&messages, 0, 1, 0);
        assert_eq!(first.next_offset, Some(1));
        assert_eq!(first.items[0].message.text.chars().count(), 4000);
        let second = page(&messages, 0, 1, first.items[0].next_text_offset.unwrap());
        assert_eq!(
            format!(
                "{}{}",
                first.items[0].message.text, second.items[0].message.text
            ),
            text
        );
        assert_eq!(second.items[0].next_text_offset, None);
        assert_eq!(page(&messages, 1, 1, 0).items[0].message.text, "answer");
        assert!(page(&messages, usize::MAX, 1, 0).items.is_empty());
    }

    #[test]
    fn excerpts_keep_scalar_boundaries_metadata_and_exact_continuation_offsets() {
        for count in [0usize, 1, 3999, 4000, 4001, 8000, 8001] {
            let chars: Vec<_> = "🦀éa".chars().cycle().take(count).collect();
            let text: String = chars.iter().copied().collect();
            let messages = [Message {
                role: Role::Assistant,
                text,
                source: "turn=起点, item=🦀".into(),
            }];
            for start in [
                0,
                1,
                3999,
                4000,
                4001,
                count,
                count.saturating_add(1),
                usize::MAX,
            ] {
                let output = page(&messages, 0, 1, start);
                let end = start.saturating_add(4000).min(count);
                assert_eq!(output.items.len(), 1);
                assert_eq!(output.items[0].position, 0);
                assert_eq!(
                    output.items[0].message,
                    Message {
                        role: Role::Assistant,
                        text: chars[start.min(count)..end].iter().copied().collect(),
                        source: "turn=起点, item=🦀".into(),
                    }
                );
                assert_eq!(
                    output.items[0].next_text_offset,
                    (end < count).then_some(end)
                );
                assert_eq!(output.next_offset, None);
            }
        }
    }

    #[test]
    fn retrieval_offsets_apply_only_to_the_first_message_and_keep_message_limits() {
        let messages = [
            message(Role::User, &"🦀".repeat(4501)),
            message(Role::Assistant, "reply"),
            message(Role::User, "tail"),
        ];
        let output = page(&messages, 0, 2, 4500);
        assert_eq!(output.items.len(), 2);
        assert_eq!(output.items[0].message.text, "🦀");
        assert_eq!(output.items[0].next_text_offset, None);
        assert_eq!(output.items[1].message, messages[1]);
        assert_eq!(output.items[1].position, 1);
        assert_eq!(output.next_offset, Some(2));
        let last = page(&messages, 2, 0, 0);
        assert_eq!(last.items.len(), 1);
        assert_eq!(last.items[0].message, messages[2]);
        assert_eq!(last.next_offset, None);
        let many = vec![message(Role::User, "message"); 25];
        let capped = page(&many, 0, usize::MAX, 0);
        assert_eq!(capped.items.len(), 20);
        assert_eq!(capped.next_offset, Some(20));
    }

    #[test]
    fn retrieval_stops_after_the_requested_page_and_one_lookahead() {
        let messages = (0..).map(|index| {
            assert!(index < 24, "the page must not consume the rest of history");
            message(Role::Assistant, &format!("message {index}"))
        });
        let output = page(messages, 3, 20, 1);
        assert_eq!(output.next_offset, Some(23));
        assert_eq!(output.items.len(), 20);
        for (index, excerpt) in output.items.iter().enumerate() {
            assert_eq!(excerpt.position, index + 3);
            assert_eq!(excerpt.message.role, Role::Assistant);
            assert_eq!(excerpt.message.source, "source");
            assert_eq!(
                excerpt.message.text,
                if index == 0 {
                    "essage 3".into()
                } else {
                    format!("message {}", index + 3)
                }
            );
            assert_eq!(excerpt.next_text_offset, None);
        }
    }

    #[test]
    fn retrieval_cursors_match_message_boundaries_for_owned_and_borrowed_inputs() {
        let messages: Vec<_> = (0..25)
            .map(|index| message(Role::User, &format!("message {index}")))
            .collect();
        for offset in [0, 1, 19, 20, 21, 24, 25, 26, usize::MAX] {
            for (limit, count) in [(0, 1), (1, 1), (20, 20), (21, 20), (usize::MAX, 20)] {
                let remaining = messages.get(offset..).unwrap_or_default();
                let expected = &remaining[..remaining.len().min(count)];
                let borrowed = page(&messages, offset, limit, 0);
                assert_eq!(borrowed.items.len(), expected.len());
                for (index, (excerpt, message)) in borrowed.items.iter().zip(expected).enumerate() {
                    assert_eq!(&excerpt.message, message);
                    assert_eq!(excerpt.position, offset + index);
                }
                assert_eq!(
                    borrowed.next_offset,
                    if remaining.len() > count {
                        Some(offset + count)
                    } else {
                        None
                    }
                );
                let owned = page(messages.iter().cloned(), offset, limit, 0);
                assert_eq!(
                    serde_json::to_value(owned).unwrap(),
                    serde_json::to_value(borrowed).unwrap()
                );
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn excerpt_matches_the_requested_slice_of_unicode_scalars(
            chars in proptest::collection::vec(proptest::char::any(),0..10_000),
            start in proptest::prop_oneof![0usize..10_010,proptest::strategy::Just(usize::MAX)],
        ) {
            let messages=[Message {role:Role::User,text:chars.iter().copied().collect(),source:"source".into()}];
            let output=page(&messages,0,1,start);
            let end=start.saturating_add(4000).min(chars.len());
            let expected: String=chars[start.min(chars.len())..end].iter().copied().collect();
            proptest::prop_assert_eq!(&output.items[0].message.text,&expected);
            proptest::prop_assert_eq!(output.items[0].next_text_offset,(end<chars.len()).then_some(end));
            proptest::prop_assert_eq!(output.items[0].message.role,Role::User);
            proptest::prop_assert_eq!(&output.items[0].message.source,"source");
        }
    }

    fn message(role: Role, text: &str) -> Message {
        Message {
            role,
            text: text.into(),
            source: "source".into(),
        }
    }
}
