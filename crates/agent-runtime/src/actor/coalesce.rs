use agent_domain::{ProviderEvent, RunAttemptId};

pub const COALESCE_LIMIT: usize = 64 * 1024;

pub(super) fn is_delta(event: &ProviderEvent) -> bool {
    matches!(
        event,
        ProviderEvent::TextDelta { .. } | ProviderEvent::PlanDelta { .. }
    )
}

/// Appends `next` to `first` when both stream into the same item and the result stays bounded.
pub(super) fn join(
    attempt: &RunAttemptId,
    first: &mut ProviderEvent,
    next_attempt: &RunAttemptId,
    next: &ProviderEvent,
) -> bool {
    if attempt != next_attempt {
        return false;
    }
    let (text, addition) = match (first, next) {
        (
            ProviderEvent::TextDelta { key, kind, text },
            ProviderEvent::TextDelta {
                key: next_key,
                kind: next_kind,
                text: addition,
            },
        ) if key == next_key && kind == next_kind => (text, addition),
        (
            ProviderEvent::PlanDelta { key, text },
            ProviderEvent::PlanDelta {
                key: next_key,
                text: addition,
            },
        ) if key == next_key => (text, addition),
        _ => return false,
    };
    if text.len() + addition.len() > COALESCE_LIMIT {
        return false;
    }
    text.push_str(addition);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::ProviderItem;
    use proptest::prelude::*;

    fn attempt(id: &str) -> RunAttemptId {
        RunAttemptId::new(id).unwrap()
    }
    fn text(key: &str, text: &str) -> ProviderEvent {
        ProviderEvent::TextDelta {
            key: key.into(),
            kind: ProviderItem::Text,
            text: text.into(),
        }
    }

    #[test]
    fn joins_only_matching_streams() {
        let a = attempt("a");
        let mut first = text("k", "he");
        assert!(join(&a, &mut first, &a, &text("k", "llo")));
        assert_eq!(first, text("k", "hello"));
        assert!(!join(&a, &mut first, &attempt("b"), &text("k", "!")));
        assert!(!join(&a, &mut first, &a, &text("other", "!")));
        let reasoning = ProviderEvent::TextDelta {
            key: "k".into(),
            kind: ProviderItem::Reasoning,
            text: "!".into(),
        };
        assert!(!join(&a, &mut first, &a, &reasoning));
        let plan = ProviderEvent::PlanDelta {
            key: "k".into(),
            text: "!".into(),
        };
        assert!(!join(&a, &mut first, &a, &plan));
        let mut plan_first = plan.clone();
        assert!(join(&a, &mut plan_first, &a, &plan));
        let wrapped = ProviderEvent::NativeOutput {
            echoed_prompts: vec![],
            acknowledged_prompt: None,
            root: true,
            result: None,
            events: vec![text("k", "!")],
        };
        assert!(!join(&a, &mut first, &a, &wrapped));
        assert_eq!(first, text("k", "hello"));
    }

    #[test]
    fn stops_at_the_size_limit() {
        let a = attempt("a");
        let mut first = text("k", &"x".repeat(COALESCE_LIMIT - 1));
        assert!(join(&a, &mut first, &a, &text("k", "y")));
        assert!(!join(&a, &mut first, &a, &text("k", "z")));
    }

    proptest! {
        #[test]
        fn joined_chunks_preserve_text_and_bound(
            pieces in prop::collection::vec((any::<char>(), 0usize..30_000), 1..8),
        ) {
            let chunks: Vec<String> = pieces
                .iter()
                .map(|(c, n)| c.to_string().repeat(*n))
                .collect();
            let a = attempt("a");
            let mut joined = vec![(text("k", &chunks[0]), 1)];
            for chunk in &chunks[1..] {
                let next = text("k", chunk);
                let (last, count) = joined.last_mut().unwrap();
                if join(&a, last, &a, &next) {
                    *count += 1;
                } else {
                    joined.push((next, 1));
                }
            }
            let mut all = String::new();
            for (event, count) in &joined {
                let ProviderEvent::TextDelta { text, .. } = event else { unreachable!() };
                prop_assert!(*count == 1 || text.len() <= COALESCE_LIMIT);
                all.push_str(text);
            }
            prop_assert_eq!(all, chunks.concat());
        }
    }
}
