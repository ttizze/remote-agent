//! Conversation display policy shared by native desktop and mobile clients.
//! Grouping borrows item metadata; body formatting runs only for changed items
//! or explicit expansion. Native views own rendering and local expansion state.

pub mod connections;
pub mod diff;
pub mod error;
pub mod model_settings;
pub mod permissions;

use crate::models::{
    ApprovalReviewStatus, AssistantPhase, AttachmentKind, Item, ItemBody, ToolKind, Turn,
    TurnStatus,
};
use serde::Serialize;
pub mod body;

pub mod conversation;
pub mod list;
pub mod markdown;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupKind {
    Hidden,
    User,
    Assistant,
    Command,
    FileChange,
    Image,
    #[default]
    Activity,
}
/// Borrow only the values needed for grouping, including pending input.
#[derive(Clone, Copy, Default)]
pub struct ItemMetadata<'a> {
    pub id: &'a str,
    pub client_id: Option<&'a str>,
    pub kind: GroupKind,
    pub phase: AssistantPhase,
}
impl<'a> From<&'a Item> for ItemMetadata<'a> {
    fn from(item: &'a Item) -> Self {
        Self {
            id: &item.id,
            client_id: item.client_input_id.as_deref(),
            kind: match item.body() {
                ItemBody::UserMessage { .. } => GroupKind::User,
                ItemBody::AssistantText { .. } => GroupKind::Assistant,
                ItemBody::CommandExecution { .. } => GroupKind::Command,
                ItemBody::FileChange { .. } => GroupKind::FileChange,
                ItemBody::ImageGeneration { .. } => GroupKind::Image,
                ItemBody::Reasoning { .. } | ItemBody::Review { .. } | ItemBody::Sleep {} => {
                    GroupKind::Hidden
                }
                _ => GroupKind::Activity,
            },
            phase: match item.body() {
                ItemBody::AssistantText { phase, .. } => *phase,
                _ => AssistantPhase::Unknown,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    Hidden,
    User,
    Activity,
    Response,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: String,
    pub start: usize,
    pub end: usize,
    pub answer: Option<usize>,
    pub last: bool,
    pub collapsible: bool,
    pub initially_expanded: bool,
    pub label: Option<String>,
}
impl Segment {
    pub fn role(&self, index: usize, item: ItemMetadata<'_>) -> Role {
        match item.kind {
            GroupKind::Hidden => Role::Hidden,
            GroupKind::User => Role::User,
            GroupKind::Image => Role::Response,
            GroupKind::Assistant if self.answer.is_none_or(|answer| answer == index) => {
                Role::Response
            }
            _ => Role::Activity,
        }
    }
}

pub fn project(turn: &Turn) -> impl Iterator<Item = Segment> {
    let items = turn.items.as_deref().unwrap_or_default();
    project_items(turn, items.len(), move |index| items[index].as_ref().into())
}

/// The accessor lets clients project interleaved pending input without cloning
/// native item bodies. It must return the same item for an index throughout a call.
pub fn project_items<'a>(
    turn: &'a Turn,
    count: usize,
    item: impl Fn(usize) -> ItemMetadata<'a> + 'a,
) -> impl Iterator<Item = Segment> + 'a {
    let completed = turn.status == TurnStatus::Completed;
    let mut start = 0;
    let mut first = true;
    let mut exchange_end = 0;
    let mut exchange_has_answer = false;
    std::iter::from_fn(move || {
        if !first && start == count {
            return None;
        }
        first = false;
        if start == exchange_end {
            exchange_end = (start + 1..count)
                .find(|&index| item(index).kind == GroupKind::User)
                .unwrap_or(count);
            exchange_has_answer = completed
                && (start..exchange_end).any(|index| {
                    let value = item(index);
                    value.kind == GroupKind::Assistant && value.phase != AssistantPhase::Commentary
                });
        }
        let mut end = exchange_end;
        if !exchange_has_answer {
            let mut follows_response = false;
            for index in start..exchange_end {
                let value = item(index);
                if follows_response
                    && value.kind != GroupKind::Assistant
                    && value.kind != GroupKind::Hidden
                {
                    end = index;
                    break;
                }
                follows_response |= value.kind == GroupKind::Assistant;
            }
        }
        let answer = if completed {
            (start..end)
                .rev()
                .find(|&index| {
                    item(index).kind == GroupKind::Assistant
                        && item(index).phase == AssistantPhase::Final
                })
                .or_else(|| {
                    (start..end).rev().find(|&index| {
                        item(index).kind == GroupKind::Assistant
                            && item(index).phase == AssistantPhase::Unknown
                    })
                })
        } else {
            None
        };
        let id = if start == 0 {
            turn.id.to_string()
        } else {
            let first = item(start);
            format!("{}:{}", turn.id, first.client_id.unwrap_or(first.id))
        };
        let mut segment = Segment {
            id,
            start,
            end,
            answer,
            last: end == count,
            collapsible: false,
            initially_expanded: false,
            label: None,
        };
        segment.collapsible =
            (start..end).any(|index| segment.role(index, item(index)) == Role::Activity);
        segment.label = if segment.collapsible {
            Some(if answer.is_some() {
                let messages = (start..end)
                    .filter(|&index| segment.role(index, item(index)) == Role::Activity)
                    .count();
                format!("{messages}件の過去のメッセージ")
            } else {
                let summary = activity_summary(
                    (start..end)
                        .filter(|&index| segment.role(index, item(index)) == Role::Activity)
                        .map(&item),
                );
                if segment.last
                    && matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted)
                {
                    format!("{}・{summary}", work_summary(turn))
                } else {
                    summary
                }
            })
        } else if segment.last && !completed {
            Some(work_summary(turn))
        } else {
            None
        };
        start = end;
        Some(segment)
    })
}
fn activity_summary<'a>(items: impl Iterator<Item = ItemMetadata<'a>>) -> String {
    let (mut commands, mut files, mut tools) = (false, false, false);
    for item in items {
        match item.kind {
            GroupKind::Command => commands = true,
            GroupKind::FileChange => files = true,
            _ => tools = true,
        }
    }
    let labels: Vec<_> = [
        (commands, "コマンドを実行しました"),
        (files, "ファイルを変更しました"),
        (tools, "ツールを使用しました"),
    ]
    .into_iter()
    .filter_map(|(present, label)| present.then_some(label))
    .collect();
    if labels.is_empty() {
        "作業の詳細".into()
    } else {
        labels.join("、")
    }
}

fn work_summary(turn: &Turn) -> String {
    if turn.status == agent_protocol::execution::TurnStatus::Running {
        return "作業中…".into();
    }
    let duration = turn
        .duration_ms
        .or_else(|| turn.completed_at_ms?.checked_sub(turn.started_at_ms?));
    let mut summary = String::new();
    if let Some(ms) = duration {
        use std::fmt::Write;
        if ms < 1000 {
            summary.push_str("<1秒");
        } else {
            let seconds = ms / 1000;
            for (value, unit) in [
                (seconds / 3600, "時間"),
                (seconds / 60 % 60, "分"),
                (seconds % 60, "秒"),
            ] {
                if value == 0 {
                    continue;
                }
                if !summary.is_empty() {
                    summary.push(' ');
                }
                write!(summary, "{value}{unit}").unwrap();
            }
        }
        summary.push_str(" 作業");
    } else {
        summary.push_str("作業");
    }
    summary.push_str(match turn.status {
        TurnStatus::Interrupted => {
            if duration.is_some() {
                "した後に中断しました"
            } else {
                "を中断しました"
            }
        }
        TurnStatus::Failed => {
            if duration.is_some() {
                "した後に失敗しました"
            } else {
                "に失敗しました"
            }
        }
        _ => "しました",
    });
    summary
}

/// Place pending inputs after their saved native anchor. No anchor means before
/// the first item; an unloaded anchor goes at the tail. Repeated native IDs use
/// the latest occurrence, never duplicating
/// a pending input. Indices >= item_count refer to the pending input slice.
pub fn source_order<'a>(
    item_count: usize,
    item_id: impl Fn(usize) -> &'a str,
    anchors: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<usize> {
    let positions: Vec<_> = anchors
        .into_iter()
        .map(|anchor| {
            anchor.map_or(0, |anchor| {
                (0..item_count)
                    .rev()
                    .find(|&index| item_id(index) == anchor)
                    .map_or(item_count, |index| index + 1)
            })
        })
        .collect();
    let mut order = Vec::with_capacity(item_count + positions.len());
    for boundary in 0..=item_count {
        order.extend(
            positions
                .iter()
                .enumerate()
                .filter_map(|(pending, &position)| {
                    (position == boundary).then_some(item_count + pending)
                }),
        );
        if boundary < item_count {
            order.push(boundary);
        }
    }
    order
}

use agent_protocol::models::compact_title;
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPresentation {
    pub kind: &'static str,
    pub title: Option<String>,
    pub collapsible: bool,
}
pub fn item_presentation(
    item: &Item,
    provider: Option<crate::session::ProviderKind>,
) -> ItemPresentation {
    let (kind, title, collapsible) = match item.body() {
        ItemBody::UserMessage { .. } => ("user", Some("You".into()), false),
        ItemBody::AssistantText { phase, .. } => (
            if *phase == AssistantPhase::Commentary {
                "commentary"
            } else {
                "agent"
            },
            Some(
                match provider {
                    Some(crate::session::ProviderKind::Codex) => "Codex",
                    Some(crate::session::ProviderKind::Claude) => "Claude",
                    None => "Assistant",
                }
                .into(),
            ),
            false,
        ),
        ItemBody::Reasoning { .. } => ("reasoning", Some("作業の詳細".into()), true),
        ItemBody::CommandExecution { command, .. } => {
            ("command", Some(compact_title(command)), true)
        }
        ItemBody::FileChange { changes, .. } => (
            "fileChange",
            Some(format!("{}件のファイル変更", changes.len())),
            true,
        ),
        ItemBody::ImageGeneration { .. } => ("imageGeneration", None, false),
        _ => ("unknown", Some(tool_title(item.body())), true),
    };
    ItemPresentation {
        kind,
        title,
        collapsible,
    }
}
fn tool_title(body: &ItemBody) -> String {
    match body {
        ItemBody::Attachment { kind, .. } => match kind {
            AttachmentKind::HookResult => "フックの実行結果",
            AttachmentKind::FileEdit => "ファイルの編集内容",
            AttachmentKind::SessionUpdate => "セッションの更新情報",
            AttachmentKind::Other => "会話の添付情報",
        }
        .into(),
        ItemBody::Hook { .. } => "追加指示".into(),
        ItemBody::Plan { .. } => "計画を更新しました".into(),
        ItemBody::ToolCall {
            kind: ToolKind::Mcp,
            tool,
            server,
            ..
        } => match (server.as_deref().unwrap_or_default(), tool.as_str()) {
            ("", "") => "MCPツールを実行しました".into(),
            (server, "") => format!("{}の連携を使用しました", compact_title(server)),
            ("", tool) => compact_title(tool),
            (server, tool) => format!(
                "{}の連携を使用しました · {}",
                compact_title(server),
                compact_title(tool)
            ),
        },
        ItemBody::ToolCall { tool, .. } => {
            if tool.is_empty() {
                "ツールを実行しました".into()
            } else {
                format!("{}を実行しました", compact_title(tool))
            }
        }
        ItemBody::Subagent { tool, .. } => {
            if tool.is_empty() {
                "サブエージェントが作業しました".into()
            } else {
                format!("サブエージェント: {}", compact_title(tool))
            }
        }
        ItemBody::WebSearch { query, .. } => {
            if query.is_empty() {
                "Webを検索しました".into()
            } else {
                format!("Webを検索: {}", compact_title(query))
            }
        }
        ItemBody::ImageView { path } => {
            if path.is_empty() {
                "画像を確認しました".into()
            } else {
                format!("画像を確認: {}", compact_title(path))
            }
        }
        ItemBody::Compaction {} => "コンテキストを圧縮しました".into(),
        ItemBody::AutomaticApproval { status, .. } => match status {
            ApprovalReviewStatus::Running => "承認を自動確認中",
            ApprovalReviewStatus::Denied => "自動確認で拒否されました",
            ApprovalReviewStatus::TimedOut => "自動確認がタイムアウトしました",
            ApprovalReviewStatus::Aborted => "自動確認を中止しました",
            _ => "承認を自動確認しました",
        }
        .into(),
        ItemBody::Sleep {} => "待機しました".into(),
        ItemBody::Review { entering, .. } => if *entering {
            "レビューを開始しました"
        } else {
            "レビューを終了しました"
        }
        .into(),
        ItemBody::Custom { provider, kind, .. } => {
            format!("{provider:?} item ({})", compact_title(kind))
        }
        _ => unreachable!("messages use their own presentation"),
    }
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    use serde_json::json;
    macro_rules! turn {
        ($($value:tt)*) => { serde_json::from_value::<Turn>(json!($($value)*)).unwrap() };
    }

    #[rstest::rstest]
    fn generated_images_remain_visible_outside_completed_work(
        #[values("running", "completed", "failed")] status: &str,
    ) {
        let turn = turn!({"id":"turn","status":"completed","items":[{"id":"work","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":[]}}}}},{"id":"image","status":status,"clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":null,"data":null,"revisedPrompt":null}}}}},{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Here is the image","phase":"final"}}}}}]});
        let segment = project(&turn).next().unwrap();
        assert_eq!(
            segment.role(1, turn.items.as_ref().unwrap()[1].as_ref().into()),
            Role::Response
        );
        let presentation = item_presentation(
            turn.items.as_ref().unwrap()[1].as_ref(),
            Some(crate::session::ProviderKind::Codex),
        );
        assert!(!presentation.collapsible);
        assert!(presentation.title.is_none());
    }

    fn rows<'a>(part: &'a Segment, turn: &'a Turn, role: Role) -> impl Iterator<Item = &'a Item> {
        (part.start..part.end)
            .filter(move |&index| {
                part.role(index, turn.items.as_ref().unwrap()[index].as_ref().into()) == role
            })
            .map(move |index| turn.items.as_ref().unwrap()[index].as_ref())
    }
    #[test]
    fn completed_turn_projects_user_work_and_final() {
        let turn = turn!({"id":"turn","status":"completed","durationMs":40000,"items":[{"id":"u","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"r","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":[]}}}}},{"id":"c","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},{"id":"a","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"f","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"final"}}}}}]});
        let p = project(&turn).next().unwrap();
        assert_eq!(rows(&p, &turn, Role::User).count(), 1);
        assert_eq!(
            rows(&p, &turn, Role::Activity)
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a"]
        );
        assert_eq!(
            rows(&p, &turn, Role::Response).next().unwrap().id,
            "f".into()
        );
        assert!(p.collapsible);
        assert!(!p.initially_expanded);
        assert_eq!(p.label.as_deref(), Some("2件の過去のメッセージ"));
    }
    #[test]
    fn completed_empty_turn_does_not_restore_thinking() {
        let turn = turn!({"id":"turn","status":"completed","items":[{"id":"u","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"追加メッセージ","content":[]}}}}}]});
        assert!(project(&turn).next().unwrap().label.is_none());
    }
    fn order(turn: &Turn) -> Vec<String> {
        project(turn)
            .flat_map(|part| {
                rows(&part, turn, Role::User)
                    .chain(rows(&part, turn, Role::Activity))
                    .chain(rows(&part, turn, Role::Response))
                    .map(|item| item.id.to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    #[rstest::rstest]
    #[case::live("running", "running")]
    #[case::live_completed_command("running", "completed")]
    #[case::live_failed_command("running", "failed")]
    #[case::completed("completed", "completed")]
    #[case::failed("failed", "failed")]
    #[case::interrupted("interrupted", "running")]
    #[case::missing_status("unknown", "completed")]
    fn command_groups_start_collapsed_for_live_and_restored_turns(
        #[case] status: &str,
        #[case] command_status: &str,
    ) {
        let turn = turn!({"id":"turn","status":status,"items":[{"id":"u","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"Inspect","content":[]}}}}},{"id":"c","status":command_status,"clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"pwd","cwd":null,"output":"","exitCode":null}}}}},{"id":"a","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Result","phase":"unknown"}}}}}]});
        let group = project(&turn).find(|part| part.collapsible).unwrap();
        assert!(
            !group.initially_expanded,
            "command group opened without a user action: turn={status}, command={command_status}"
        );
        assert_eq!(
            rows(&group, &turn, Role::Activity).next().unwrap().id,
            "c".into()
        );
        assert!(group.label.is_some());
    }

    #[test]
    fn live_commentary_separates_activity_groups_without_reordering() {
        let turn = turn!({"id":"turn","status":"running","items":[{"id":"u","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"intro","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"c1","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},{"id":"c2","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},{"id":"progress","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"tool","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"toolCall":{"kind":"mcp","tool":"","server":null,"namespace":null,"arguments":null,"result":null,"error":null,"content":[],"success":null,"durationMs":null}}}}},{"id":"followup","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"r","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":[]}}}}}]});
        assert_eq!(
            order(&turn),
            ["u", "intro", "c1", "c2", "progress", "tool", "followup"]
        );
        assert!(project(&turn).all(|part| !part.initially_expanded));
        let labels: Vec<_> = project(&turn)
            .filter(|part| part.collapsible)
            .map(|part| part.label.unwrap())
            .collect();
        assert_eq!(labels, ["コマンドを実行しました", "ツールを使用しました"]);
    }
    #[rstest::rstest]
    fn completed_exchanges_keep_each_answer_beside_its_question(
        #[values(AssistantPhase::Unknown, AssistantPhase::Final)] phase: AssistantPhase,
    ) {
        let turn = turn!({"id":"turn","status":"completed","durationMs":1459000,"items":[{"id":"u1","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"progress","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"f1","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":phase}}}}},{"id":"u2","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"c","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},{"id":"f2","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":phase}}}}}]});
        assert_eq!(order(&turn), ["u1", "progress", "f1", "u2", "c", "f2"]);
        let parts: Vec<_> = project(&turn).collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            rows(&parts[0], &turn, Role::Response).next().unwrap().id,
            "f1".into()
        );
        assert_eq!(
            rows(&parts[1], &turn, Role::Response).next().unwrap().id,
            "f2".into()
        );
        assert_eq!(parts[0].label.as_deref(), Some("1件の過去のメッセージ"));
        assert_eq!(parts[1].label.as_deref(), Some("1件の過去のメッセージ"));
    }
    #[test]
    fn later_answer_does_not_hide_earlier_unanswered_commentary() {
        let turn = turn!({"id":"turn","status":"completed","items":[{"id":"u1","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"a1","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"c","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"","exitCode":null}}}}},{"id":"a2","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"u2","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}},{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"final"}}}}}]});
        assert_eq!(order(&turn), ["u1", "a1", "c", "a2", "u2", "answer"]);
        let responses: Vec<_> = project(&turn)
            .flat_map(|part| {
                rows(&part, &turn, Role::Response)
                    .map(|item| item.id.to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(responses, ["a1", "a2", "answer"]);
    }
    #[test]
    fn failures_and_interruptions_keep_partial_responses_and_status() {
        for (status, expected) in [
            ("failed", "12秒 作業した後に失敗しました"),
            ("interrupted", "12秒 作業した後に中断しました"),
        ] {
            let turn = turn!({"id":"turn","status":status,"durationMs":12000,"items":[{"id":"a","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"r","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":[]}}}}}]});
            assert_eq!(order(&turn), ["a"]);
            let last = project(&turn).last().unwrap();
            assert!(last.last && !last.collapsible);
            assert_eq!(last.label.as_deref(), Some(expected));
        }
    }
    #[test]
    fn hidden_lifecycle_items_do_not_split_visible_responses() {
        let turn = turn!({"id":"turn","status":"running","items":[{"id":"a","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}},{"id":"sleep","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"sleep":{}}}}},{"id":"review","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"review":{"entering":false,"text":""}}}}},{"id":"b","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"commentary"}}}}}]});
        assert_eq!(project(&turn).count(), 1);
        assert_eq!(order(&turn), ["a", "b"]);
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use serde_json::json;
    macro_rules! turn {
        ($($value:tt)*) => { serde_json::from_value::<Turn>(json!($($value)*)).unwrap() };
    }
    #[test]
    fn pending_inputs_anchor_after_latest_occurrence() {
        let ids = ["first", "repeat", "repeat", "later"];
        assert_eq!(
            source_order(
                ids.len(),
                |i| ids[i],
                [Some("repeat"), Some("missing"), Some("repeat"), None]
            ),
            [7, 0, 1, 2, 4, 6, 3, 5]
        );
    }
    #[test]
    fn duplicate_native_ids_keep_distinct_response_and_activity_indices() {
        let turn = turn!({"id":"turn","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old","phase":"unknown"}}}}},{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"final"}}}}}]});
        let segment = project(&turn).next().unwrap();
        assert_eq!(
            segment.role(0, turn.items.as_ref().unwrap()[0].as_ref().into()),
            Role::Activity
        );
        assert_eq!(
            segment.role(1, turn.items.as_ref().unwrap()[1].as_ref().into()),
            Role::Response
        );
        assert!(!segment.initially_expanded);
    }
    #[test]
    fn pending_metadata_preserves_source_positions() {
        let turn = turn!({"id":"t","status":"running","items":[{"id":"a","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"private body","phase":"commentary"}}}}},{"id":"b","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":null,"output":"private output","exitCode":null}}}}}]});
        let items = turn.items.as_ref().unwrap();
        let order = source_order(items.len(), |i| &items[i].id, [Some("a")]);
        let item = |index: usize| {
            let source = order[index];
            if source < items.len() {
                items[source].as_ref().into()
            } else {
                ItemMetadata {
                    id: "p",
                    client_id: Some("p"),
                    kind: GroupKind::User,
                    ..Default::default()
                }
            }
        };
        let segments: Vec<_> = project_items(&turn, order.len(), item).collect();
        let rows: Vec<_> = segments
            .iter()
            .flat_map(|s| (s.start..s.end).map(|index| (order[index], s.role(index, item(index)))))
            .collect();
        assert_eq!(
            rows,
            [(0, Role::Response), (2, Role::User), (1, Role::Activity)]
        );
    }
    #[test]
    fn titles_keep_one_bounded_unicode_line() {
        let command: Item = serde_json::from_value(json!({"id":"command","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"  cargo test\nsecret second line","cwd":null,"output":"","exitCode":null}}}}})).unwrap();
        assert_eq!(
            item_presentation(&command, Some(crate::session::ProviderKind::Codex))
                .title
                .as_deref(),
            Some("cargo test")
        );
        let command: Item = serde_json::from_value(
            json!({"id":"command","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"日".repeat(121),"cwd":null,"output":"","exitCode":null}}}}}),
        )
        .unwrap();
        let title = item_presentation(&command, Some(crate::session::ProviderKind::Codex))
            .title
            .unwrap();
        assert_eq!(title.chars().count(), 121);
        assert!(title.ends_with('…'));
    }
}

#[cfg(test)]
mod deferred_item_tests {
    use super::*;
    use crate::models::ItemStatus;
    #[test]
    fn deferred_command_keeps_its_display_title_without_hidden_output() {
        for command in [
            format!("{}\nhidden script", "日本語".repeat(60)),
            "cargo check\nhidden script".into(),
        ] {
            let mut item = Item::new(
                "command".into(),
                ItemStatus::Completed,
                ItemBody::CommandExecution {
                    command,
                    cwd: None,
                    output: "hidden output".repeat(100),
                    exit_code: None,
                },
            );
            let title = crate::presentation::item_presentation(
                &item,
                Some(crate::session::ProviderKind::Codex),
            )
            .title
            .unwrap();
            item.defer();
            assert_eq!(
                crate::presentation::item_presentation(
                    &item,
                    Some(crate::session::ProviderKind::Codex)
                )
                .title
                .as_deref(),
                Some(title.as_str())
            );
            assert!(item.is_deferred());
            let ItemBody::CommandExecution {
                command, output, ..
            } = item.body()
            else {
                panic!("command body")
            };
            assert_eq!(command, &title);
            assert!(output.is_empty());
        }
    }
}
