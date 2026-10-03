//! Text and image sources are projected once in Rust; native views render them.
use crate::{
    models::{FileChangeKind, Item, ItemBody as WireBody, ItemStatus, MessagePart},
    state::Draft,
};
use std::{borrow::Cow, fmt::Write};

pub struct ItemBody {
    pub text: Option<String>,
    pub images: Vec<String>,
}
fn attachment(text: &mut String, name: &str, path: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    write!(text, "添付: {name} ({path})").expect("String write cannot fail");
}
fn message(text: Option<&str>, content: &[MessagePart]) -> String {
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
            attachment(&mut result, name, path);
        }
    }
    result
}
pub fn item_body(item: &Item) -> ItemBody {
    let text = match item.body() {
        WireBody::UserMessage { text, content } => Some(message(text.as_deref(), content)),
        WireBody::AssistantText { text, .. } => Some(text.clone()),
        WireBody::Reasoning { .. } => Some("詳細を表示".into()),
        WireBody::ImageGeneration { .. } => {
            (item.status == ItemStatus::Failed).then(|| "画像を生成できませんでした".into())
        }
        _ => Some(
            match item.status {
                ItemStatus::Unknown => "詳細を表示",
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
                    .map(|data| format!("data:image/png;base64,{data}"))
            })
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
pub fn draft_body(draft: &Draft) -> ItemBody {
    let mut text = draft.text.clone();
    let mut images = Vec::new();
    for file in &draft.attachments {
        if file.is_image {
            images.push(file.path.clone());
        } else {
            attachment(&mut text, &file.name, &file.path);
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
    diff: Option<&str>,
    proposal: Option<&crate::models::FileProposal>,
) -> String {
    let mut details = diff.unwrap_or("差分は取得できません").to_owned();
    if let Some(proposal) = proposal {
        write!(
            details,
            "\n提案された変更:\n{}",
            serde_json::to_string_pretty(proposal).expect("proposal serializes")
        )
        .expect("String write cannot fail");
    }
    details
}
/// Format large output only for expansion.
pub fn expanded_body(item: &Item) -> String {
    match item.body() {
        WireBody::UserMessage { text, content } => message(text.as_deref(), content),
        WireBody::AssistantText { text, .. } => text.clone(),
        WireBody::Reasoning { content, summary } => {
            if summary.is_empty() { content } else { summary }.join("\n")
        }
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
                    file_change_details(change.diff.as_deref(), change.proposal.as_ref())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
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
        json!({"id":"cmd","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"","cwd":"/fixture","output":"done","exitCode":null,"durationMs":null}}}}}),
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
        let body = item_body(&item);
        assert_eq!(body.text.as_deref(), Some(collapsed));
        assert_eq!(body.images, images);
        assert_eq!(expanded_body(&item), expanded);
    }

    #[test]
    fn generated_image_paths_are_displayed() {
        let generated: Item = serde_json::from_value(json!({"id":"image","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":"/saved.png","data":"base64","revisedPrompt":null}}}}})).unwrap();
        assert_eq!(item_body(&generated).images, ["/saved.png"]);
    }
}
