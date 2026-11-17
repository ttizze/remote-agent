use super::*;
use crate::panels::PanelScope;
use serde_json::json;

#[test]
fn working_header_names_active_compaction_and_keeps_the_turns_elapsed_timer() {
    // T3 WorkingTimelineRow / CompactingLabel: the ordinary header stays plain;
    // active compaction changes its label, retaining WorkingTimer's turn origin.
    for start in [Some(1000), None] {
        let source = serde_json::from_value(json!({
            "id":{"provider":"codex","id":"thread"},
            "turns":[{"id":"turn","status":"running","startedAtMs":start,
                "items":[{"id":"compact","status":"running","body":{"inline":{"body":{"compaction":{}}}}}]}]
        })).unwrap();
        let raw = project_conversation(
            UiLanguage::English,
            &"draft".into(),
            &Default::default(),
            Arc::new(source),
            &None,
        );
        let rows = rows::ConversationRows::project(
            crate::presentation::conversation::TimelineStyle::NATIVE,
            raw,
            PanelScope {
                host: "host".into(),
                storage: "store".into(),
                draft: "draft".into(),
            },
            None,
            HashMap::new(),
            None,
        );
        let working = rows
            .rows
            .iter()
            .find(|row| matches!(row.timeline(), Some(TurnRow::Working)))
            .unwrap();
        let label = working
            .progress
            .as_ref()
            .unwrap()
            .label(UiLanguage::English, 3.);
        assert_eq!(
            label,
            if start.is_some() {
                "Compacting for 2s"
            } else {
                "Compacting…"
            }
        );
    }
}
