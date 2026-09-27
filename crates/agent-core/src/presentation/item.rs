//! Item titles and visibility for native conversation rows.

use super::grouping::{ItemMetadata, visible};
use crate::models::Item;
use agent_protocol::models::compact_title;
use serde::Serialize;
use serde_json::Value;

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
            "inProgress" => "",
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
mod item_tests {
    use super::*;
    use serde_json::json;
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
