//! Text and image sources are projected once in Rust; native views render them.
use crate::localization::{UiLanguage, localized_ui_format};
use crate::{
    models::{FileChangeKind, Item, ItemBody as WireBody, ItemStatus, MessagePart},
    state::Draft,
};
use std::{borrow::Cow, fmt::Write};
mod tool_output;

pub struct ItemBody {
    pub text: Option<String>,
    pub images: Vec<String>,
}
fn attachment(language: UiLanguage, text: &mut String, name: &str, path: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&localized_ui_format(
        language,
        "添付: {0} ({1})".into(),
        vec![name.into(), path.into()],
    ));
}
fn message(language: UiLanguage, text: Option<&str>, content: &[MessagePart]) -> String {
    let mut result = text.map(str::to_owned).unwrap_or_else(|| {
        content
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>()
    });
    for part in content {
        if let MessagePart::Attachment { name, path } = part {
            attachment(language, &mut result, name, path);
        }
    }
    result
}
pub fn item_body(language: UiLanguage, item: &Item) -> ItemBody {
    let text = match item.body() {
        WireBody::UserMessage { text, content } => {
            Some(message(language, text.as_deref(), content))
        }
        WireBody::AssistantText { text, .. } => Some(text.clone()),
        WireBody::Reasoning { .. } => Some(language.text("詳細を表示").into()),
        WireBody::ImageGeneration { .. } => (item.status == ItemStatus::Failed)
            .then(|| language.text("画像を生成できませんでした").into()),
        WireBody::ImageView { .. } => None,
        _ => Some(
            match item.status {
                ItemStatus::Unknown => language.text("詳細を表示"),
                ItemStatus::Running => "running",
                ItemStatus::Completed => "completed",
                ItemStatus::Failed => "failed",
                ItemStatus::Declined => "declined",
                ItemStatus::Interrupted => "interrupted",
            }
            .into(),
        ),
    };
    let images = match item.body() {
        WireBody::ImageGeneration {
            saved_path, data, ..
        } => saved_path
            .clone()
            .filter(|path| !path.is_empty())
            .or_else(|| {
                data.as_ref()
                    .filter(|data| !data.is_empty())
                    .map(|data| crate::presentation::media::encoded_image_source(data))
            })
            .into_iter()
            .collect(),
        WireBody::ImageView { path } => (!path.is_empty())
            .then(|| path.clone())
            .into_iter()
            .collect(),
        WireBody::UserMessage { content, .. } => content
            .iter()
            .filter_map(|part| match part {
                MessagePart::Image { source } => Some(source.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    };
    ItemBody { text, images }
}
pub fn draft_body(language: UiLanguage, draft: &Draft) -> ItemBody {
    let mut text = draft.text.clone();
    let mut images = Vec::new();
    for file in &draft.attachments {
        if file.is_image {
            images.push(file.path.clone());
        } else {
            attachment(language, &mut text, &file.name, &file.path);
        }
    }
    ItemBody {
        text: Some(text),
        images,
    }
}
pub struct FileChange<'a> {
    pub path: Cow<'a, str>,
    pub kind: Cow<'a, str>,
    pub diff: Option<Cow<'a, str>>,
    pub proposal: Option<&'a crate::models::FileProposal>,
}
pub fn file_changes(item: &Item) -> impl Iterator<Item = FileChange<'_>> {
    let changes = match item.body() {
        WireBody::FileChange { changes, .. } => changes.as_slice(),
        _ => &[],
    };
    changes.iter().map(|change| FileChange {
        path: Cow::Borrowed(&change.path),
        kind: Cow::Borrowed(match change.kind {
            FileChangeKind::Add => "add",
            FileChangeKind::Delete => "delete",
            FileChangeKind::Update { .. } => "update",
            FileChangeKind::Unknown => "unknown",
        }),
        diff: change.diff.as_deref().map(Cow::Borrowed),
        proposal: change.proposal.as_ref(),
    })
}
pub fn file_change_details(
    language: UiLanguage,
    diff: Option<&str>,
    proposal: Option<&crate::models::FileProposal>,
) -> String {
    let mut details = diff
        .unwrap_or_else(|| language.text("差分は取得できません"))
        .to_owned();
    if let Some(proposal) = proposal {
        write!(
            details,
            "\n{}\n{}",
            language.text("提案された変更:"),
            serde_json::to_string_pretty(proposal).expect("proposal serializes")
        )
        .expect("String write cannot fail");
    }
    details
}
/// Format large output only for expansion.
pub fn expanded_body(language: UiLanguage, item: &Item) -> String {
    match item.body() {
        WireBody::UserMessage { text, content } => message(language, text.as_deref(), content),
        WireBody::AssistantText { text, .. } => text.clone(),
        WireBody::Plan { text } => text.clone(),
        WireBody::Reasoning { content, summary } => reasoning_parts(content, summary)
            .unwrap_or_default()
            .join("\n"),
        WireBody::CommandExecution { cwd, output, .. } => match cwd {
            Some(cwd) => {
                if output.is_empty() {
                    format!("cwd: {cwd}")
                } else {
                    format!("cwd: {cwd}\n{output}")
                }
            }
            None => output.clone(),
        },
        WireBody::Attachment { content, .. } | WireBody::Custom { value: content, .. } => {
            serde_json::to_string_pretty(content).expect("value serializes")
        }
        WireBody::FileChange { changes, output } => {
            let mut result = String::new();
            for (index, (change, display)) in changes.iter().zip(file_changes(item)).enumerate() {
                if index != 0 {
                    result.push_str("\n\n");
                }
                write!(
                    result,
                    "{}: {}\n{}",
                    display.kind,
                    display.path,
                    file_change_details(language, change.diff.as_deref(), change.proposal.as_ref())
                )
                .expect("String write cannot fail");
            }
            if !output.is_empty() {
                if !result.is_empty() {
                    result.push_str("\n\n");
                }
                result.push_str(output);
            }
            result
        }
        _ => serde_json::to_string_pretty(item.body()).expect("body serializes"),
    }
}

/// The parts of a reasoning trace to read: its summary when the provider gave
/// one, else its own text; none when neither has any.
pub(super) fn reasoning_parts<'a>(
    content: &'a [String],
    summary: &'a [String],
) -> Option<&'a [String]> {
    let readable = |parts: &'a [String]| {
        Some(parts).filter(|parts| parts.iter().any(|part| !part.trim().is_empty()))
    };
    readable(summary).or_else(|| readable(content))
}

/// A reasoning trace as Markdown, its parts as paragraphs (T3 Code reads them
/// expanded, with the Thinking heading).
fn reasoning_trace(item: &Item) -> Option<String> {
    match item.body() {
        WireBody::Reasoning { content, summary } => {
            reasoning_parts(content, summary).map(|parts| parts.join("\n\n"))
        }
        _ => None,
    }
}

/// A reasoning trace on one line, as T3 Code's collapsed Thinking row: its
/// Markdown read as plain text with each run of whitespace one space.
pub(super) fn reasoning_preview(item: &Item) -> Option<String> {
    use super::markdown::MarkdownBlock;
    let mut text = String::new();
    let mut append = |runs: &[super::markdown::MarkdownRun]| {
        text.push(' ');
        text.extend(runs.iter().map(|run| run.text.as_str()));
    };
    for block in super::markdown::markdown_blocks(reasoning_trace(item)?) {
        match &block {
            MarkdownBlock::Paragraph { runs, .. } => append(runs),
            MarkdownBlock::Table { rows, .. } => {
                rows.iter().flatten().for_each(|cell| append(&cell.runs))
            }
            MarkdownBlock::Visualization { .. } => {}
        }
    }
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}

/// Native detail views consume typed Markdown blocks; output never becomes executable markup.
pub(super) fn detail_blocks(
    language: UiLanguage,
    item: &Item,
) -> Vec<super::markdown::MarkdownBlock> {
    use super::markdown::{markdown_blocks, markdown_code_block};
    let code = |text: &str, syntax: Option<&str>, label: &str| {
        markdown_code_block(
            text.into(),
            syntax.map(str::to_owned),
            Some(language.text(label).into()),
        )
    };
    match item.body() {
        WireBody::UserMessage { text, content } => {
            markdown_blocks(message(language, text.as_deref(), content))
        }
        WireBody::AssistantText { text, .. } => markdown_blocks(text.clone()),
        WireBody::Reasoning { .. } => reasoning_trace(item)
            .map(markdown_blocks)
            .unwrap_or_default(),
        WireBody::CommandExecution {
            command,
            cwd,
            output,
            exit_code,
        } => {
            let mut blocks = vec![code(command, Some("sh"), "コマンド")];
            if let Some(cwd) = cwd {
                blocks.extend(markdown_blocks(localized_ui_format(
                    language,
                    "作業フォルダ: `{0}/`".into(),
                    vec![cwd.clone()],
                )));
            }
            if !output.is_empty() {
                blocks.push(code(output, None, "出力"));
            }
            if let Some(exit_code) = exit_code {
                blocks.extend(markdown_blocks(localized_ui_format(
                    language,
                    "終了コード: **{0}**".into(),
                    vec![exit_code.to_string()],
                )));
            }
            blocks
        }
        WireBody::FileChange { changes, output } => {
            let mut blocks = Vec::new();
            for change in changes {
                blocks.extend(markdown_blocks(format!(
                    "[{}](<{}>)",
                    change.path,
                    change.path.replace('>', "%3E")
                )));
                if let Some(diff) = &change.diff {
                    blocks.push(code(diff, Some("diff"), "差分"));
                }
                if let Some(proposal) = &change.proposal {
                    blocks.push(code(
                        &serde_json::to_string_pretty(proposal).expect("proposal serializes"),
                        Some("json"),
                        "提案",
                    ));
                }
            }
            if !output.is_empty() {
                blocks.push(code(output, None, "出力"));
            }
            blocks
        }
        WireBody::Plan { text } => markdown_blocks(text.clone()),
        WireBody::ToolCall {
            arguments,
            result,
            error,
            content,
            ..
        } => {
            let mut blocks = Vec::new();
            if !arguments.is_null() {
                blocks.push(code(
                    &serde_json::to_string_pretty(arguments).expect("arguments serialize"),
                    Some("json"),
                    "入力",
                ));
            }
            if let Some(result) = result {
                blocks.extend(tool_output::result_blocks(language, result));
            }
            if let Some(error) = error {
                blocks.push(code(
                    &serde_json::to_string_pretty(error).expect("error serializes"),
                    Some("json"),
                    "エラー",
                ));
            }
            for content in content {
                match content {
                    agent_protocol::requests::ToolContent::Text { text } => {
                        blocks.extend(markdown_blocks(text.clone()))
                    }
                    agent_protocol::requests::ToolContent::Image { data_url } => {
                        blocks.push(tool_output::image_block(language, data_url.clone()));
                    }
                }
            }
            blocks
        }
        WireBody::WebSearch { query, action } => {
            let mut blocks = markdown_blocks(localized_ui_format(
                language,
                "検索: **{0}**".into(),
                vec![query.replace('*', "\\*")],
            ));
            let url = match action {
                Some(
                    crate::models::WebSearchAction::OpenPage { url }
                    | crate::models::WebSearchAction::Find { url, .. },
                ) => url.as_ref(),
                _ => None,
            };
            if let Some(url) = url {
                blocks.extend(markdown_blocks(format!("<{url}>")));
            }
            if let Some(action) = action {
                blocks.push(code(
                    &serde_json::to_string_pretty(action).expect("action serializes"),
                    Some("json"),
                    "検索操作",
                ));
            }
            blocks
        }
        _ => vec![code(&expanded_body(language, item), Some("json"), "詳細")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    #[test]
    fn plain_message_details_stay_verbatim_in_both_languages() {
        for body in [
            WireBody::UserMessage {
                text: Some("設定{0}".into()),
                content: vec![],
            },
            WireBody::AssistantText {
                text: "設定{0}".into(),
                phase: Default::default(),
            },
        ] {
            let item = Item::new("message".into(), ItemStatus::Completed, body);
            for language in [UiLanguage::Japanese, UiLanguage::English] {
                let blocks = detail_blocks(language, &item);
                let super::super::markdown::MarkdownBlock::Paragraph { runs, style } = &blocks[0]
                else {
                    panic!()
                };
                assert_eq!(style.filename, None);
                assert_eq!(
                    runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                    "設定{0}"
                );
            }
        }
    }

    #[test]
    fn translated_detail_labels_keep_commands_and_output_verbatim() {
        let item = Item::new(
            "command".into(),
            ItemStatus::Completed,
            WireBody::CommandExecution {
                command: "設定\n{0}".into(),
                cwd: Some("/設定/{0}".into()),
                output: "詳細を表示\n{0}".into(),
                exit_code: Some(2),
            },
        );
        let blocks = detail_blocks(UiLanguage::English, &item);
        let super::super::markdown::MarkdownBlock::Paragraph { runs, style } = &blocks[0] else {
            panic!()
        };
        assert_eq!(style.filename.as_deref(), Some("Command"));
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "設定\n{0}"
        );
        let output = blocks
            .iter()
            .find_map(|block| match block {
                super::super::markdown::MarkdownBlock::Paragraph { runs, style }
                    if style.filename.as_deref() == Some("Output") =>
                {
                    Some(runs)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            output
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "詳細を表示\n{0}"
        );
        assert_eq!(
            expanded_body(UiLanguage::English, &item),
            "cwd: /設定/{0}\n詳細を表示\n{0}"
        );
    }

    #[test]
    fn command_details_keep_output_literal_and_syntax_information() {
        let item: Item = serde_json::from_value(json!({"id":"cmd","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"echo hi","cwd":"/workspace","output":"**literal**\n[link](https://example.com)","exitCode":2}}}}})).unwrap();
        let blocks = detail_blocks(UiLanguage::Japanese, &item);
        let super::super::markdown::MarkdownBlock::Paragraph { style, .. } = &blocks[0] else {
            panic!()
        };
        assert_eq!(style.language.as_deref(), Some("sh"));
        let output = blocks
            .iter()
            .find_map(|block| match block {
                super::super::markdown::MarkdownBlock::Paragraph { runs, style }
                    if style.filename.as_deref() == Some("出力") =>
                {
                    Some(runs)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            output
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>(),
            "**literal**\n[link](https://example.com)"
        );
        assert!(output.iter().all(|run| run.link.is_none() && !run.strong));
    }
    #[rstest::rstest]
    #[case::named_fields(
        json!([{"path":"/a.txt","kind":{"update":{"movePath":null}},"diff":"-old\n+new"}, {"path":"/b.txt","kind":"add","diff":"+second"}]),
        &[("/a.txt", "update", "-old\n+new"), ("/b.txt", "add", "+second")]
    )]
    #[case::missing_diff(json!([{"path":"/a.txt","kind":{"update":{"movePath":null}}}]), &[("/a.txt", "update", "")])]
    #[case::empty(json!([]), &[])]
    fn file_display_keeps_paths_and_diffs_from_named_wire_fields(
        #[case] changes: Value,
        #[case] expected: &[(&str, &str, &str)],
    ) {
        let item: Item =
            serde_json::from_value(json!({"id":"files","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":changes,"output":""}}}}}))
                .unwrap();
        let changes: Vec<_> = file_changes(&item).collect();
        let displayed: Vec<_> = changes
            .iter()
            .map(|change| {
                (
                    change.path.as_ref(),
                    change.kind.as_ref(),
                    change.diff.as_deref().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(displayed, expected);
    }

    #[rstest::rstest]
    #[case::user_with_attachments(
        json!({"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":"hello"}},{"attachment":{"name":"a.txt","path":"/a"}},{"image":{"source":"/photo.png"}}]}}}}}),
        "hello\n添付: a.txt (/a)", "hello\n添付: a.txt (/a)", &["/photo.png"]
    )]
    #[case::user_with_invocations(
        json!({"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":"$review @Tools"}},{"invocation":{"name":"review","path":"/skills/review/SKILL.md"}},{"invocation":{"name":"Tools","path":"plugin://tools@local"}}]}}}}}),
        "$review @Tools", "$review @Tools", &[]
    )]
    #[case::reasoning_summary(
        json!({"id":"reason","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":[],"summary":["one","two"]}}}}}),
        "詳細を表示", "one\ntwo", &[]
    )]
    #[case::reasoning_text(
        json!({"id":"reason","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"reasoning":{"content":["reasoning text"],"summary":[]}}}}}),
        "詳細を表示", "reasoning text", &[]
    )]
    #[case::command(
        json!({"id":"cmd","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":"/fixture","output":"done","exitCode":null}}}}}),
        "詳細を表示", "cwd: /fixture\ndone", &[]
    )]
    #[case::file_change(
        json!({"id":"file","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"/a","kind":{"update":{"movePath":null}},"diff":"+line","proposal":null}],"output":""}}}}}),
        "詳細を表示", "update: /a\n+line", &[]
    )]
    fn body_contract_covers_messages_images_and_details(
        #[case] wire: Value,
        #[case] collapsed: &str,
        #[case] expanded: &str,
        #[case] images: &[&str],
    ) {
        let item: Item = serde_json::from_value(wire).unwrap();
        let body = item_body(UiLanguage::Japanese, &item);
        assert_eq!(body.text.as_deref(), Some(collapsed));
        assert_eq!(body.images, images);
        assert_eq!(expanded_body(UiLanguage::Japanese, &item), expanded);
    }

    #[test]
    fn generated_image_paths_are_displayed() {
        let generated: Item = serde_json::from_value(json!({"id":"image","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":"/saved.png","data":"base64","revisedPrompt":null}}}}})).unwrap();
        assert_eq!(
            item_body(UiLanguage::Japanese, &generated).images,
            ["/saved.png"]
        );
    }
}
