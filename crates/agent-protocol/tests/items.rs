use agent_protocol::{
    execution::*,
    items::*,
    models::{Thread, Turn},
    session::*,
};
use proptest::prelude::*;
use std::sync::Arc;

fn conversation(body: ItemBody) -> Thread {
    Thread {
        turns: Some(vec![Arc::new(Turn {
            id: "turn".into(),
            status: TurnStatus::Running,
            items: Some(vec![Arc::new(Item::new(
                "item".into(),
                ItemStatus::Running,
                body,
            ))]),
            ..Default::default()
        })]),
        ..Default::default()
    }
}

#[test]
fn invalid_appends_report_the_boundary_and_preserve_the_input() {
    let original = conversation(ItemBody::AssistantText {
        text: "original".into(),
        phase: AssistantPhase::Final,
        citation: None,
    });
    let before = original.clone();
    for (turn, item, field, expected) in [
        (
            "missing",
            "item",
            TextField::AssistantText,
            UpdateError::MissingTurn,
        ),
        (
            "turn",
            "missing",
            TextField::AssistantText,
            UpdateError::MissingItem,
        ),
        (
            "turn",
            "item",
            TextField::CommandOutput,
            UpdateError::WrongBody,
        ),
        (
            "turn",
            "item",
            TextField::ReasoningSummary { index: u32::MAX },
            UpdateError::WrongBody,
        ),
    ] {
        assert_eq!(
            SessionChange::Text {
                turn_id: turn.into(),
                item_id: item.into(),
                field,
                delta: "bad".into()
            }
            .apply(&original),
            Err(expected)
        );
        assert_eq!(original, before);
    }
    let mut deferred = original.clone();
    Arc::make_mut(
        &mut Arc::make_mut(&mut deferred.turns.as_mut().unwrap()[0])
            .items
            .as_mut()
            .unwrap()[0],
    )
    .defer();
    let before = deferred.clone();
    assert_eq!(
        SessionChange::Text {
            turn_id: "turn".into(),
            item_id: "item".into(),
            field: TextField::AssistantText,
            delta: "bad".into()
        }
        .apply(&deferred),
        Err(UpdateError::DeferredBody)
    );
    assert_eq!(deferred, before);
}

proptest! {
    #[test]
    fn reasoning_parts_keep_indexes_and_never_allocate_holes(operations in prop::collection::vec((any::<bool>(), 0u32..16, ".{0,20}"), 0..60)) {
        let mut thread = conversation(ItemBody::Reasoning {content:vec![], summary:vec![]});
        let (mut expected_content, mut expected_summary) = (Vec::<String>::new(), Vec::<String>::new());
        for (summary, index, delta) in operations {
            let before = thread.clone();
            let part = if summary {&mut expected_summary} else {&mut expected_content};
            let update = SessionChange::Text {turn_id:"turn".into(), item_id:"item".into(), field:if summary {TextField::ReasoningSummary {index}} else {TextField::ReasoningContent {index}}, delta:delta.clone()};
            if index as usize > part.len() {
                prop_assert_eq!(update.apply(&thread), Err(UpdateError::InvalidPartIndex));
                prop_assert_eq!(&thread, &before);
            } else {
                if index as usize == part.len() {part.push(String::new());}
                part[index as usize].push_str(&delta);
                thread = update.apply(&thread).unwrap();
                let ItemBody::Reasoning {content, summary} = thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body() else {unreachable!()};
                prop_assert_eq!(content, &expected_content);
                prop_assert_eq!(summary, &expected_summary);
            }
        }
    }
}

#[test]
fn reasoning_part_notifications_are_idempotent_and_do_not_erase_text() {
    let original = conversation(ItemBody::Reasoning {
        content: vec!["content".into()],
        summary: vec!["summary".into()],
    });
    for field in [ReasoningField::Content, ReasoningField::Summary] {
        let same = SessionChange::ReasoningPart {
            turn_id: "turn".into(),
            item_id: "item".into(),
            field,
            index: 0,
        }
        .apply(&original)
        .unwrap();
        assert_eq!(same, original);
        assert_eq!(
            SessionChange::ReasoningPart {
                turn_id: "turn".into(),
                item_id: "item".into(),
                field,
                index: u32::MAX
            }
            .apply(&original),
            Err(UpdateError::InvalidPartIndex)
        );
    }
}
