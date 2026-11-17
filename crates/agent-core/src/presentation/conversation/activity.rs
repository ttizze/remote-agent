//! Native clients use the same child-conversation and compaction
//! decisions as the desktop timeline, rather than rendering provider JSON.
use super::{
    RenderedItem, TurnRow,
    timeline::{Place, place},
};
use crate::{models::TurnStatus, session::SessionRef, state::Snapshot};
use std::sync::Arc;

#[derive(Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ActivityDecoration {
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
        let rows = match place(self.catalog.ui_language, &item) {
            Place::Alone(rows) => rows,
            _ => return Vec::new(),
        };
        rows.into_iter()
            .filter_map(|row| self.timeline_decoration(row, now_seconds))
            .collect()
    }

    pub fn timeline_decoration(
        &self,
        row: TurnRow,
        now_seconds: f64,
    ) -> Option<ActivityDecoration> {
        match row {
            TurnRow::Subagent { id, agent, .. } => {
                let child = agent
                    .target
                    .as_ref()
                    .and_then(|target| self.sessions.conversations.get(target))
                    .map(|child| &**child);
                let child_status = agent.status(child);
                let status = match child_status {
                    TurnStatus::Running => "running",
                    TurnStatus::Completed => "completed",
                    TurnStatus::Failed => "failed",
                    TurnStatus::Interrupted => "interrupted",
                    TurnStatus::Unknown => "unknown",
                };
                Some(ActivityDecoration::Subagent {
                    id,
                    name: agent.name(child),
                    message: agent.detail(child_status).unwrap_or_default(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{localization::UiLanguage, models::Item};
    use serde_json::json;

    #[test]
    fn native_activity_uses_the_live_child_result_and_the_shared_plain_detail_rule() {
        let mut snapshot = Snapshot::default();
        snapshot.catalog.ui_language = UiLanguage::English;
        let child =
            crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "child".into())
                .unwrap();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(child.clone(), Arc::new(serde_json::from_value(json!({
            "id":child,"name":"Subagent: /root/api_review", "turns":[{"id":"child-turn","status":"completed"}]
        })).unwrap()));
        let item: Item = serde_json::from_value(json!({
            "id":"delegation","status":"running","body":{"inline":{"body":{"subagent":{
                "tool":"task","prompt":"Review", "model":null,"effort":null,"sender":null,
                "receivers":[child],"states":[{"session":child,"status":"running","message":"Still working"}],
                "agentId":null,"result":"- Reviewed `auth.rs`\n- [API](https://example.test/api)"
            }}}}
        })).unwrap();
        let item = RenderedItem::native(
            UiLanguage::English,
            &Arc::new(item),
            None,
            TurnStatus::Completed,
            None,
            None,
            None,
        );
        assert!(matches!(&snapshot.activity_decorations(item,0.0)[..],
            [ActivityDecoration::Subagent { target, name, message, status, .. }]
                if target.as_ref() == Some(&child) && name == "Api Review" && message == "Reviewed auth.rs API" && status == "completed"));
    }

    #[test]
    fn native_activity_only_decorates_subagents_and_compaction() {
        let mut snapshot = Snapshot::default();
        snapshot.catalog.ui_language = UiLanguage::English;
        let item = |body| {
            let item: Item = serde_json::from_value(json!({"id":"activity","status":"completed","clientInputId":null,"body":{"inline":{"body":body}}})).unwrap();
            RenderedItem::native(
                UiLanguage::English,
                &Arc::new(item),
                None,
                crate::models::TurnStatus::Completed,
                None,
                None,
                None,
            )
        };
        assert!(
            snapshot
                .activity_decorations(item(json!({"plan":{"text":"# Plan\n\nFull text"}})), 0.0)
                .is_empty()
        );
        let compacted = snapshot.activity_decorations(item(json!({"compaction":{}})), 0.0);
        assert!(
            matches!(&compacted[..],[ActivityDecoration::Divider {label}] if label == UiLanguage::English.text("work.compaction.done"))
        );
        assert!(snapshot.activity_decorations(item(json!({"commandExecution":{"command":"pwd","cwd":null,"output":"/repo","exitCode":0}})),0.0).is_empty());
    }
}
