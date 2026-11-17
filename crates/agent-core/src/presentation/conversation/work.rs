//! T3 Code's live work: what a running turn shows as live, and how a work row
//! reads, reasoning as its thinking.
use super::{ConversationRowContent, ItemSource, RenderedConversation, RenderedItem, RenderedTurn};
use crate::models::{ItemStatus, TurnStatus};

/// An expanded item reads once while live, then for each settled body revision.
/// A new connection also retires a failed or unavailable read from the old one.
pub fn item_detail_key(streaming: bool, body_revision: Option<&str>, epoch: u64) -> String {
    match (streaming, body_revision) {
        (true, _) => format!("{epoch}:live"),
        (false, Some(revision)) => format!("{epoch}:{revision}"),
        (false, None) => format!("{epoch}:settled"),
    }
}

pub(super) fn item_is_settled(item: ItemStatus, turn: TurnStatus) -> bool {
    item != ItemStatus::Running && (item != ItemStatus::Unknown || turn != TurnStatus::Running)
}

/// What a running turn shows as live, as T3 Code's live activity row: it
/// stays shown while the turn's work is folded, and shimmers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LiveActivity {
    /// The work row last reached, by row ID.
    Row(String),
    /// No work is live: the turn is thinking (T3 Code's Thinking row).
    Thinking,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RenderedConversation {
    /// Hydrated provider history can omit item status even while its turn streams.
    pub fn shell_command_settled(&self, item_id: agent_protocol::ids::ItemId) -> bool {
        let mut matches = self
            .turns
            .iter()
            .flat_map(|turn| {
                turn.source
                    .items
                    .iter()
                    .flatten()
                    .map(move |item| (turn.source.status, item))
            })
            .filter(|(_, item)| item.id == item_id);
        let Some((turn, item)) = matches.next() else {
            return false;
        };
        matches.next().is_none() && item_is_settled(item.status, turn)
    }
}

impl RenderedTurn {
    /// None once the turn has ended, while a request waits for its answer
    /// (nothing works then) and while a response streams (it is the live part).
    pub(super) fn live_activity(&self) -> Option<LiveActivity> {
        if self.source.status != TurnStatus::Running || !self.requests.is_empty() {
            return None;
        }
        let status = |item: &RenderedItem| match &item.source {
            ItemSource::Native(item) => item.status,
            ItemSource::Pending(..) => ItemStatus::Unknown,
        };
        let last = self.rows.iter().rev().find(|row| match &row.content {
            ConversationRowContent::User { .. } => true,
            ConversationRowContent::Activity { item, .. } => item.data.kind != "systemNotice",
            ConversationRowContent::Response { item, .. } => {
                !matches!(item.data.kind.as_str(), "imageView" | "imageGeneration")
                    || status(item) == ItemStatus::Running
            }
            _ => false,
        });
        match last.map(|row| (&row.content, row)) {
            // As T3 Code, failed work leaves the turn thinking.
            Some((ConversationRowContent::Activity { item, .. }, row))
                if !matches!(
                    status(item),
                    ItemStatus::Failed | ItemStatus::Declined | ItemStatus::Interrupted
                ) =>
            {
                Some(LiveActivity::Row(row.id.clone()))
            }
            Some((ConversationRowContent::Response { item, .. }, _))
                if status(item) == ItemStatus::Running =>
            {
                None
            }
            _ => Some(LiveActivity::Thinking),
        }
    }
}

/// A work row's text, as T3 Code's work log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRowLabel {
    /// The work's title in the UI language; reasoning's is Thinking while live
    /// and Thought after. Expanded, the row reads it.
    pub heading: String,
    /// Provider text, shown verbatim in place of the heading while the row is
    /// collapsed: reasoning's trace on one line.
    pub preview: Option<String>,
}

impl RenderedItem {
    pub fn work_row_label(&self, live: bool) -> WorkRowLabel {
        let reasoning = self.data.kind == "reasoning";
        WorkRowLabel {
            heading: if reasoning && live {
                self.language.unwrap_or_default().text("思考中").into()
            } else {
                self.data.title.clone().unwrap_or_default()
            },
            preview: self.data.preview.clone().filter(|_| reasoning),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ConversationRowContent, project_conversation};
    use super::*;
    use crate::{models, state::Snapshot};
    use serde_json::{Value, json};
    use std::sync::Arc;

    #[test]
    fn detail_keys_track_settled_bytes_and_connections_but_ignore_live_byte_updates() {
        for (streaming, revision, epoch, expected) in [
            (true, None, 0, "0:live"),
            (true, Some("1"), 0, "0:live"),
            (true, Some("2"), 0, "0:live"),
            (false, None, 0, "0:settled"),
            (false, Some("0"), 0, "0:0"),
            (false, Some("3"), 0, "0:3"),
            (false, Some("4"), 0, "0:4"),
            (true, Some("2"), 1, "1:live"),
            (false, Some("3"), 1, "1:3"),
            (
                false,
                Some("18446744073709551615"),
                0,
                "0:18446744073709551615",
            ),
        ] {
            assert_eq!(item_detail_key(streaming, revision, epoch), expected);
        }
    }

    #[test]
    fn shell_actions_wait_for_a_stable_item_in_its_own_turn() {
        let render = |turns| {
            let source: models::Thread = serde_json::from_value(json!({
                "id":{"provider":"codex","id":"shell"}, "turns": turns
            }))
            .unwrap();
            project_conversation(
                crate::localization::UiLanguage::English,
                &Default::default(),
                &Default::default(),
                Arc::new(source),
                &None,
            )
        };
        let source = render(json!([
            {"id":"old","status":"completed","items":[text("old", "unknown", "final")]},
            {"id":"run","status":"running","items":[text("hydrated", "unknown", "final"), text("streaming", "running", "final"), text("complete", "completed", "final")]}
        ]));
        assert!(source.shell_command_settled("old".into()));
        assert!(source.shell_command_settled("complete".into()));
        assert!(!source.shell_command_settled("hydrated".into()));
        assert!(!source.shell_command_settled("streaming".into()));
        assert!(!source.shell_command_settled("absent".into()));
        let finished = render(json!([
            {"id":"run","status":"completed","items":[text("hydrated", "unknown", "final")]}
        ]));
        assert!(finished.shell_command_settled("hydrated".into()));
        let ambiguous = render(json!([
            {"id":"one","status":"completed","items":[text("same", "completed", "final")]},
            {"id":"two","status":"completed","items":[text("same", "completed", "final")]}
        ]));
        assert!(!ambiguous.shell_command_settled("same".into()));
    }

    fn item(id: &str, status: &str, body: Value) -> Value {
        json!({"id":id,"status":status,"clientInputId":null,"body":{"inline":{"body":body}}})
    }
    fn reasoning(id: &str, status: &str, summary: &[&str]) -> Value {
        item(
            id,
            status,
            json!({"reasoning":{"content":[],"summary":summary}}),
        )
    }
    fn command(id: &str, status: &str) -> Value {
        item(
            id,
            status,
            json!({"commandExecution":{"command":id,"cwd":null,"output":"","exitCode":null}}),
        )
    }
    fn text(id: &str, status: &str, phase: &str) -> Value {
        item(
            id,
            status,
            json!({"assistantText":{"text":id,"phase":phase}}),
        )
    }
    fn user(id: &str) -> Value {
        item(
            id,
            "completed",
            json!({"userMessage":{"text":id,"content":[]}}),
        )
    }

    fn turn(status: &str, items: Vec<Value>, requested: bool) -> Arc<RenderedTurn> {
        let mut thread: models::Thread = serde_json::from_value(json!({
            "id":{"provider":"codex","id":"thread"},
            "turns":[{"id":"turn","status":status,"items":items}]
        }))
        .unwrap();
        if requested {
            use agent_protocol::requests::{ElicitationInput, RequestBody, RequestTarget};
            let request = agent_protocol::requests::Request {
                id: "request".into(),
                target: RequestTarget::Turn {
                    turn_id: "turn".into(),
                    item_id: None,
                },
                delivery: agent_protocol::session::RequestDelivery::Awaiting,
                body: RequestBody::Elicitation {
                    server: "mcp".into(),
                    message: "confirm".into(),
                    input: ElicitationInput::Url {
                        url: "https://example.com/".into(),
                    },
                },
            };
            thread.requests = [(request.id.clone(), Arc::new(request))].into();
        }
        let snapshot = Snapshot::default();
        project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(thread),
            &None,
        )
        .turns[0]
            .clone()
    }

    /// The native ID of the live row, or "thinking".
    fn live(turn: &RenderedTurn) -> Option<String> {
        // Verify the output consumed by both clients, not only the tail hint
        // before grouping, request placement and turn-wide composition.
        super::super::turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            turn,
            &Default::default(),
            &Default::default(),
        )
        .into_iter()
        .find_map(|row| match row {
            super::super::TurnRow::Thinking => Some("thinking".into()),
            super::super::TurnRow::Live { row, .. } => {
                let ConversationRowContent::Activity { item, .. } = &row.content else {
                    panic!("a live row is work")
                };
                Some(item.data.native_id.clone().unwrap().to_string())
            }
            _ => None,
        })
    }

    #[rstest::rstest]
    #[case::started(vec![user("u")], Some("thinking"))]
    #[case::thinking(vec![user("u"), reasoning("r", "running", &["plan"])], Some("r"))]
    #[case::blank_reasoning(vec![user("u"), reasoning("r", "running", &[" "])], Some("thinking"))]
    #[case::running_tool(vec![user("u"), reasoning("r", "completed", &["plan"]), command("c", "running")], Some("c"))]
    // As T3 Code, finished work stays live until the next arrives.
    #[case::finished_tool(vec![user("u"), command("c", "completed")], Some("c"))]
    #[case::superseded_reasoning(vec![user("u"), reasoning("r", "running", &["plan"]), command("c", "completed")], Some("c"))]
    #[case::failed_tool(vec![user("u"), command("c", "failed")], Some("thinking"))]
    #[case::declined_tool(vec![user("u"), command("c", "declined")], Some("thinking"))]
    #[case::after_commentary(vec![user("u"), command("c", "completed"), text("a", "completed", "commentary")], Some("thinking"))]
    #[case::work_after_commentary(vec![user("u"), text("a", "completed", "commentary"), command("c", "running")], Some("c"))]
    #[case::streaming_answer(vec![user("u"), command("c", "completed"), text("a", "running", "final")], None)]
    #[case::steered(vec![user("u"), command("c", "completed"), user("steer")], Some("thinking"))]
    fn a_running_turn_shows_its_latest_work_or_thinking(
        #[case] items: Vec<Value>,
        #[case] expected: Option<&str>,
    ) {
        assert_eq!(live(&turn("running", items, false)).as_deref(), expected);
    }

    #[rstest::rstest]
    fn nothing_is_live_once_the_turn_ends_or_while_it_asks(
        #[values("completed", "failed", "interrupted", "running")] status: &str,
    ) {
        let items = vec![user("u"), command("c", "running")];
        let requested = status == "running";
        assert_eq!(live(&turn(status, items, requested)), None);
    }

    #[rstest::rstest]
    fn screenshots_do_not_hide_work_started_after_them(
        #[values("completed", "unknown")] image_status: &str,
    ) {
        let screenshot = item(
            "image",
            image_status,
            json!({"imageView":{"path":"file:///tmp/blog.png"}}),
        );
        let projected = turn(
            "running",
            vec![
                user("u"),
                text("comment", "completed", "commentary"),
                command("first", "completed"),
                screenshot,
                command("later", "running"),
            ],
            false,
        );
        assert_eq!(live(&projected).as_deref(), Some("later"));
    }

    #[test]
    fn reasoning_reads_as_thinking_while_live_and_as_its_preview_collapsed() {
        let turn = turn(
            "running",
            vec![
                user("u"),
                reasoning(
                    "r",
                    "running",
                    &["**Inspecting** the\n\n`config`", "next  step"],
                ),
            ],
            false,
        );
        let Some(LiveActivity::Row(id)) = turn.live_activity() else {
            panic!("the reasoning is live")
        };
        let item = turn
            .rows
            .iter()
            .find_map(|row| match &row.content {
                ConversationRowContent::Activity { item, .. } if row.id == id => Some(item),
                _ => None,
            })
            .unwrap();
        let preview = Some("Inspecting the config next step".to_owned());
        assert_eq!(
            item.work_row_label(true),
            WorkRowLabel {
                heading: "思考中".into(),
                preview: preview.clone()
            }
        );
        assert_eq!(
            item.work_row_label(false),
            WorkRowLabel {
                heading: "思考".into(),
                preview
            }
        );
        // Expanded, the whole trace reads as Markdown, a paragraph per part.
        let blocks = item.detail_blocks();
        assert_eq!(blocks.len(), 3);
    }

    #[test]
    fn other_work_reads_as_its_title_live_or_not() {
        let turn = turn(
            "running",
            vec![user("u"), command("cargo test", "running")],
            false,
        );
        let item = turn
            .rows
            .iter()
            .find_map(|row| match &row.content {
                ConversationRowContent::Activity { item, .. } => Some(item),
                _ => None,
            })
            .unwrap();
        for live in [true, false] {
            assert_eq!(
                item.work_row_label(live),
                WorkRowLabel {
                    heading: "cargo test".into(),
                    preview: None
                }
            );
        }
    }

    #[test]
    fn a_group_of_only_reasoning_is_named_thought_and_mixed_work_by_its_tools() {
        let summary = |items| {
            turn("running", items, false)
                .rows
                .iter()
                .filter_map(|row| match &row.content {
                    ConversationRowContent::ActivityHeader { activity } => {
                        Some(activity.activity_summary.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        // An earlier segment of a running turn is named by what it did.
        let earlier = |work: Value| {
            vec![
                user("u"),
                work,
                text("a", "completed", "commentary"),
                command("later", "running"),
            ]
        };
        assert_eq!(
            summary(earlier(reasoning("r", "completed", &["plan"])))[0],
            "思考"
        );
        assert_eq!(
            summary(earlier(command("c", "completed")))[0],
            "コマンドを実行しました"
        );
    }
    #[test]
    fn system_notices_do_not_replace_the_live_work_or_become_work_themselves() {
        let notice = || {
            item(
                "notice",
                "completed",
                json!({"systemNotice":{"message":"Provider information"}}),
            )
        };
        assert_eq!(
            live(&turn(
                "running",
                vec![user("u"), command("build", "running"), notice()],
                false
            ))
            .as_deref(),
            Some("build")
        );
        let running = turn(
            "running",
            vec![user("u"), command("build", "running"), notice()],
            false,
        );
        let rows = super::super::turn_rows(
            crate::presentation::conversation::TimelineStyle::T3,
            &running,
            &Default::default(),
            &Default::default(),
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row, super::super::TurnRow::Live { row, .. }
            if matches!(&row.content, ConversationRowContent::Activity { item, .. }
                if item.data.native_id.as_ref().is_some_and(|id| id.as_str() == "build")))),
            "the native timeline keeps the actual command live before the notice"
        );
        assert_eq!(
            live(&turn("running", vec![user("u"), notice()], false)).as_deref(),
            Some("thinking")
        );
    }
}
