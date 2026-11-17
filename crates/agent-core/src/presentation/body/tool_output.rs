use crate::localization::UiLanguage;
use crate::presentation::markdown::{MarkdownBlock, MarkdownRun, markdown_code_block};
use crate::text::trim;
use agent_protocol::tool_output;
use serde_json::Value;

pub(super) fn image_block(language: UiLanguage, source: String) -> MarkdownBlock {
    MarkdownBlock::Paragraph {
        runs: vec![MarkdownRun {
            text: language.text("ツールの画像").into(),
            image: Some(source),
            ..Default::default()
        }],
        style: Default::default(),
    }
}

/// One labeled value for a tool's input, result or error. String payloads stay
/// text; typed values retain their JSON representation.
pub(super) fn value_block(
    language: UiLanguage,
    value: &Value,
    label: &str,
) -> Option<MarkdownBlock> {
    if value.as_str().is_some_and(|text| trim(text).is_empty()) {
        return None;
    }
    let (text, json) = match value {
        Value::String(text) => formatted_string(text),
        _ => (
            serde_json::to_string_pretty(value).expect("value serializes"),
            true,
        ),
    };
    Some(markdown_code_block(
        text,
        json.then(|| "json".into()),
        Some(language.text(label).into()),
    ))
}

/// Render image payloads as images, retaining other output and structured data
/// without copying base64 into a second, visible JSON block.
pub(super) fn result_blocks(language: UiLanguage, result: &Value) -> Vec<MarkdownBlock> {
    let blocks = tool_output::blocks(result);
    if !blocks
        .iter()
        .any(|block| tool_output::image_data(block).is_some())
    {
        return value_block(language, result, "結果").into_iter().collect();
    }
    let mut images = Vec::new();
    let remaining: Vec<_> = blocks
        .iter()
        .filter_map(|block| {
            if let Some((mime, data)) = tool_output::image_data(block) {
                images.push(image_block(language, format!("data:{mime};base64,{data}")));
                None
            } else {
                Some(block.clone())
            }
        })
        .collect();
    let remaining = if let Some(object) = result
        .as_object()
        .filter(|object| object.get("content").is_some_and(Value::is_array))
    {
        let mut object: serde_json::Map<_, _> = object
            .iter()
            .filter(|(key, _)| key.as_str() != "content")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if !remaining.is_empty() {
            object.insert("content".into(), Value::Array(remaining));
        }
        (!object.is_empty()).then_some(Value::Object(object))
    } else if result.is_array() && !remaining.is_empty() {
        Some(Value::Array(remaining))
    } else {
        None
    };
    if let Some(remaining) = remaining {
        images.extend(value_block(language, &remaining, "結果"));
    }
    images
}

/// T3's string outputs stay literal except complete JSON objects/arrays and
/// newline-delimited JSON documents, which are indented for reading.
fn formatted_string(text: &str) -> (String, bool) {
    let trimmed = trim(text);
    if trimmed.starts_with(['{', '[']) {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            return (
                serde_json::to_string_pretty(&value).expect("value serializes"),
                true,
            );
        }
        let documents = trimmed
            .lines()
            .filter(|line| !trim(line).is_empty())
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>();
        if let Ok(documents) = documents {
            return (
                documents
                    .iter()
                    .map(|value| serde_json::to_string_pretty(value).expect("value serializes"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                true,
            );
        }
    }
    (text.to_owned(), is_json_text(text))
}

fn is_json_text(text: &str) -> bool {
    if serde_json::from_str::<Value>(text).is_ok() {
        return true;
    }
    let mut documents = Vec::new();
    let mut current = String::new();
    for line in trim(text).lines() {
        if trim(line).is_empty() {
            if !current.is_empty() {
                documents.push(std::mem::take(&mut current));
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    if !current.is_empty() {
        documents.push(current);
    }
    documents.len() > 1
        && documents
            .iter()
            .all(|document| serde_json::from_str::<Value>(document).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{models::Item, presentation::body::detail_blocks};
    use serde_json::json;

    fn tool(result: Value, content: Value) -> Item {
        serde_json::from_value(
            json!({"id":"capture","status":"completed","body":{"inline":{"body":{"toolCall":{
                "kind":"mcp","tool":"capture","arguments":null,"result":result,"content":content
            }}}}}),
        )
        .unwrap()
    }

    fn rendered_images(blocks: &[MarkdownBlock]) -> Vec<&str> {
        blocks
            .iter()
            .filter_map(|block| match block {
                MarkdownBlock::Paragraph { runs, .. } => Some(runs),
                _ => None,
            })
            .flatten()
            .filter_map(|run| run.image.as_deref())
            .collect()
    }

    #[test]
    fn mcp_images_render_with_text_resources_and_structured_content_preserved() {
        let result = json!({"content":[
            {"type":"text","text":"Blog screenshot"},
            {"type":"image","mimeType":"image/png","data":"aW1hZ2U="},
            {"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"cGhvdG8="}},
            {"type":"resource_link","uri":"file:///tmp/report.txt"}
        ],"structuredContent":{"width":1280},"isError":false});
        let item = tool(result.clone(), json!([]));
        let blocks = detail_blocks(UiLanguage::English, &item);
        assert_eq!(
            rendered_images(&blocks),
            [
                "data:image/png;base64,aW1hZ2U=",
                "data:image/jpeg;base64,cGhvdG8="
            ]
        );
        let code: String = blocks
            .iter()
            .find_map(|block| match block {
                MarkdownBlock::Paragraph { runs, style } if style.code => Some(runs),
                _ => None,
            })
            .unwrap()
            .iter()
            .map(|run| run.text.as_str())
            .collect();
        assert!(code.contains("Blog screenshot"));
        assert!(code.contains("file:///tmp/report.txt"));
        assert!(code.contains("width"));
        assert!(!code.contains("aW1hZ2U="));
        assert!(!code.contains("cGhvdG8="));
        assert!(
            matches!(item.body(), crate::models::ItemBody::ToolCall {result:Some(saved),..} if saved == &result)
        );
    }

    #[test]
    fn image_only_results_show_no_base64_json_in_any_envelope() {
        let image = json!({"type":"image","mimeType":"image/png","data":"aW1hZ2U="});
        for result in [image.clone(), json!([image]), json!({"content":[image]})] {
            let blocks = detail_blocks(UiLanguage::Japanese, &tool(result, json!([])));
            assert_eq!(rendered_images(&blocks), ["data:image/png;base64,aW1hZ2U="]);
            assert_eq!(blocks.len(), 1);
        }
    }

    #[test]
    fn exec_content_images_and_text_keep_their_existing_order() {
        let item = tool(
            Value::Null,
            json!([
                {"text":{"text":"Captured screenshot"}},
            {"image":{"dataUrl":"data:image/png;base64,aW1hZ2U="}}
            ]),
        );
        let blocks = detail_blocks(UiLanguage::Japanese, &item);
        assert_eq!(rendered_images(&blocks), ["data:image/png;base64,aW1hZ2U="]);
        assert!(
            matches!(&blocks[0], MarkdownBlock::Paragraph { runs, .. } if runs.iter().any(|run| run.text == "Captured screenshot"))
        );
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn malformed_or_unsupported_images_keep_the_original_result_readable() {
        for result in [
            json!({"content":[{"type":"image","mimeType":"image/png","data":3}]}),
            json!({"content":[{"type":"image","mimeType":"image/svg+xml","data":"<svg/>"}]}),
            json!({"content":[{"type":"text","text":"No images"}]}),
        ] {
            let blocks = detail_blocks(UiLanguage::English, &tool(result.clone(), json!([])));
            assert!(rendered_images(&blocks).is_empty());
            assert_eq!(
                blocks,
                vec![markdown_code_block(
                    serde_json::to_string_pretty(&result).unwrap(),
                    Some("json".into()),
                    Some("Result".into())
                )]
            );
        }
    }

    #[test]
    fn string_results_keep_literal_text_and_classify_complete_json_documents() {
        for (raw, expected, syntax) in [
            (" **literal**\n日本語 ", " **literal**\n日本語 ", None),
            ("{\"ok\":true}", "{\n  \"ok\": true\n}", Some("json")),
            (
                "{\"a\":1}\n{\"b\":2}",
                "{\n  \"a\": 1\n}\n\n{\n  \"b\": 2\n}",
                Some("json"),
            ),
            ("{\"a\":\n", "{\"a\":\n", None),
            ("1\n \t\n2", "1\n \t\n2", Some("json")),
            ("1\n2", "1\n2", None),
            ("1\n2\n\n3", "1\n2\n\n3", None),
            ("true", "true", Some("json")),
            ("42", "42", Some("json")),
            ("1\n\u{3000}", "1\n\u{3000}", None),
            (
                "\u{feff}{\"ok\":true}",
                "{\n  \"ok\": true\n}",
                Some("json"),
            ),
            ("\u{0085}", "\u{0085}", None),
            ("{}\ninvalid", "{}\ninvalid", None),
        ] {
            let blocks = result_blocks(UiLanguage::English, &json!(raw));
            let [MarkdownBlock::Paragraph { runs, style }] = blocks.as_slice() else {
                panic!("expected one literal output code block")
            };
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                expected
            );
            assert_eq!(style.language.as_deref(), syntax, "{raw:?}");
            assert_eq!(style.filename.as_deref(), Some("Result"));
        }
        for empty in ["", " \n\t", "\u{3000}", "\u{feff}"] {
            assert!(result_blocks(UiLanguage::English, &json!(empty)).is_empty());
        }
    }

    #[test]
    fn input_and_error_strings_share_literal_value_formatting_and_keep_structured_fields() {
        for (label, value, expected, syntax) in [
            (
                "入力",
                json!("opaque input\nline two"),
                "opaque input\nline two",
                None,
            ),
            (
                "エラー",
                json!("failure\nretry detail"),
                "failure\nretry detail",
                None,
            ),
            (
                "入力",
                json!({"clear": null, "value": ""}),
                "{\n  \"clear\": null,\n  \"value\": \"\"\n}",
                Some("json"),
            ),
        ] {
            let Some(MarkdownBlock::Paragraph { runs, style }) =
                value_block(UiLanguage::English, &value, label)
            else {
                panic!("expected code value")
            };
            assert_eq!(
                runs.iter().map(|run| run.text.as_str()).collect::<String>(),
                expected
            );
            assert_eq!(style.language.as_deref(), syntax);
        }
        assert!(value_block(UiLanguage::English, &json!("\n\t"), "入力").is_none());
    }
}
