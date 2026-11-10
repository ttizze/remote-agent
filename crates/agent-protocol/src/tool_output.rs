//! Standard tool result blocks shared by provider history and client rendering.
use serde_json::Value;

pub fn blocks(result: &Value) -> &[Value] {
    result
        .as_array()
        .or_else(|| result.get("content").and_then(Value::as_array))
        .map_or_else(|| std::slice::from_ref(result), Vec::as_slice)
}

/// Borrow raster image data without copying base64 just to classify a result.
/// Other blocks remain available as ordinary tool output.
pub fn image_data(block: &Value) -> Option<(&'static str, &str)> {
    if block.get("type")?.as_str()? != "image" {
        return None;
    }
    let (mime, data) = match block.get("source") {
        Some(source) if source.get("type")?.as_str()? == "base64" => (
            source.get("media_type")?.as_str()?,
            source.get("data")?.as_str()?,
        ),
        Some(_) => return None,
        None => (
            block.get("mimeType")?.as_str()?,
            block.get("data")?.as_str()?,
        ),
    };
    let mime = ["image/png", "image/jpeg", "image/gif", "image/webp"]
        .into_iter()
        .find(|supported| supported.eq_ignore_ascii_case(mime))?;
    (!data.is_empty()).then_some((mime, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
