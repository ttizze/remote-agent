//! Native clients use the same plan, child-conversation and compaction
//! decisions as the desktop timeline, rather than rendering provider JSON.
use super::{
    RenderedItem, TurnRow, plan_export,
    timeline::{Place, place},
};
use crate::{models::TurnStatus, session::SessionRef, state::Snapshot};
use std::sync::Arc;

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ActivityDecoration {
    Plan {
        title: String,
        markdown: String,
        filename: String,
    },
    Subagent {
        id: String,
        target: Option<SessionRef>,
        name: String,
        message: String,
        status: String,
        elapsed: Option<String>,
    },
    Divider {
        label: String,
    },
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn activity_decorations(
        &self,
        item: Arc<RenderedItem>,
        now_seconds: f64,
    ) -> Vec<ActivityDecoration> {
        let Place::Alone(rows) = place(self.catalog.ui_language, &item) else {
            return Vec::new();
        };
        rows.into_iter()
            .filter_map(|row| match row {
                TurnRow::Plan { title, text, .. } => {
                    let (filename, markdown) = plan_export(&text);
                    Some(ActivityDecoration::Plan {
                        title,
                        markdown,
                        filename,
                    })
                }
                TurnRow::Subagent { id, agent } => {
                    let child = agent
                        .target
                        .as_ref()
                        .and_then(|target| self.sessions.conversations.get(target))
                        .map(|child| &**child);
                    let status = match agent.status(child) {
                        TurnStatus::Running => "running",
                        TurnStatus::Completed => "completed",
                        TurnStatus::Failed => "failed",
                        TurnStatus::Interrupted => "interrupted",
                        TurnStatus::Unknown => "unknown",
                    };
                    Some(ActivityDecoration::Subagent {
                        id,
                        name: agent.name(child),
                        message: agent.message.clone(),
                        status: status.into(),
                        elapsed: crate::presentation::agents::subagent_elapsed(
                            self.catalog.ui_language,
                            child,
                            now_seconds,
                        ),
                        target: agent.target,
                    })
                }
                TurnRow::Divider { label, .. } => Some(ActivityDecoration::Divider { label }),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{localization::UiLanguage, models::Item};
    use serde_json::json;

    #[test]
    fn native_activity_uses_the_timeline_plan_export_and_compaction_label() {
        let mut snapshot = Snapshot::default();
        snapshot.catalog.ui_language = UiLanguage::English;
        let item = |body| {
            let item: Item = serde_json::from_value(json!({"id":"activity","status":"completed","clientInputId":null,"body":{"inline":{"body":body}}})).unwrap();
            RenderedItem::native(
                UiLanguage::English,
                &Arc::new(item),
                None,
                false,
                None,
                None,
                None,
            )
        };
        assert!(
            matches!(&snapshot.activity_decorations(item(json!({"plan":{"text":"# Check native UI\n\n1. Verify"}})),0.0)[..],
            [ActivityDecoration::Plan {title,markdown,filename}] if title == "Check native UI" && markdown == "# Check native UI\n\n1. Verify\n" && filename == "check-native-ui.md")
        );
        let compacted = snapshot.activity_decorations(item(json!({"compaction":{}})), 0.0);
        assert!(
            matches!(&compacted[..],[ActivityDecoration::Divider {label}] if label == UiLanguage::English.text("work.compaction.done"))
        );
        assert!(snapshot.activity_decorations(item(json!({"commandExecution":{"command":"pwd","cwd":null,"output":"/repo","exitCode":0}})),0.0).is_empty());
    }
}
