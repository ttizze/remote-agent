//! Conversation display policy shared by native desktop and mobile clients.
//! Grouping borrows item metadata; body formatting runs only for changed items
//! or explicit expansion. Native views own rendering and local expansion state.
//!
//! Work starts collapsed for live, failed, interrupted and reopened turns.
//! Streaming and deferred-detail hydration must not expand it. Keep commentary,
//! final responses, pending requests and errors visible outside collapsed work;
//! reported file changes remain inside it. Native clients consume core row order
//! and identities instead of independently guessing finality or grouping.

pub mod agents;
pub mod appearance;
pub mod background_work;
pub mod connection_list;
pub mod connections;
pub mod diff;
pub mod editors;
pub mod error;
pub mod files;
pub mod host_management;
pub mod media;
pub mod model_settings;
pub mod onboarding;
pub mod permissions;
pub mod project_menu;
pub mod settings;
pub mod sidebar;

use crate::localization::{UiLanguage, localized_ui_format};
use crate::models::{
    ApprovalReviewStatus, AssistantPhase, AttachmentKind, Item, ItemBody, ToolKind, Turn,
    TurnStatus,
};
use serde::Serialize;
pub mod body;

pub mod composer;
pub mod context;
pub mod conversation;
pub mod list;
pub mod markdown;
pub mod task_activity;
pub mod terminals;
pub mod thread_status;
pub mod worktrees;

/// Names and previews are opaque; only the absence of both has UI copy.
fn task_title<'a>(
    language: UiLanguage,
    name: Option<&'a str>,
    preview: Option<&'a str>,
) -> &'a str {
    crate::models::task_title(name, preview).unwrap_or_else(|| language.text("無題のタスク"))
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupKind {
    Hidden,
    User,
    Assistant,
    Command,
    FileChange,
    Image,
    /// Reasoning with text to read, which T3 Code's work log shows as its
    /// thinking. Reasoning without any stays hidden, as in T3 Code.
    Reasoning,
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
                // A notification starts a turn as a user message does.
                ItemBody::UserMessage { .. } | ItemBody::Notification { .. } => GroupKind::User,
                ItemBody::AssistantText { .. } => GroupKind::Assistant,
                ItemBody::CommandExecution { .. } => GroupKind::Command,
                ItemBody::FileChange { .. } => GroupKind::FileChange,
                ItemBody::ImageGeneration { .. } => GroupKind::Image,
                ItemBody::Reasoning { content, summary }
                    if body::reasoning_parts(content, summary).is_some() =>
                {
                    GroupKind::Reasoning
                }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Hidden,
    User,
    Activity,
    Response,
}

struct Segment {
    id: String,
    start: usize,
    end: usize,
    answer: Option<usize>,
    last: bool,
    collapsible: bool,
    label: Option<String>,
}
impl Segment {
    fn role(&self, index: usize, item: ItemMetadata<'_>) -> Role {
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

#[cfg(test)]
fn project(turn: &Turn) -> impl Iterator<Item = Segment> {
    let items = turn.items.as_deref().unwrap_or_default();
    project_items(UiLanguage::Japanese, turn, items.len(), move |index| {
        items[index].as_ref().into()
    })
}

#[cfg(test)]
fn assert_work_starts_collapsed(turn: &Turn) {
    use conversation::{ConversationRowContent, activity_is_expanded, project_conversation};
    use std::sync::Arc;
    let rendered = project_conversation(
        crate::localization::UiLanguage::Japanese,
        &crate::state::Snapshot::default()
            .panes
            .primary()
            .navigation
            .draft_key,
        &crate::state::Snapshot::default()
            .sessions
            .pending_submissions,
        Arc::new(crate::models::Thread {
            turns: Some(vec![Arc::new(turn.clone())]),
            ..Default::default()
        }),
        &None,
    );
    let mut activities = 0;
    for row in &rendered.turns[0].rows {
        if let ConversationRowContent::ActivityHeader { activity } = &row.content {
            activities += 1;
            assert!(!activity_is_expanded(&activity.status, None));
        }
    }
    assert!(activities > 0);
}

/// The accessor lets clients project interleaved pending input without cloning
/// native item bodies. It must return the same item for an index throughout a call.
fn project_items<'a>(
    language: UiLanguage,
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
            label: None,
        };
        segment.collapsible =
            (start..end).any(|index| segment.role(index, item(index)) == Role::Activity);
        // As T3 Code: a turn that has ended folds its work under the whole turn's
        // time, and the work after each steer under a row that continues it. A
        // running turn names what its earlier segments did.
        segment.label = if segment.collapsible {
            let activity = || {
                activity_summary(
                    language,
                    (start..end)
                        .filter(|&index| segment.role(index, item(index)) == Role::Activity)
                        .map(&item),
                )
            };
            Some(if turn.status == TurnStatus::Running {
                activity()
            } else if start == 0 {
                // A turn that failed or was stopped says what its last work did.
                if segment.last
                    && matches!(turn.status, TurnStatus::Failed | TurnStatus::Interrupted)
                {
                    localized_ui_format(
                        language,
                        "work.summary.detail".into(),
                        vec![work_summary(language, turn), activity()],
                    )
                } else {
                    work_summary(language, turn)
                }
            } else {
                language.text("続けて作業しました").into()
            })
        } else if segment.last && !completed {
            Some(work_summary(language, turn))
        } else {
            None
        };
        start = end;
        Some(segment)
    })
}
fn activity_summary<'a>(
    language: UiLanguage,
    items: impl Iterator<Item = ItemMetadata<'a>>,
) -> String {
    let (mut commands, mut files, mut tools, mut thought) = (false, false, false, false);
    for item in items {
        match item.kind {
            GroupKind::Command => commands = true,
            GroupKind::FileChange => files = true,
            // As T3 Code's work groups: reasoning names only a group of its own.
            GroupKind::Reasoning => thought = true,
            _ => tools = true,
        }
    }
    let labels: Vec<_> = [
        (commands, "コマンドを実行しました"),
        (files, "ファイルを変更しました"),
        (tools, "ツールを使用しました"),
    ]
    .into_iter()
    .filter(|(present, _)| *present)
    .map(|(_, label)| language.text(label))
    .collect();
    if labels.is_empty() {
        return language
            .text(if thought { "思考" } else { "作業の詳細" })
            .into();
    }
    labels.join(if language == UiLanguage::Japanese {
        "、"
    } else {
        ", "
    })
}

fn work_summary(language: UiLanguage, turn: &Turn) -> String {
    if turn.status == TurnStatus::Running {
        return language.text("作業中…").into();
    }
    let duration = turn
        .duration_ms
        .or_else(|| turn.completed_at_ms?.checked_sub(turn.started_at_ms?));
    let Some(ms) = duration else {
        return language
            .text(match turn.status {
                TurnStatus::Interrupted => "作業を中断しました",
                TurnStatus::Failed => "作業に失敗しました",
                _ => "作業しました",
            })
            .into();
    };
    localized_ui_format(
        language,
        match turn.status {
            TurnStatus::Interrupted => "work.interrupted.duration",
            TurnStatus::Failed => "work.failed.duration",
            _ => "work.completed.duration",
        }
        .into(),
        vec![duration_text(language, ms)],
    )
}

/// A duration as T3 Code's `formatDuration` names it: `350ms`, `2.5s`, `12s`,
/// `1h 2m 3s`.
fn duration_text(language: UiLanguage, ms: u64) -> String {
    let seconds =
        |value: String| localized_ui_format(language, "duration.seconds".into(), vec![value]);
    if ms < 1000 {
        return localized_ui_format(
            language,
            "duration.milliseconds".into(),
            vec![ms.max(1).to_string()],
        );
    }
    if ms < 10_000 {
        let tenths = (ms as f64 / 100.).round() / 10.;
        return seconds(if tenths >= 10. {
            "10".into()
        } else {
            format!("{tenths:.1}")
        });
    }
    if ms < 60_000 {
        return seconds(((ms as f64 / 1000.).round() as u64).to_string());
    }
    clock_text(language, (ms as f64 / 1000.).round() as u64)
}

/// How long a running turn has worked, as T3 Code's working timer counts it:
/// whole seconds for its first minute, then as a duration.
fn elapsed_text(language: UiLanguage, ms: u64) -> String {
    if ms < 60_000 {
        return localized_ui_format(
            language,
            "duration.seconds".into(),
            vec![(ms / 1000).to_string()],
        );
    }
    duration_text(language, ms)
}

/// Whole seconds as hours, minutes and seconds, each only when not zero.
fn clock_text(language: UiLanguage, seconds: u64) -> String {
    [
        (seconds / 3600, "duration.hours"),
        (seconds / 60 % 60, "duration.minutes"),
        (seconds % 60, "duration.seconds"),
    ]
    .into_iter()
    .filter(|(value, _)| *value != 0)
    .map(|(value, key)| localized_ui_format(language, key.into(), vec![value.to_string()]))
    .collect::<Vec<_>>()
    .join(" ")
}

/// Place pending inputs after their saved native anchor. No anchor means before
/// the first item; an unloaded anchor goes at the tail. Repeated native IDs use
/// the latest occurrence, never duplicating
/// a pending input. Indices >= item_count refer to the pending input slice.
fn source_order<'a>(
    item_count: usize,
    item_id: impl Fn(usize) -> &'a str,
    anchors: impl IntoIterator<Item = Option<&'a str>>,
) -> Vec<usize> {
    let anchors: Vec<_> = anchors.into_iter().collect();
    if anchors.is_empty() {
        return (0..item_count).collect();
    }
    let latest: std::collections::HashMap<_, _> = (0..item_count)
        .map(|index| (item_id(index), index + 1))
        .collect();
    let mut positions: Vec<_> = anchors
        .into_iter()
        .enumerate()
        .map(|(pending, anchor)| {
            (
                anchor.map_or(0, |anchor| {
                    latest.get(anchor).copied().unwrap_or(item_count)
                }),
                item_count + pending,
            )
        })
        .collect();
    // The pending index breaks ties in submission order.
    positions.sort_unstable();
    let mut order = Vec::with_capacity(item_count + positions.len());
    let mut start = 0;
    for (boundary, pending) in positions {
        order.extend(start..boundary);
        order.push(pending);
        start = boundary;
    }
    order.extend(start..item_count);
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
    language: UiLanguage,
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
                    Some(crate::session::ProviderKind::DeepSeek) => "DeepSeek",
                    None => "Assistant",
                }
                .into(),
            ),
            false,
        ),
        // T3 Code's "Thought"; the live row reads "Thinking" (`work_row_label`).
        ItemBody::Reasoning { .. } => ("reasoning", Some(language.text("思考").into()), true),
        ItemBody::CommandExecution { command, .. } => {
            ("command", Some(compact_title(command)), true)
        }
        ItemBody::FileChange { changes, .. } => (
            "fileChange",
            Some(localized_ui_format(
                language,
                "{0}件のファイル変更".into(),
                vec![changes.len().to_string()],
            )),
            true,
        ),
        ItemBody::ImageGeneration { .. } => ("imageGeneration", None, false),
        ItemBody::Notification { reports } => (
            "notification",
            Some(background_work::notification_text(
                reports.clone(),
                language,
            )),
            false,
        ),
        // As T3 Code's "Compacting…" while it runs.
        ItemBody::Compaction {} if item.status == crate::models::ItemStatus::Running => (
            "unknown",
            Some(language.text("コンテキストを圧縮中…").into()),
            true,
        ),
        ItemBody::WebSearch { .. } => ("webSearch", Some(tool_title(language, item.body())), true),
        _ => ("unknown", Some(tool_title(language, item.body())), true),
    };
    ItemPresentation {
        kind,
        title,
        collapsible,
    }
}
fn tool_title(language: UiLanguage, body: &ItemBody) -> String {
    match body {
        ItemBody::Attachment { kind, .. } => language
            .text(match kind {
                AttachmentKind::HookResult => "フックの実行結果",
                AttachmentKind::FileEdit => "ファイルの編集内容",
                AttachmentKind::SessionUpdate => "セッションの更新情報",
                AttachmentKind::Other => "会話の添付情報",
            })
            .into(),
        ItemBody::Hook { .. } => language.text("追加指示").into(),
        ItemBody::Plan { .. } => language.text("計画を更新しました").into(),
        ItemBody::ToolCall {
            kind: ToolKind::Mcp,
            tool,
            server,
            ..
        } => match (server.as_deref().unwrap_or_default(), tool.as_str()) {
            ("", "") => language.text("MCPツールを実行しました").into(),
            (server, "") => localized_ui_format(
                language,
                "{0}の連携を使用しました".into(),
                vec![compact_title(server)],
            ),
            ("", tool) => compact_title(tool),
            (server, tool) => localized_ui_format(
                language,
                "{0}の連携を使用しました · {1}".into(),
                vec![compact_title(server), compact_title(tool)],
            ),
        },
        ItemBody::ToolCall { tool, .. } => {
            if tool.is_empty() {
                language.text("ツールを実行しました").into()
            } else {
                localized_ui_format(
                    language,
                    "{0}を実行しました".into(),
                    vec![compact_title(tool)],
                )
            }
        }
        ItemBody::Subagent { tool, .. } => {
            if tool.is_empty() {
                language.text("サブエージェントが作業しました").into()
            } else {
                localized_ui_format(
                    language,
                    "サブエージェント: {0}".into(),
                    vec![compact_title(tool)],
                )
            }
        }
        ItemBody::WebSearch { query, .. } => {
            if query.is_empty() {
                language.text("Webを検索しました").into()
            } else {
                localized_ui_format(
                    language,
                    "Webを検索: {0}".into(),
                    vec![compact_title(query)],
                )
            }
        }
        ItemBody::ImageView { path } => {
            if path.is_empty() {
                language.text("画像を確認しました").into()
            } else {
                localized_ui_format(
                    language,
                    "画像を確認: {0}".into(),
                    vec![compact_title(path)],
                )
            }
        }
        ItemBody::Compaction {} => language.text("コンテキストを圧縮しました").into(),
        ItemBody::AutomaticApproval { status, .. } => language
            .text(match status {
                ApprovalReviewStatus::Running => "承認を自動確認中",
                ApprovalReviewStatus::Denied => "自動確認で拒否されました",
                ApprovalReviewStatus::TimedOut => "自動確認がタイムアウトしました",
                ApprovalReviewStatus::Aborted => "自動確認を中止しました",
                _ => "承認を自動確認しました",
            })
            .into(),
        ItemBody::Sleep {} => language.text("待機しました").into(),
        ItemBody::Review { entering, .. } => language
            .text(if *entering {
                "レビューを開始しました"
            } else {
                "レビューを終了しました"
            })
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
            UiLanguage::Japanese,
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
        assert_work_starts_collapsed(&turn);
        // As T3 Code, a finished turn folds its work under its working time.
        assert_eq!(p.label.as_deref(), Some("40秒 作業しました"));
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
    // Product choice: live work stays closed too. Changing the expected value
    // to match an implementation change previously concealed a UI regression.
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
        assert_work_starts_collapsed(&turn);
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
        assert_work_starts_collapsed(&turn);
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
        // The work after a steer continues the turn's first fold, which keeps the
        // whole turn's time.
        assert_eq!(parts[0].label.as_deref(), Some("24分 19秒 作業しました"));
        assert_eq!(parts[1].label.as_deref(), Some("続けて作業しました"));
        let rendered = conversation::project_conversation(
            UiLanguage::Japanese,
            &crate::state::Snapshot::default()
                .panes
                .primary()
                .navigation
                .draft_key,
            &crate::state::Snapshot::default()
                .sessions
                .pending_submissions,
            std::sync::Arc::new(crate::models::Thread {
                turns: Some(vec![std::sync::Arc::new(turn.clone())]),
                ..Default::default()
            }),
            &None,
        );
        let steers: Vec<_> = rendered.turns[0]
            .rows
            .iter()
            .filter(|row| {
                matches!(
                    row.content,
                    conversation::ConversationRowContent::User { .. }
                )
            })
            .map(|row| row.steer)
            .collect();
        assert_eq!(
            steers,
            [false, true],
            "only the message sent while the turn worked steers it"
        );
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
    use proptest::prelude::*;
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
    proptest! {
        #[test]
        fn input_order_preserves_every_occurrence_and_submission_boundary(
            native in prop::collection::vec(0u8..8, 0..32),
            anchors in prop::collection::vec(prop::option::of(0u8..12), 0..196),
        ) {
            let ids: Vec<_> = native.iter().map(u8::to_string).collect();
            let names: Vec<_> = anchors.iter().map(|id| id.map(|id| id.to_string())).collect();
            let order = source_order(ids.len(), |index| &ids[index], names.iter().map(|id| id.as_deref()));
            let mut all = order.clone();
            all.sort_unstable();
            prop_assert_eq!(all, (0..native.len()+anchors.len()).collect::<Vec<_>>());
            prop_assert!(order.iter().copied().filter(|&index| index < native.len()).eq(0..native.len()));
            let boundaries: Vec<_> = anchors.iter().map(|anchor| anchor.map_or(0, |id| {
                native.iter().rposition(|&old| old == id).map_or(native.len(), |index| index+1)
            })).collect();
            for (pending, &boundary) in boundaries.iter().enumerate() {
                let position = order.iter().position(|&index| index == native.len()+pending).unwrap();
                prop_assert_eq!(order[..position].iter().filter(|&&index| index < native.len()).count(), boundary);
            }
            let pending: Vec<_> = order.iter().filter_map(|&index| index.checked_sub(native.len())).collect();
            prop_assert!(pending.windows(2).all(|pair| boundaries[pair[0]] != boundaries[pair[1]] || pair[0] < pair[1]));
        }
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
        assert_work_starts_collapsed(&turn);
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
        let segments: Vec<_> =
            project_items(UiLanguage::Japanese, &turn, order.len(), item).collect();
        let rows: Vec<_> = segments
            .iter()
            .flat_map(|s| (s.start..s.end).map(|index| (order[index], s.role(index, item(index)))))
            .collect();
        assert_eq!(
            rows,
            [(0, Role::Response), (2, Role::User), (1, Role::Activity)]
        );
    }
    #[rstest::rstest]
    #[case(
        TurnStatus::Completed,
        Some(0),
        "1ミリ秒 作業しました",
        "Worked for 1ms"
    )]
    #[case(
        TurnStatus::Failed,
        Some(61000),
        "1分 1秒 作業した後に失敗しました",
        "Worked for 1m 1s · failed"
    )]
    #[case(
        TurnStatus::Interrupted,
        Some(3600000),
        "1時間 作業した後に中断しました",
        "You stopped after 1h"
    )]
    #[case(
        TurnStatus::Completed,
        Some(3723000),
        "1時間 2分 3秒 作業しました",
        "Worked for 1h 2m 3s"
    )]
    #[case(TurnStatus::Completed, None, "作業しました", "Worked")]
    #[case(TurnStatus::Failed, None, "作業に失敗しました", "Work failed")]
    #[case(
        TurnStatus::Interrupted,
        None,
        "作業を中断しました",
        "You stopped this response"
    )]
    #[case(TurnStatus::Running, Some(1000), "作業中…", "Working…")]
    fn work_summaries_format_the_duration_and_outcome(
        #[case] status: TurnStatus,
        #[case] duration_ms: Option<u64>,
        #[case] japanese: &str,
        #[case] english: &str,
    ) {
        let turn = Turn {
            status,
            duration_ms,
            ..Default::default()
        };
        assert_eq!(work_summary(UiLanguage::Japanese, &turn), japanese);
        assert_eq!(work_summary(UiLanguage::English, &turn), english);
    }

    #[test]
    fn failed_work_combines_outcome_and_activity_copy_in_each_language() {
        let turn = Turn {
            status: TurnStatus::Failed,
            duration_ms: Some(0),
            ..Default::default()
        };
        let kinds = [
            GroupKind::Command,
            GroupKind::FileChange,
            GroupKind::Activity,
        ];
        for (language, expected) in [
            (
                UiLanguage::Japanese,
                "1ミリ秒 作業した後に失敗しました・コマンドを実行しました、ファイルを変更しました、ツールを使用しました",
            ),
            (
                UiLanguage::English,
                "Worked for 1ms · failed · Ran a command, Changed files, Used a tool",
            ),
        ] {
            let segment = project_items(language, &turn, kinds.len(), |index| ItemMetadata {
                kind: kinds[index],
                ..Default::default()
            })
            .next()
            .unwrap();
            assert_eq!(segment.label.as_deref(), Some(expected));
        }
    }

    #[test]
    fn titles_keep_one_bounded_unicode_line() {
        let command: Item = serde_json::from_value(json!({"id":"command","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"  cargo test\nsecret second line","cwd":null,"output":"","exitCode":null}}}}})).unwrap();
        assert_eq!(
            item_presentation(
                UiLanguage::Japanese,
                &command,
                Some(crate::session::ProviderKind::Codex)
            )
            .title
            .as_deref(),
            Some("cargo test")
        );
        let command: Item = serde_json::from_value(
            json!({"id":"command","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"日".repeat(121),"cwd":null,"output":"","exitCode":null}}}}}),
        )
        .unwrap();
        let title = item_presentation(
            UiLanguage::Japanese,
            &command,
            Some(crate::session::ProviderKind::Codex),
        )
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
                UiLanguage::Japanese,
                &item,
                Some(crate::session::ProviderKind::Codex),
            )
            .title
            .unwrap();
            item.defer();
            assert_eq!(
                crate::presentation::item_presentation(
                    UiLanguage::Japanese,
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

#[cfg(test)]
#[path = "view/publication_tests.rs"]
mod publication_tests;
pub mod view;
