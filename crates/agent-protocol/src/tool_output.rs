//! Standard tool result blocks shared by provider history and client rendering.
use serde_json::Value;

pub fn blocks(result: &Value) -> &[Value] {
    result
        .as_array()
        .or_else(|| result.get("content").and_then(Value::as_array))
        .map_or_else(|| std::slice::from_ref(result), Vec::as_slice)
}

/// Identify supported raster image blocks, including metadata whose image
/// data remains behind hydration. Rendering separately requires actual bytes.
pub fn image_mime(block: &Value) -> Option<&'static str> {
    if block.get("type")?.as_str()? != "image" {
        return None;
    }
    let mime = match block.get("source") {
        Some(source) if source.get("type")?.as_str()? == "base64" => {
            source.get("media_type")?.as_str()?
        }
        Some(_) => return None,
        None => block.get("mimeType")?.as_str()?,
    };
    ["image/png", "image/jpeg", "image/gif", "image/webp"]
        .into_iter()
        .find(|supported| supported.eq_ignore_ascii_case(mime))
}

/// Borrow raster image data without copying base64 just to classify a result.
/// Other blocks remain available as ordinary tool output.
pub fn image_data(block: &Value) -> Option<(&'static str, &str)> {
    let mime = image_mime(block)?;
    let data = block.get("source").unwrap_or(block).get("data")?.as_str()?;
    (!data.is_empty()).then_some((mime, data))
}

/// T3's bounded result-envelope reader. Parsing limits cover the entire tree,
/// including JSON strings nested inside MCP content blocks.
struct ReadBudget {
    bytes: usize,
    nodes: usize,
    exceeded: bool,
}

fn read_result(value: &Value, budget: &mut ReadBudget, depth: usize) -> (Option<Value>, bool) {
    if depth > 4 || budget.nodes == 0 {
        budget.exceeded = true;
        return (None, false);
    }
    budget.nodes -= 1;
    match value {
        Value::String(text) => {
            if text.len() > budget.bytes {
                budget.exceeded = true;
                return (None, false);
            }
            budget.bytes -= text.len();
            serde_json::from_str(text)
                .ok()
                .map(|value| read_result(&value, budget, depth + 1))
                .unwrap_or((None, false))
        }
        Value::Array(blocks) => {
            if blocks.len() > 32 {
                budget.exceeded = true;
                return (None, false);
            }
            let mut data = None;
            let mut failed = false;
            for block in blocks {
                let text = block.get("text").unwrap_or(&Value::Null);
                let (next, error) = read_result(
                    text.get("text")
                        .filter(|text| !text.is_null())
                        .unwrap_or(text),
                    budget,
                    depth + 1,
                );
                data = data.or(next);
                failed |= error;
                if budget.exceeded {
                    break;
                }
            }
            (data, failed)
        }
        Value::Object(object) => {
            let failed = object.get("isError") == Some(&Value::Bool(true))
                || object.get("is_error") == Some(&Value::Bool(true))
                || object.get("_tag").and_then(Value::as_str) == Some("OrchestratorMcpFailure")
                || object.get("error").is_some_and(|value| !value.is_null());
            if let Some(content) = object
                .get("structuredContent")
                .filter(|value| !value.is_null())
                .or_else(|| object.get("content"))
            {
                let (data, nested_failed) = read_result(content, budget, depth + 1);
                (data, failed || nested_failed)
            } else {
                (Some(value.clone()), failed)
            }
        }
        _ => (None, false),
    }
}

fn bounded_id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.encode_utf16().count() <= 256)
}

/// Metadata that ordinary turn-item frames need; tool text and image data are
/// fetched as item details. This follows T3's compactDynamicToolOutput.
pub fn compact(value: &Value) -> Option<Value> {
    let mut budget = ReadBudget {
        bytes: 16_384,
        nodes: 128,
        exceeded: false,
    };
    let (data, failed) = read_result(value, &mut budget, 0);
    let mut output = serde_json::Map::new();
    if failed {
        output.insert("isError".into(), true.into());
    }
    if let Some(data) = data.filter(|_| !budget.exceeded) {
        for key in ["threadId", "messageId", "taskId", "scheduledTaskId"] {
            if let Some(id) = data.get(key).and_then(bounded_id) {
                output.insert(key.into(), id.into());
            }
        }
        if data.get("status").and_then(Value::as_str) == Some("rolled_back") {
            output.insert("status".into(), "rolled_back".into());
        }
        for key in ["linkedCount", "unlinkedCount"] {
            if let Some(count) = data
                .get(key)
                .and_then(Value::as_f64)
                .filter(|count| count.fract() == 0.0 && count.abs() <= 9_007_199_254_740_991.0)
            {
                output.insert(key.into(), (count as i64).into());
            }
        }
        if let Some(text) = data
            .get("sentMessage")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty() && text.encode_utf16().count() <= 4_000)
        {
            output.insert("sentMessage".into(), text.into());
        }
        if let Some(id) = data
            .get("thread")
            .and_then(|thread| thread.get("threadId"))
            .and_then(bounded_id)
        {
            output.insert("thread".into(), serde_json::json!({"threadId":id}));
        }
        if let Some(threads) = data.get("threads").and_then(Value::as_array) {
            let compact = (threads.len() <= 100)
                .then(|| {
                    threads
                        .iter()
                        .map(|entry| {
                            entry.as_object()?;
                            let id = entry.get("threadId").and_then(bounded_id);
                            let rolled_back =
                                entry.get("status").and_then(Value::as_str) == Some("rolled_back");
                            if id.is_none() && !rolled_back {
                                return None;
                            }
                            let mut thread = serde_json::Map::new();
                            if let Some(id) = id {
                                thread.insert("threadId".into(), id.into());
                            }
                            if rolled_back {
                                thread.insert("status".into(), "rolled_back".into());
                            }
                            Some(Value::Object(thread))
                        })
                        .collect::<Option<Vec<_>>>()
                })
                .flatten();
            if let Some(threads) = compact {
                output.insert("threads".into(), threads.into());
            } else {
                output.remove("threadId");
                output.remove("status");
            }
        }
    }
    if serde_json::to_vec(&output)
        .expect("JSON values serialize")
        .len()
        > 8_192
    {
        output.remove("threads");
        output.remove("threadId");
        output.remove("status");
    }
    (!output.is_empty()).then_some(Value::Object(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_results_preserve_errors_and_do_not_report_partial_thread_batches() {
        let result = json!({"isError":true,"structuredContent":{"threadId":"one","status":"rolled_back","threads":[{"threadId":"one"},{}]}});
        assert_eq!(compact(&result), Some(json!({"isError":true})));
        assert_eq!(
            compact(&json!({"content":[{"type":"text","text":"{\"taskId\":\"task\"}"}]})),
            Some(json!({"taskId":"task"}))
        );
        assert_eq!(
            compact(&json!({"threads":[{"threadId":"one"},{"status":"rolled_back"}]})),
            Some(json!({"threads":[{"threadId":"one"},{"status":"rolled_back"}]}))
        );
        assert_eq!(compact(&json!({"linkedCount":9007199254740992_i64})), None);
        assert_eq!(
            compact(&json!({"linkedCount":3.0})),
            Some(json!({"linkedCount":3}))
        );
        assert_eq!(
            compact(&json!({"isError":true,"content":[{"text":"x".repeat(20_000)}]})),
            Some(json!({"isError":true}))
        );
    }

    #[test]
    fn reads_flat_and_base64_source_images_in_every_result_envelope() {
        for image in [
            json!({"type":"image","mimeType":"IMAGE/PNG","data":"aW1hZ2U="}),
            json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}),
        ] {
            for result in [
                image.clone(),
                json!([image]),
                json!({"content":[image],"structuredContent":{"count":1}}),
            ] {
                let images: Vec<_> = blocks(&result).iter().filter_map(image_data).collect();
                assert_eq!(images, [("image/png", "aW1hZ2U=")]);
            }
        }
    }

    #[test]
    fn unsupported_and_incomplete_image_blocks_stay_ordinary_output() {
        for block in [
            json!({"type":"text","mimeType":"image/png","data":"data"}),
            json!({"type":"image","mimeType":"image/svg+xml","data":"data"}),
            json!({"type":"image","mimeType":"text/html","data":"data"}),
            json!({"type":"image","mimeType":"image/png","data":""}),
            json!({"type":"image","mimeType":"image/png","data":3}),
            json!({"type":"image","data":"data"}),
            json!({"type":"image","source":{"type":"url","url":"https://example.test/image.png"}}),
            json!({"type":"image","source":null}),
        ] {
            assert_eq!(image_data(&block), None);
            assert_eq!(blocks(&block), std::slice::from_ref(&block));
        }
    }

    #[test]
    fn recognizes_each_supported_raster_format() {
        for (mime, expected) in [
            ("image/png", "image/png"),
            ("IMAGE/JPEG", "image/jpeg"),
            ("image/gif", "image/gif"),
            ("IMAGE/WEBP", "image/webp"),
        ] {
            assert_eq!(
                image_data(&json!({"type":"image","mimeType":mime,"data":"aW1hZ2U="})),
                Some((expected, "aW1hZ2U="))
            );
        }
    }
}
