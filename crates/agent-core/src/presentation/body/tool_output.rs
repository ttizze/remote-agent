use crate::localization::UiLanguage;
use crate::presentation::markdown::{MarkdownBlock, MarkdownRun, markdown_code_block};
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

/// Render image payloads as images, retaining other output and structured data
/// without copying base64 into a second, visible JSON block.
pub(super) fn result_blocks(language: UiLanguage, result: &Value) -> Vec<MarkdownBlock> {
    let code = |result: &Value| {
        markdown_code_block(
            serde_json::to_string_pretty(result).expect("result serializes"),
            Some("json".into()),
            Some(language.text("結果").into()),
        )
    };
    let blocks = tool_output::blocks(result);
    if !blocks
        .iter()
        .any(|block| tool_output::image_data(block).is_some())
    {
        return vec![code(result)];
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
        images.push(code(&remaining));
    }
    images
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
}
