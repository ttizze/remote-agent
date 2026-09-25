//! Conversation display policy shared by native desktop and mobile clients.
//! Grouping borrows item metadata; body formatting runs only for changed items
//! or explicit expansion. Native views own rendering and local expansion state.

pub mod diff;
pub mod error;
pub mod model_settings;

use crate::models::{Item, Turn};
use serde::Serialize;
use serde_json::Value;
pub mod body;

pub mod conversation;
pub mod list;
pub mod markdown;

/// Borrow only the fields used for grouping; pending input can supply metadata
/// without allocating a native item or copying its message and attachments.
#[derive(Clone, Copy, Default)]
pub struct ItemMetadata<'a> {
    pub id: &'a str,
    pub client_id: Option<&'a str>,
    pub kind: &'a str,
    pub phase: Option<&'a str>,
    pub file_count: usize,
}
impl<'a> From<&'a Item> for ItemMetadata<'a> {
    fn from(item: &'a Item) -> Self {
        Self {
            id: &item.id,
            client_id: item.client_id.as_deref(),
            kind: item.kind.as_deref().unwrap_or_default(),
            phase: item.phase.as_deref(),
            file_count: item
                .changes
                .as_ref()
                .map_or(0, crate::models::ItemChanges::len),
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
            "reasoning" | "sleep" | "enteredReviewMode" | "exitedReviewMode" => Role::Hidden,
            "userMessage" => Role::User,
            "imageGeneration" => Role::Response,
            "agentMessage" if self.answer.is_none_or(|answer| answer == index) => Role::Response,
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
    let completed = turn.status.as_deref() == Some("completed");
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
                .find(|&index| item(index).kind == "userMessage")
                .unwrap_or(count);
            exchange_has_answer = completed
                && (start..exchange_end).any(|index| {
                    let value = item(index);
                    value.kind == "agentMessage" && value.phase != Some("commentary")
                });
        }
        let mut end = exchange_end;
        if !exchange_has_answer {
            let mut follows_response = false;
            for index in start..exchange_end {
                let value = item(index);
                if follows_response && value.kind != "agentMessage" && visible(value) {
                    end = index;
                    break;
                }
                follows_response |= value.kind == "agentMessage";
            }
        }
        let answer = if completed {
            (start..end)
                .rev()
                .find(|&index| {
                    item(index).kind == "agentMessage" && item(index).phase == Some("final_answer")
                })
                .or_else(|| {
                    (start..end).rev().find(|&index| {
                        item(index).kind == "agentMessage" && item(index).phase.is_none()
                    })
                })
        } else {
            None
        };
        let id = if start == 0 {
            turn.id.clone()
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
                    && matches!(
                        turn.status.as_deref().unwrap_or_default(),
                        "failed" | "interrupted"
                    )
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
fn visible(item: ItemMetadata<'_>) -> bool {
    !matches!(
        item.kind,
        "reasoning" | "sleep" | "enteredReviewMode" | "exitedReviewMode"
    )
}
fn activity_summary<'a>(items: impl Iterator<Item = ItemMetadata<'a>>) -> String {
    let (mut commands, mut files, mut tools) = (false, false, false);
    for item in items {
        match item.kind {
            "commandExecution" => commands = true,
            "fileChange" => files = true,
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
    if turn.status.as_deref() == Some("inProgress") {
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
    summary.push_str(match turn.status.as_deref().unwrap_or_default() {
        "interrupted" => {
            if duration.is_some() {
                "した後に中断しました"
            } else {
                "を中断しました"
            }
        }
        "failed" => {
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
    pub title: String,
    pub collapsible: bool,
    pub visible: bool,
}
pub fn item_presentation(item: &Item) -> ItemPresentation {
    let (kind, title, collapsible) = match item.kind.as_deref().unwrap_or_default() {
        "userMessage" => ("user", "You".into(), false),
        "agentMessage" => (
            if item.phase.as_deref().unwrap_or_default() == "commentary" {
                "commentary"
            } else {
                "agent"
            },
            "Codex".into(),
            false,
        ),
        "reasoning" => ("reasoning", "作業の詳細".into(), true),
        "imageGeneration" => ("imageGeneration", tool_title(item), false),
        "commandExecution" => (
            "command",
            compact_title(item.command.as_deref().unwrap_or_default()),
            true,
        ),
        "fileChange" => (
            "fileChange",
            format!("{}件のファイル変更", ItemMetadata::from(item).file_count),
            true,
        ),
        _ => ("unknown", tool_title(item), true),
    };
    ItemPresentation {
        kind,
        title,
        collapsible,
        visible: visible(item.into()),
    }
}
fn tool_title(item: &Item) -> String {
    let tool = item.tool.as_deref().unwrap_or_default();
    match item.kind.as_deref().unwrap_or_default() {
        "nativeAttachment" => match item
            .result
            .as_ref()
            .and_then(|value| value["type"].as_str())
        {
            Some(
                "hook_success" | "hook_error" | "hook_non_blocking_error" | "hook_blocking_error",
            ) => "フックの実行結果".into(),
            Some("edited_text_file") => "ファイルの編集内容".into(),
            Some("remote_session_change") => "セッションの更新情報".into(),
            _ => "会話の添付情報".into(),
        },
        "hookPrompt" => "追加指示".into(),
        "plan" => "計画を更新しました".into(),
        "mcpToolCall" => match (item.server.as_deref().unwrap_or_default(), tool) {
            ("", "") => "MCPツールを実行しました".into(),
            (server, "") => format!("{}の連携を使用しました", compact_title(server)),
            ("", tool) => compact_title(tool),
            (server, tool) => format!(
                "{}の連携を使用しました · {}",
                compact_title(server),
                compact_title(tool)
            ),
        },
        "dynamicToolCall" => {
            if tool.is_empty() {
                "ツールを実行しました".into()
            } else {
                format!("{}を実行しました", compact_title(tool))
            }
        }
        "collabAgentToolCall" => {
            if tool.is_empty() {
                "サブエージェントを操作しました".into()
            } else {
                format!("サブエージェント: {}", compact_title(tool))
            }
        }
        "subAgentActivity" => "サブエージェントが作業しました".into(),
        "webSearch" => match item.query.as_deref().unwrap_or_default() {
            "" => "Webを検索しました".into(),
            query => format!("Webを検索: {}", compact_title(query)),
        },
        "imageView" => match item.path.as_deref().unwrap_or_default() {
            "" => "画像を確認しました".into(),
            path => format!("画像を確認: {}", compact_title(path)),
        },
        "imageGeneration" => match item.status.as_deref().unwrap_or_default() {
            "inProgress" => "画像を生成中…",
            "failed" => "画像を生成できませんでした",
            _ => "生成画像",
        }
        .into(),
        "contextCompaction" => "コンテキストを圧縮しました".into(),
        "automaticApprovalReview" => match item
            .review
            .as_ref()
            .and_then(|review| review.get("status"))
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "inProgress" => "承認を自動確認中",
            "denied" => "自動確認で拒否されました",
            "timedOut" => "自動確認がタイムアウトしました",
            "aborted" => "自動確認を中止しました",
            _ => "承認を自動確認しました",
        }
        .into(),
        "sleep" => "待機しました".into(),
        "enteredReviewMode" => "レビューを開始しました".into(),
        "exitedReviewMode" => "レビューを終了しました".into(),
        kind => format!("Codex item ({})", compact_title(kind)),
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
        #[values("inProgress", "completed", "failed")] status: &str,
    ) {
        let turn = turn!({"id":"turn","status":"completed","items":[
            {"id":"work","type":"reasoning"},
            {"id":"image","type":"imageGeneration","status":status},
            {"id":"answer","type":"agentMessage","phase":"final_answer","text":"Here is the image"}
        ]});
        let segment = project(&turn).next().unwrap();
        assert_eq!(
            segment.role(1, turn.items.as_ref().unwrap()[1].as_ref().into()),
            Role::Response
        );
        assert!(!item_presentation(turn.items.as_ref().unwrap()[1].as_ref()).collapsible);
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
        let turn = turn!({"id":"turn","status":"completed","durationMs":40000,"items":[{"id":"u","type":"userMessage"},{"id":"r","type":"reasoning"},{"id":"c","type":"commandExecution"},{"id":"a","type":"agentMessage","phase":"commentary"},{"id":"f","type":"agentMessage","phase":"final_answer"}]});
        let p = project(&turn).next().unwrap();
        assert_eq!(rows(&p, &turn, Role::User).count(), 1);
        assert_eq!(
            rows(&p, &turn, Role::Activity)
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "a"]
        );
        assert_eq!(rows(&p, &turn, Role::Response).next().unwrap().id, "f");
        assert!(p.collapsible);
        assert!(!p.initially_expanded);
        assert_eq!(p.label.as_deref(), Some("2件の過去のメッセージ"));
    }
    #[test]
    fn completed_empty_turn_does_not_restore_thinking() {
        let turn = turn!({"id":"turn","status":"completed","items":[{"id":"u","type":"userMessage","text":"追加メッセージ"}]});
        assert!(project(&turn).next().unwrap().label.is_none());
    }
    fn order(turn: &Turn) -> Vec<String> {
        project(turn)
            .flat_map(|part| {
                rows(&part, turn, Role::User)
                    .chain(rows(&part, turn, Role::Activity))
                    .chain(rows(&part, turn, Role::Response))
                    .map(|item| item.id.clone())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    #[rstest::rstest]
    #[case::live("inProgress", "inProgress")]
    #[case::live_completed_command("inProgress", "completed")]
    #[case::live_failed_command("inProgress", "failed")]
    #[case::completed("completed", "completed")]
    #[case::failed("failed", "failed")]
    #[case::interrupted("interrupted", "inProgress")]
    #[case::missing_status("", "completed")]
    fn command_groups_start_collapsed_for_live_and_restored_turns(
        #[case] status: &str,
        #[case] command_status: &str,
    ) {
        let turn = turn!({"id":"turn","status":status,"items":[
            {"id":"u","type":"userMessage","text":"Inspect"},
            {"id":"c","type":"commandExecution","command":"pwd","status":command_status},
            {"id":"a","type":"agentMessage","text":"Result"}
        ]});
        let group = project(&turn).find(|part| part.collapsible).unwrap();
        assert!(
            !group.initially_expanded,
            "command group opened without a user action: turn={status}, command={command_status}"
        );
        assert_eq!(rows(&group, &turn, Role::Activity).next().unwrap().id, "c");
        assert!(group.label.is_some());
    }

    #[test]
    fn live_commentary_separates_activity_groups_without_reordering() {
        let turn = turn!({"id":"turn","status":"inProgress","items":[
            {"id":"u","type":"userMessage"},
            {"id":"intro","type":"agentMessage","phase":"commentary"},
            {"id":"c1","type":"commandExecution"},
            {"id":"c2","type":"commandExecution"},
            {"id":"progress","type":"agentMessage","phase":"commentary"},
            {"id":"tool","type":"mcpToolCall"},
            {"id":"followup","type":"userMessage"},
            {"id":"r","type":"reasoning"}
        ]});
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
        #[values(Value::Null, json!("final_answer"))] phase: Value,
    ) {
        let turn = turn!({"id":"turn","status":"completed","durationMs":1459000,"items":[
            {"id":"u1","type":"userMessage"},
            {"id":"progress","type":"agentMessage","phase":"commentary"},
            {"id":"f1","type":"agentMessage","phase":phase},
            {"id":"u2","type":"userMessage"},
            {"id":"c","type":"commandExecution"},
            {"id":"f2","type":"agentMessage","phase":phase}
        ]});
        assert_eq!(order(&turn), ["u1", "progress", "f1", "u2", "c", "f2"]);
        let parts: Vec<_> = project(&turn).collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(
            rows(&parts[0], &turn, Role::Response).next().unwrap().id,
            "f1"
        );
        assert_eq!(
            rows(&parts[1], &turn, Role::Response).next().unwrap().id,
            "f2"
        );
        assert_eq!(parts[0].label.as_deref(), Some("1件の過去のメッセージ"));
        assert_eq!(parts[1].label.as_deref(), Some("1件の過去のメッセージ"));
    }
    #[test]
    fn later_answer_does_not_hide_earlier_unanswered_commentary() {
        let turn = turn!({"id":"turn","status":"completed","items":[
            {"id":"u1","type":"userMessage"},
            {"id":"a1","type":"agentMessage","phase":"commentary"},
            {"id":"c","type":"commandExecution"},
            {"id":"a2","type":"agentMessage","phase":"commentary"},
            {"id":"u2","type":"userMessage"},
            {"id":"answer","type":"agentMessage","phase":"final_answer"}
        ]});
        assert_eq!(order(&turn), ["u1", "a1", "c", "a2", "u2", "answer"]);
        let responses: Vec<_> = project(&turn)
            .flat_map(|part| {
                rows(&part, &turn, Role::Response)
                    .map(|item| item.id.clone())
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
            let turn = turn!({"id":"turn","status":status,"durationMs":12000,"items":[
                {"id":"a","type":"agentMessage","phase":"commentary"},
                {"id":"r","type":"reasoning"}
            ]});
            assert_eq!(order(&turn), ["a"]);
            let last = project(&turn).last().unwrap();
            assert!(last.last && !last.collapsible);
            assert_eq!(last.label.as_deref(), Some(expected));
        }
    }
    #[test]
    fn hidden_lifecycle_items_do_not_split_visible_responses() {
        let turn = turn!({"id":"turn","status":"inProgress","items":[
            {"id":"a","type":"agentMessage","phase":"commentary"},
            {"id":"sleep","type":"sleep"},
            {"id":"review","type":"exitedReviewMode"},
            {"id":"b","type":"agentMessage","phase":"commentary"}
        ]});
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
        let turn = turn!({"id":"turn","status":"completed","items":[
            {"id":"same","type":"agentMessage","text":"old"},
            {"id":"same","type":"agentMessage","phase":"final_answer","text":"new"}
        ]});
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
        let turn = turn!({"id":"t","status":"inProgress","items":[
            {"id":"a","type":"agentMessage","phase":"commentary","text":"private body"},
            {"id":"b","type":"commandExecution","aggregatedOutput":"private output"}
        ]});
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
                    kind: "userMessage",
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
        let command: Item = serde_json::from_value(json!({"id":"command","type":"commandExecution","command":"  cargo test\nsecret second line"})).unwrap();
        assert_eq!(item_presentation(&command).title, "cargo test");
        let command: Item = serde_json::from_value(
            json!({"id":"command","type":"commandExecution","command":"日".repeat(121)}),
        )
        .unwrap();
        let title = item_presentation(&command).title;
        assert_eq!(title.chars().count(), 121);
        assert!(title.ends_with('…'));
    }
}

#[cfg(test)]
mod deferred_item_tests {
    use super::*;
    #[test]
    fn deferred_command_keeps_its_display_title_without_hidden_output() {
        for command in [
            format!("{}\nhidden script", "日本語".repeat(60)),
            "cargo check\nhidden script".into(),
        ] {
            let mut item = Item {
                kind: Some("commandExecution".into()),
                command: Some(command),
                aggregated_output: Some("hidden output".repeat(100)),
                ..Default::default()
            };
            let title = crate::presentation::item_presentation(&item).title;
            item.retain_header();
            assert_eq!(crate::presentation::item_presentation(&item).title, title);
            assert_eq!(item.command.as_deref(), Some(title.as_str()));
            assert_eq!(item.aggregated_output, None);
        }
    }
}
