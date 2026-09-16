//! Text and image sources are projected once in Rust; native views render them.
use crate::{models::Item, state::Draft};
use serde_json::Value;
use std::{borrow::Cow, fmt::Write};

pub struct ItemBody {
    pub text: String,
    pub images: Vec<String>,
}

fn text(value: &Value) -> Option<Cow<'_, str>> {
    match value {
        Value::String(value) => Some(Cow::Borrowed(value)),
        Value::Number(value) => Some(Cow::Owned(value.to_string())),
        _ => None,
    }
}
fn field<'a>(value: &'a Value, key: &str) -> Cow<'a, str> {
    text(&value[key]).unwrap_or_default()
}
fn parts(value: &Value, separator: &str) -> String {
    let mut result = String::new();
    if let Some(parts) = value.as_array() {
        let mut first = true;
        for part in parts {
            if let Some(value) = text(&part["text"]).or_else(|| text(part)) {
                if !first {
                    result.push_str(separator);
                }
                first = false;
                result.push_str(&value);
            }
        }
    }
    result
}
fn attachment(text: &mut String, name: &str, path: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    write!(text, "添付: {name} ({path})").expect("writing a String cannot fail");
}
fn message(item: &Item) -> String {
    let content = item.extra.get("content").unwrap_or(&Value::Null);
    let mut result = item.text.clone().unwrap_or_else(|| parts(content, ""));
    if let Some(parts) = content.as_array() {
        for part in parts.iter().filter(|part| part["type"] == "mention") {
            attachment(&mut result, &field(part, "name"), &field(part, "path"));
        }
    }
    result
}

pub fn item_body(item: &Item, presentation: &super::ItemPresentation) -> ItemBody {
    let text = match presentation.kind {
        "user" | "agent" | "commentary" => message(item),
        "reasoning" => "詳細を表示".into(),
        "imageGeneration" => presentation.title.clone(),
        _ => item.status.clone().unwrap_or_else(|| "詳細を表示".into()),
    };
    let images = if presentation.kind == "imageGeneration" {
        item.saved_path
            .clone()
            .or_else(|| {
                item.result.as_ref().and_then(|result| {
                    self::text(result).map(|data| format!("data:image/png;base64,{data}"))
                })
            })
            .filter(|source| !source.is_empty())
            .into_iter()
            .collect()
    } else {
        item.extra
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| match part["type"].as_str() {
                Some("localImage") => self::text(&part["path"]),
                Some("image") => self::text(&part["url"]),
                _ => None,
            })
            .map(Cow::into_owned)
            .collect()
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
    ItemBody { text, images }
}

/// Borrow file display fields without rescanning or serializing the wire payload.
pub struct FileChange<'a> {
    pub path: Cow<'a, str>,
    pub kind: Cow<'a, str>,
    pub diff: Cow<'a, str>,
}
pub fn file_changes(item: &Item) -> impl Iterator<Item = FileChange<'_>> {
    let (files, unknown) = match &item.changes {
        Some(crate::models::ItemChanges::Files(files)) => (files.as_slice(), None),
        Some(crate::models::ItemChanges::Unknown(value)) => (&[][..], value.as_array()),
        None => (&[][..], None),
    };
    files
        .iter()
        .map(|change| {
            let kind = change.kind.as_ref().unwrap_or(&Value::Null);
            FileChange {
                path: Cow::Borrowed(change.path.as_deref().unwrap_or_default()),
                kind: text(kind).unwrap_or_else(|| field(kind, "type")),
                diff: Cow::Borrowed(change.diff.as_deref().unwrap_or_default()),
            }
        })
        .chain(unknown.into_iter().flatten().map(|change| FileChange {
            path: field(change, "path"),
            kind: text(&change["kind"]).unwrap_or_else(|| field(&change["kind"], "type")),
            diff: field(change, "diff"),
        }))
}

/// Potentially large output is formatted only when its row is expanded.
pub fn expanded_body(item: &Item) -> String {
    match item.kind.as_deref() {
        Some("userMessage" | "agentMessage") => message(item),
        Some("reasoning") => {
            let summary = item.extra.get("summary").unwrap_or(&Value::Null);
            if summary.is_array() {
                parts(summary, "\n")
            } else {
                text(summary)
                    .map(Cow::into_owned)
                    .unwrap_or_else(|| item.text.clone().unwrap_or_default())
            }
        }
        Some("commandExecution") => {
            let mut result = String::new();
            if let Some(cwd) = item.extra.get("cwd").and_then(text) {
                result.push_str("cwd: ");
                result.push_str(&cwd);
                if item.aggregated_output.is_some() {
                    result.push('\n');
                }
            }
            if let Some(output) = &item.aggregated_output {
                result.push_str(output);
            }
            result
        }
        Some("fileChange") => {
            let mut result = String::new();
            for (index, change) in file_changes(item).enumerate() {
                if index != 0 {
                    result.push_str("\n\n");
                }
                write!(result, "{}: {}\n{}", change.kind, change.path, change.diff)
                    .expect("writing a String cannot fail");
            }
            result
        }
        _ => serde_json::to_string_pretty(item).expect("Item serializes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[rstest::rstest]
    #[case::named_fields(
        json!([{"path":"/a.txt","kind":{"type":"update"},"diff":"-old\n+new"}, {"path":"/b.txt","kind":"add","diff":"+second"}]),
        &[("/a.txt", "update", "-old\n+new"), ("/b.txt", "add", "+second")]
    )]
    #[case::missing_diff(json!([{"path":"/a.txt","kind":"update"}]), &[("/a.txt", "update", "")])]
    #[case::empty(json!([]), &[])]
    #[case::null(json!(null), &[])]
    #[case::unknown(json!({"future":true}), &[])]
    fn file_display_keeps_paths_and_diffs_from_named_wire_fields(
        #[case] changes: Value,
        #[case] expected: &[(&str, &str, &str)],
    ) {
        let item: Item =
            serde_json::from_value(json!({"id":"files", "type":"fileChange", "changes":changes}))
                .unwrap();
        let changes: Vec<_> = file_changes(&item).collect();
        let displayed: Vec<_> = changes
            .iter()
            .map(|change| {
                (
                    change.path.as_ref(),
                    change.kind.as_ref(),
                    change.diff.as_ref(),
                )
            })
            .collect();
        assert_eq!(displayed, expected);
    }

    #[rstest::rstest]
    #[case::user_with_attachments(
        json!({"id":"user","type":"userMessage","content":[{"text":"hello"},{"type":"mention","name":"a.txt","path":"/a"},{"type":"localImage","path":"/photo.png"}]}),
        "hello\n添付: a.txt (/a)", "hello\n添付: a.txt (/a)", &["/photo.png"]
    )]
    #[case::reasoning_summary(
        json!({"id":"reason","type":"reasoning","summary":[{"text":"one"},{"unknown":true},"two"]}),
        "詳細を表示", "one\ntwo", &[]
    )]
    #[case::reasoning_text(
        json!({"id":"reason","type":"reasoning","text":"reasoning text"}),
        "詳細を表示", "reasoning text", &[]
    )]
    #[case::command(
        json!({"id":"cmd","type":"commandExecution","cwd":"/fixture","aggregatedOutput":"done"}),
        "詳細を表示", "cwd: /fixture\ndone", &[]
    )]
    #[case::file_change(
        json!({"id":"file","type":"fileChange","changes":[{"kind":{"type":"update"},"path":"/a","diff":"+line"}]}),
        "詳細を表示", "update: /a\n+line", &[]
    )]
    fn body_contract_covers_messages_images_and_details(
        #[case] wire: Value,
        #[case] collapsed: &str,
        #[case] expanded: &str,
        #[case] images: &[&str],
    ) {
        let item: Item = serde_json::from_value(wire).unwrap();
        let body = item_body(&item, &super::super::item_presentation(&item));
        assert_eq!(body.text, collapsed);
        assert_eq!(body.images, images);
        assert_eq!(expanded_body(&item), expanded);
    }

    #[test]
    fn body_contract_preserves_unknown_payloads_and_generated_image_paths() {
        let item: Item = serde_json::from_value(
            json!({"id":"future","type":"futureTool","extraPayload":{"array":[1,true]}}),
        )
        .unwrap();
        let expanded: Value = serde_json::from_str(&expanded_body(&item)).unwrap();
        assert_eq!(expanded["extraPayload"]["array"], json!([1, true]));
        assert_eq!(expanded["id"], "future");
        let generated: Item = serde_json::from_value(json!({"id":"image","type":"imageGeneration","savedPath":"/saved.png","result":"base64"})).unwrap();
        assert_eq!(
            item_body(&generated, &super::super::item_presentation(&generated)).images,
            ["/saved.png"]
        );
    }
}
