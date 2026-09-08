//! Conversation display policy shared by native desktop and mobile clients.
//! Inputs may be native Codex values or metadata-only values. Message bodies
//! are never read; projections identify source items by index, including when
//! native IDs repeat. Rendering and local expansion state belong to each UI.

use serde::Serialize;
use serde_json::{Value, json};
pub mod history;
pub mod state;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
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
    pub fn role(&self, index: usize, item: &Value) -> Role {
        match text(item, "type") {
            "sleep" | "enteredReviewMode" | "exitedReviewMode" => Role::Hidden,
            "userMessage" => Role::User,
            "imageGeneration" => Role::Response,
            "agentMessage" if self.answer.is_none_or(|answer| answer == index) => Role::Response,
            _ => Role::Activity,
        }
    }
}

pub fn project(turn: &Value) -> impl Iterator<Item = Segment> {
    let items = array(&turn["items"]);
    project_items(turn, items.len(), move |index| &items[index])
}

/// The accessor lets clients project interleaved pending input without cloning
/// native item bodies. It must return the same item for an index throughout a call.
pub fn project_items<'a>(
    turn: &'a Value,
    count: usize,
    item: impl Fn(usize) -> &'a Value + 'a,
) -> impl Iterator<Item = Segment> + 'a {
    let completed = turn["status"] == "completed";
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
                .find(|&index| item(index)["type"] == "userMessage")
                .unwrap_or(count);
            exchange_has_answer = completed
                && (start..exchange_end).any(|index| {
                    let value = item(index);
                    value["type"] == "agentMessage" && value["phase"] != "commentary"
                });
        }
        let mut end = exchange_end;
        if !exchange_has_answer {
            let mut follows_response = false;
            for index in start..exchange_end {
                let value = item(index);
                if follows_response && value["type"] != "agentMessage" && visible(value) {
                    end = index;
                    break;
                }
                follows_response |= value["type"] == "agentMessage";
            }
        }
        let answer = if completed {
            (start..end)
                .rev()
                .find(|&index| {
                    item(index)["type"] == "agentMessage" && item(index)["phase"] == "final_answer"
                })
                .or_else(|| {
                    (start..end).rev().find(|&index| {
                        item(index)["type"] == "agentMessage" && item(index)["phase"].is_null()
                    })
                })
        } else {
            None
        };
        let id = if start == 0 {
            text(turn, "id").to_owned()
        } else {
            let first = item(start);
            format!(
                "{}:{}",
                text(turn, "id"),
                first["clientId"]
                    .as_str()
                    .unwrap_or_else(|| text(first, "id"))
            )
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
                if segment.last {
                    work_summary(turn)
                } else {
                    "作業内容".into()
                }
            } else {
                let summary = activity_summary(
                    (start..end)
                        .filter(|&index| segment.role(index, item(index)) == Role::Activity)
                        .map(&item),
                );
                if segment.last && matches!(text(turn, "status"), "failed" | "interrupted") {
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
fn visible(item: &Value) -> bool {
    !matches!(
        text(item, "type"),
        "sleep" | "enteredReviewMode" | "exitedReviewMode"
    )
}
fn activity_summary<'a>(items: impl Iterator<Item = &'a Value>) -> String {
    let (mut commands, mut files, mut tools, mut reasoning) = (0, 0, 0, false);
    for item in items {
        match text(item, "type") {
            "commandExecution" => commands += 1,
            "fileChange" => files += file_count(item),
            "reasoning" => reasoning = true,
            _ => tools += 1,
        }
    }
    use std::fmt::Write;
    let mut summary = String::new();
    for (count, kind) in [
        (commands, "コマンド"),
        (files, "ファイル変更"),
        (tools, "ツール操作"),
    ] {
        if count == 0 {
            continue;
        }
        if !summary.is_empty() {
            summary.push('、');
        }
        write!(summary, "{count}件の{kind}").unwrap();
    }
    if reasoning {
        if !summary.is_empty() {
            summary.push('、');
        }
        summary.push_str("思考");
    }
    if summary.is_empty() {
        summary.push_str("作業");
    }
    summary
}
fn file_count(item: &Value) -> usize {
    item["fileCount"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or_else(|| array(&item["changes"]).len())
}
fn work_summary(turn: &Value) -> String {
    if turn["status"] == "inProgress" {
        return "作業中…".into();
    }
    let duration = turn["durationMs"].as_u64().or_else(|| {
        turn["completedAtMs"]
            .as_u64()?
            .checked_sub(turn["startedAtMs"].as_u64()?)
    });
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
    summary.push_str(match text(turn, "status") {
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

/// Keep one pending entry per client ID until the native echo arrives.
/// Returned indices refer to the input, so clients retain their own bodies.
pub fn remaining_submissions<'a>(
    pending: &[&str],
    echoed: impl IntoIterator<Item = &'a str>,
) -> Vec<usize> {
    // Storage scales with pending input, not with the conversation history.
    let mut retained: Vec<_> = pending
        .iter()
        .enumerate()
        .filter_map(|(index, id)| (!pending[..index].contains(id)).then_some(index))
        .collect();
    for echo in echoed {
        retained.retain(|&index| pending[index] != echo);
        if retained.is_empty() {
            break;
        }
    }
    retained
}

/// Place pending inputs after their saved native anchor. An absent anchor goes
/// at the tail; repeated native IDs use the latest occurrence, never duplicating
/// a pending input. Indices >= item_count refer to the pending input slice.
pub fn source_order<'a>(
    item_count: usize,
    item_id: impl Fn(usize) -> &'a str,
    anchors: &[Option<&str>],
) -> Vec<usize> {
    let positions: Vec<_> = anchors
        .iter()
        .map(|anchor| {
            anchor.and_then(|anchor| {
                (0..item_count)
                    .rev()
                    .find(|&index| item_id(index) == anchor)
            })
        })
        .collect();
    let mut order = Vec::with_capacity(item_count + anchors.len());
    for index in 0..item_count {
        order.push(index);
        order.extend(positions.iter().enumerate().filter_map(|(pending, after)| {
            (*after == Some(index)).then_some(item_count + pending)
        }));
    }
    order.extend(
        positions
            .iter()
            .enumerate()
            .filter_map(|(pending, after)| after.is_none().then_some(item_count + pending)),
    );
    order
}

fn compact_title(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemPresentation {
    pub kind: &'static str,
    pub title: String,
    pub collapsible: bool,
    pub visible: bool,
}
pub fn item_presentation(item: &Value) -> ItemPresentation {
    let (kind, title, collapsible) = match text(item, "type") {
        "userMessage" => ("user", "You".into(), false),
        "agentMessage" => (
            if item["phase"] == "commentary" {
                "commentary"
            } else {
                "agent"
            },
            "Codex".into(),
            false,
        ),
        "reasoning" => ("reasoning", "思考".into(), true),
        "imageGeneration" => ("imageGeneration", tool_title(item), false),
        "commandExecution" => ("command", compact_title(text(item, "command")), true),
        "fileChange" => (
            "fileChange",
            format!("{}件のファイル変更", file_count(item)),
            true,
        ),
        _ => ("unknown", tool_title(item), true),
    };
    ItemPresentation {
        kind,
        title,
        collapsible,
        visible: visible(item),
    }
}
fn tool_title(item: &Value) -> String {
    let tool = text(item, "tool");
    match text(item, "type") {
        "hookPrompt" => "追加指示".into(),
        "plan" => "計画を更新しました".into(),
        "mcpToolCall" => match (text(item, "server"), tool) {
            ("", "") => "MCPツールを実行しました".into(),
            (server, "") => compact_title(server),
            ("", tool) => compact_title(tool),
            (server, tool) => format!("{} / {}", compact_title(server), compact_title(tool)),
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
        "webSearch" => match text(item, "query") {
            "" => "Webを検索しました".into(),
            query => format!("Webを検索: {}", compact_title(query)),
        },
        "imageView" => match text(item, "path") {
            "" => "画像を確認しました".into(),
            path => format!("画像を確認: {}", compact_title(path)),
        },
        "imageGeneration" => match text(item, "status") {
            "inProgress" => "画像を生成中…",
            "failed" => "画像を生成できませんでした",
            _ => "生成画像",
        }
        .into(),
        "contextCompaction" => "コンテキストを圧縮しました".into(),
        "automaticApprovalReview" => match text(&item["review"], "status") {
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

/// Metadata-only bridge used by C/JNI clients. Output contains source indices,
/// display policy and labels, never source message bodies or tool output.
pub fn present_json(request: &str) -> Result<String, String> {
    let mut request: Value = serde_json::from_str(request).map_err(|e| e.to_string())?;
    let result = match text(&request, "operation") {
        "watchPath" => serde_json::to_value(state::watch_path(&request["thread"]))
            .map_err(|e| e.to_string())?,
        "sendPlan" => {
            serde_json::to_value(state::plan_send(&request["snapshot"], &request["listed"]))
                .map_err(|e| e.to_string())?
        }
        "turnLifecycle" => {
            let kind = if request["started"] == true {
                state::EventKind::TurnStarted
            } else {
                state::EventKind::TurnCompleted
            };
            state::merge_lifecycle(request["previous"].take(), request["incoming"].take(), kind)
        }
        "historyRefresh" => {
            let (thread, result) =
                history::merge_refresh(request["previous"].take(), request["incoming"].take());
            result?;
            thread
        }
        "historyOlder" => {
            let page = serde_json::json!({"thread":request["incoming"].take()});
            let (thread, result) = history::merge_older(
                request["previous"].take(),
                page,
                request["turnId"].as_str(),
                &request["cursor"],
            );
            result?;
            thread
        }
        "turn" => {
            let turn = &request["turn"];
            let items = turn["items"]
                .as_array()
                .ok_or("turn.items must be an array")?;
            let pending = array(&request["pending"]);
            let anchors: Vec<_> = pending
                .iter()
                .map(|value| value["afterItemId"].as_str())
                .collect();
            let order = source_order(items.len(), |index| text(&items[index], "id"), &anchors);
            let item = |index: usize| {
                let source = order[index];
                if source < items.len() {
                    &items[source]
                } else {
                    &pending[source - items.len()]["item"]
                }
            };
            let segments: Vec<_> = project_items(turn, order.len(), item).map(|segment| {
                let rows: Vec<_> = (segment.start..segment.end).map(|index| json!({"source":order[index],"role":segment.role(index, item(index))})).collect();
                json!({"id":segment.id,"last":segment.last,"collapsible":segment.collapsible,"initiallyExpanded":segment.initially_expanded,"label":segment.label,"rows":rows})
            }).collect();
            json!(segments)
        }
        "item" => {
            serde_json::to_value(item_presentation(&request["item"])).map_err(|e| e.to_string())?
        }
        "reconcile" => {
            let ids = |key: &str| -> Result<Vec<&str>, String> {
                request[key]
                    .as_array()
                    .ok_or_else(|| format!("{key} must be an array"))?
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .ok_or_else(|| format!("{key} must contain strings"))
                    })
                    .collect()
            };
            json!(remaining_submissions(&ids("pending")?, ids("echoed")?))
        }
        _ => return Err("unknown conversation presentation operation".into()),
    };
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

#[cfg(test)]
mod presentation_tests {
    use super::*;

    #[test]
    fn generated_images_remain_visible_outside_completed_work() {
        for status in ["inProgress", "completed", "failed"] {
            let turn = json!({"id":"turn","status":"completed","items":[
                {"id":"work","type":"reasoning"},
                {"id":"image","type":"imageGeneration","status":status},
                {"id":"answer","type":"agentMessage","phase":"final_answer","text":"Here is the image"}
            ]});
            let segment = project(&turn).next().unwrap();
            assert_eq!(segment.role(1, &turn["items"][1]), Role::Response);
            assert!(!item_presentation(&turn["items"][1]).collapsible);
        }
    }

    fn rows<'a>(part: &'a Segment, turn: &'a Value, role: Role) -> impl Iterator<Item = &'a Value> {
        (part.start..part.end)
            .filter(move |&index| part.role(index, &turn["items"][index]) == role)
            .map(move |index| &turn["items"][index])
    }
    #[test]
    fn completed_turn_projects_user_work_and_final() {
        let turn = json!({"id":"turn","status":"completed","durationMs":40000,"items":[{"id":"u","type":"userMessage"},{"id":"r","type":"reasoning"},{"id":"c","type":"commandExecution"},{"id":"a","type":"agentMessage","phase":"commentary"},{"id":"f","type":"agentMessage","phase":"final_answer"}]});
        let p = project(&turn).next().unwrap();
        assert_eq!(rows(&p, &turn, Role::User).count(), 1);
        assert_eq!(
            rows(&p, &turn, Role::Activity)
                .map(|i| text(i, "id"))
                .collect::<Vec<_>>(),
            ["r", "c", "a"]
        );
        assert_eq!(
            text(rows(&p, &turn, Role::Response).next().unwrap(), "id"),
            "f"
        );
        assert!(p.collapsible);
        assert_eq!(p.label.as_deref(), Some("40秒 作業しました"));
    }
    #[test]
    fn completed_empty_turn_does_not_restore_thinking() {
        let turn = json!({"id":"turn","status":"completed","items":[{"id":"u","type":"userMessage","text":"追加メッセージ"}]});
        assert!(project(&turn).next().unwrap().label.is_none());
    }
    fn order(turn: &Value) -> Vec<String> {
        project(turn)
            .flat_map(|part| {
                rows(&part, turn, Role::User)
                    .chain(rows(&part, turn, Role::Activity))
                    .chain(rows(&part, turn, Role::Response))
                    .map(|item| text(item, "id").to_owned())
                    .collect::<Vec<_>>()
            })
            .collect()
    }
    #[test]
    fn live_commentary_separates_activity_groups_without_reordering() {
        let turn = json!({"id":"turn","status":"inProgress","items":[
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
            [
                "u", "intro", "c1", "c2", "progress", "tool", "followup", "r"
            ]
        );
        let labels: Vec<_> = project(&turn)
            .filter(|part| part.collapsible)
            .map(|part| part.label.unwrap())
            .collect();
        assert_eq!(labels, ["2件のコマンド", "1件のツール操作", "思考"]);
    }
    #[test]
    fn completed_exchanges_keep_each_answer_beside_its_question() {
        for phase in [Value::Null, json!("final_answer")] {
            let turn = json!({"id":"turn","status":"completed","durationMs":1459000,"items":[
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
                rows(&parts[0], &turn, Role::Response).next().unwrap()["id"],
                "f1"
            );
            assert_eq!(
                rows(&parts[1], &turn, Role::Response).next().unwrap()["id"],
                "f2"
            );
            assert_eq!(parts[0].label.as_deref(), Some("作業内容"));
            assert_eq!(parts[1].label.as_deref(), Some("24分 19秒 作業しました"));
        }
    }
    #[test]
    fn later_answer_does_not_hide_earlier_unanswered_commentary() {
        let turn = json!({"id":"turn","status":"completed","items":[
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
                    .map(|item| text(item, "id").to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(responses, ["a1", "a2", "answer"]);
    }
    #[test]
    fn failures_and_interruptions_keep_partial_responses_and_status() {
        for (status, expected) in [
            ("failed", "12秒 作業した後に失敗しました・思考"),
            ("interrupted", "12秒 作業した後に中断しました・思考"),
        ] {
            let turn = json!({"id":"turn","status":status,"durationMs":12000,"items":[
                {"id":"a","type":"agentMessage","phase":"commentary"},
                {"id":"r","type":"reasoning"}
            ]});
            assert_eq!(order(&turn), ["a", "r"]);
            let last = project(&turn).last().unwrap();
            assert!(last.last && last.collapsible);
            assert_eq!(last.label.as_deref(), Some(expected));
        }
    }
    #[test]
    fn hidden_lifecycle_items_do_not_split_visible_responses() {
        let turn = json!({"id":"turn","status":"inProgress","items":[
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
mod bridge_tests {
    use super::*;
    #[test]
    fn pending_echoes_deduplicate_by_identity_and_anchor_after_latest_occurrence() {
        assert_eq!(remaining_submissions(&["a", "b", "a", "c"], ["b"]), [0, 3]);
        let ids = ["first", "repeat", "repeat", "later"];
        assert_eq!(
            source_order(
                ids.len(),
                |i| ids[i],
                &[Some("repeat"), Some("missing"), Some("repeat"), None]
            ),
            [0, 1, 2, 4, 6, 3, 5, 7]
        );
    }
    #[test]
    fn duplicate_native_ids_keep_distinct_response_and_activity_indices() {
        let turn = json!({"id":"turn","status":"completed","items":[
            {"id":"same","type":"agentMessage","text":"old"},
            {"id":"same","type":"agentMessage","phase":"final_answer","text":"new"}
        ]});
        let segment = project(&turn).next().unwrap();
        assert_eq!(segment.role(0, &turn["items"][0]), Role::Activity);
        assert_eq!(segment.role(1, &turn["items"][1]), Role::Response);
        assert!(!segment.initially_expanded);
    }
    #[test]
    fn bridge_preserves_source_positions_without_returning_bodies() {
        let request = json!({"operation":"turn","turn":{"id":"t","status":"inProgress","items":[
            {"id":"a","type":"agentMessage","phase":"commentary","text":"private body"},
            {"id":"b","type":"commandExecution","aggregatedOutput":"private output"}
        ]},"pending":[{"afterItemId":"a","item":{"id":"p","type":"userMessage","clientId":"p","text":"private input"}}]});
        let output = present_json(&request.to_string()).unwrap();
        assert!(!output.contains("private"));
        let segments: Value = serde_json::from_str(&output).unwrap();
        let rows: Vec<_> = segments
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|s| s["rows"].as_array().unwrap())
            .map(|r| (r["source"].as_u64().unwrap(), r["role"].as_str().unwrap()))
            .collect();
        assert_eq!(rows, [(0, "response"), (2, "user"), (1, "activity")]);
    }
    #[test]
    fn titles_keep_one_bounded_unicode_line() {
        let command =
            json!({"type":"commandExecution","command":"  cargo test\nsecret second line"});
        assert_eq!(item_presentation(&command).title, "cargo test");
        let command = json!({"type":"commandExecution","command":"日".repeat(121)});
        let title = item_presentation(&command).title;
        assert_eq!(title.chars().count(), 121);
        assert!(title.ends_with('…'));
    }
}
