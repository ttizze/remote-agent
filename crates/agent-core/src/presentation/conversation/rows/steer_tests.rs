use super::*;
use crate::{localization::UiLanguage, presentation::conversation::project_conversation};
use serde_json::json;

#[test]
fn consecutive_steers_have_one_marker_without_changing_the_source() {
    // T3's timeline marks the first message in each consecutive run of steers.
    // Three or more steers also reject an implementation that alternates markers.
    for count in 1..=8 {
        let user = |index| {
            json!({"id":format!("user-{index}"), "status":"completed",
            "body":{"inline":{"body":{"userMessage":{"text":format!("message {index}"),"content":[]}}}}})
        };
        let mut items = vec![
            user(0),
            json!({"id":"thought", "status":"completed",
            "body":{"inline":{"body":{"reasoning":{"content":[],"summary":["Inspecting"]}}}}}),
        ];
        items.extend((1..=count).map(user));
        let thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},
            "turns":[{"id":"turn","status":"running","items":items}]}))
        .unwrap();
        let source = project_conversation(
            UiLanguage::English,
            &"draft".into(),
            &Arc::default(),
            Arc::new(thread),
            &None,
        );
        let raw_markers = || {
            source.turns[0]
                .rows
                .iter()
                .filter_map(|row| match row.content {
                    ConversationRowContent::User { .. } => Some(row.steer),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let mut all = vec![false];
        all.extend(std::iter::repeat_n(true, count));
        assert_eq!(raw_markers(), all);
        let projected = ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            source.clone(),
            PanelScope::default(),
            None,
            HashMap::new(),
            None,
        );
        let markers: Vec<_> = projected
            .rows
            .iter()
            .filter_map(|row| match row.data.content {
                ConversationRowContent::User { .. } => Some(row.data.steer),
                _ => None,
            })
            .collect();
        let mut expected = vec![false; count + 1];
        expected[1] = true;
        assert_eq!(
            markers, expected,
            "{count} consecutive steers need exactly one marker"
        );
        assert_eq!(
            raw_markers(),
            all,
            "projection must not mutate the provider model"
        );
    }
}
