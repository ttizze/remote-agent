use std::collections::BTreeMap;

use serde_json::value::RawValue;

/// The only message distinction the Host needs for routing Codex traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcMessageKind {
    Request,
    Response,
    Notification,
}

/// A classified Codex JSONL line.
///
/// The original line is retained verbatim (apart from the line delimiter
/// removed by the JSONL reader). raw_id is the original top-level JSON
/// representation, not a parsed integer/string DTO. method is decoded only
/// because routing needs the method name; params, result, error, and unknown
/// fields are never deserialized here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcMessage {
    kind: RpcMessageKind,
    raw_line: String,
    raw_id: Option<String>,
    method: Option<String>,
}

impl RpcMessage {
    pub fn parse(line: &str) -> Result<Self, RpcMessageError> {
        let object = parse_object(line)?;
        classify_object(line, object)
    }

    pub const fn kind(&self) -> RpcMessageKind {
        self.kind
    }

    /// Returns the top-level id exactly as it appeared in the source JSON.
    pub fn raw_id(&self) -> Option<&str> {
        self.raw_id.as_deref()
    }

    pub fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// Returns the original JSON object without its trailing JSONL newline.
    pub fn raw_line(&self) -> &str {
        &self.raw_line
    }

    pub fn into_raw_line(self) -> String {
        self.raw_line
    }
}

/// Classifies one already-delimited JSONL message without decoding its
/// params/result/error values.
pub fn classify_message(line: &str) -> Result<RpcMessage, RpcMessageError> {
    RpcMessage::parse(line)
}

fn parse_object(line: &str) -> Result<BTreeMap<String, Box<RawValue>>, RpcMessageError> {
    // Parsing to RawValue validates the complete JSON document without
    // materializing nested values. A second parse then checks the root shape.
    let raw: Box<RawValue> = serde_json::from_str(line)?;
    serde_json::from_str(raw.get()).map_err(|_| RpcMessageError::NotObject)
}

fn classify_object(
    line: &str,
    object: BTreeMap<String, Box<RawValue>>,
) -> Result<RpcMessage, RpcMessageError> {
    let has_id = object.contains_key("id");
    let has_method = object.contains_key("method");
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");

    let kind = if has_result && has_error {
        return Err(RpcMessageError::BothResultAndError);
    } else if has_method {
        if has_result || has_error {
            return Err(RpcMessageError::InvalidCombination {
                reason: "method cannot be combined with result or error",
            });
        }
        if has_id {
            RpcMessageKind::Request
        } else {
            RpcMessageKind::Notification
        }
    } else if has_result || has_error {
        if !has_id {
            return Err(RpcMessageError::MissingIdForResponse);
        }
        RpcMessageKind::Response
    } else {
        return Err(RpcMessageError::InvalidCombination {
            reason: "message must contain method or result/error",
        });
    };

    let method = object
        .get("method")
        .map(|raw| {
            serde_json::from_str::<String>(raw.get()).map_err(|_| {
                RpcMessageError::InvalidFieldType {
                    field: "method",
                    expected: "a JSON string",
                }
            })
        })
        .transpose()?;
    let raw_id = object.get("id").map(|raw| raw.get().to_owned());

    Ok(RpcMessage {
        kind,
        raw_line: line.to_owned(),
        raw_id,
        method,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum RpcMessageError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("RPC message root must be a JSON object")]
    NotObject,
    #[error("RPC response cannot contain both result and error")]
    BothResultAndError,
    #[error("RPC response must contain a top-level id")]
    MissingIdForResponse,
    #[error("invalid RPC message combination: {reason}")]
    InvalidCombination { reason: &'static str },
    #[error("RPC field {field} must be {expected}")]
    InvalidFieldType {
        field: &'static str,
        expected: &'static str,
    },
    #[error("RPC request/message to rewrite is missing a top-level id")]
    MissingIdForRewrite,
}

/// Replaces only the top-level JSON-RPC id.
///
/// replacement_id must itself be one JSON value, such as 42 or
/// "proxy-7". Every other source byte, including whitespace, key order,
/// nested values, and unknown extensions, is preserved exactly.
pub fn rewrite_top_level_id(line: &str, replacement_id: &str) -> Result<String, RpcMessageError> {
    let message = RpcMessage::parse(line)?;
    if message.raw_id.is_none() {
        return Err(RpcMessageError::MissingIdForRewrite);
    }

    let _: Box<RawValue> = serde_json::from_str(replacement_id)?;
    let spans = top_level_member_value_spans(line, "id");
    if spans.is_empty() {
        return Err(RpcMessageError::MissingIdForRewrite);
    }

    // Replace every duplicate top-level id with the same value. JSON objects
    // should not contain duplicate members, but doing this avoids leaving an
    // ambiguous old id for parsers that choose the first rather than the last.
    let replaced_bytes = spans.iter().map(|(start, end)| end - start).sum::<usize>();
    let mut rewritten =
        String::with_capacity(line.len() - replaced_bytes + replacement_id.len() * spans.len());
    let mut copied_until = 0;
    for (start, end) in spans {
        rewritten.push_str(&line[copied_until..start]);
        rewritten.push_str(replacement_id);
        copied_until = end;
    }
    rewritten.push_str(&line[copied_until..]);
    Ok(rewritten)
}

/// Locates source spans for a top-level object's member value.
///
/// The caller has already validated the complete JSON with serde_json, so
/// this scanner only has to preserve source offsets; it does not duplicate
/// JSON validation or materialize nested values.
fn top_level_member_value_spans(line: &str, wanted: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut cursor = skip_whitespace(bytes, 0);
    if bytes.get(cursor) != Some(&b'{') {
        return Vec::new();
    }
    cursor += 1;
    let mut spans = Vec::new();

    loop {
        cursor = skip_whitespace(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return spans;
        }
        let key_start = cursor;
        let Some(key_end) = scan_string(bytes, cursor) else {
            return Vec::new();
        };
        let Ok(key) = serde_json::from_str::<String>(&line[key_start..key_end]) else {
            return Vec::new();
        };
        cursor = skip_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return Vec::new();
        }
        cursor = skip_whitespace(bytes, cursor + 1);
        let value_start = cursor;
        let Some(value_end) = scan_value(bytes, cursor) else {
            return Vec::new();
        };
        if key == wanted {
            spans.push((value_start, value_end));
        }
        cursor = skip_whitespace(bytes, value_end);
        match bytes.get(cursor) {
            Some(b',') => cursor += 1,
            Some(b'}') => return spans,
            _ => return Vec::new(),
        }
    }
}

fn skip_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
    {
        cursor += 1;
    }
    cursor
}

fn scan_string(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut cursor = start + 1;
    while let Some(byte) = bytes.get(cursor) {
        match byte {
            b'\\' => cursor = cursor.checked_add(2)?,
            b'"' => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

fn scan_value(bytes: &[u8], start: usize) -> Option<usize> {
    match bytes.get(start)? {
        b'"' => scan_string(bytes, start),
        b'{' | b'[' => scan_container(bytes, start),
        _ => {
            let mut cursor = start;
            while bytes.get(cursor).is_some_and(|byte| {
                !matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            }) {
                cursor += 1;
            }
            (cursor > start).then_some(cursor)
        }
    }
}

fn scan_container(bytes: &[u8], start: usize) -> Option<usize> {
    let mut stack = vec![match bytes.get(start)? {
        b'{' => b'}',
        b'[' => b']',
        _ => return None,
    }];
    let mut cursor = start + 1;
    while let Some(byte) = bytes.get(cursor) {
        match byte {
            b'"' => cursor = scan_string(bytes, cursor)?,
            b'{' => {
                stack.push(b'}');
                cursor += 1;
            }
            b'[' => {
                stack.push(b']');
                cursor += 1;
            }
            byte if Some(byte) == stack.last() => {
                stack.pop();
                cursor += 1;
                if stack.is_empty() {
                    return Some(cursor);
                }
            }
            _ => cursor += 1,
        }
    }
    None
}

/// Returns the top-level object as raw values for code that needs to inspect
/// an extension without turning params/result/error into typed DTOs.
pub fn raw_object(line: &str) -> Result<BTreeMap<String, Box<RawValue>>, RpcMessageError> {
    parse_object(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn classifies_requests_without_deserializing_nested_values() {
        let line = r#"{"jsonrpc":"2.0","id":"r-1","method":"turn/start","params":{"nested":{"id":99,"method":"not-top-level"}},"future":{"x":[1,2,3]}}"#;
        let message = classify_message(line).unwrap();

        assert_eq!(message.kind(), RpcMessageKind::Request);
        assert_eq!(message.raw_id(), Some(r#""r-1""#));
        assert_eq!(message.method(), Some("turn/start"));
        assert_eq!(message.raw_line(), line);
    }

    #[test]
    fn classifies_responses_and_notifications() {
        let response = classify_message(
            r#"{"id":42,"result":{"answer":{"id":"nested"}},"futureField":[true,null]}"#,
        )
        .unwrap();
        assert_eq!(response.kind(), RpcMessageKind::Response);
        assert_eq!(response.raw_id(), Some("42"));
        assert_eq!(response.method(), None);

        let notification = classify_message(
            r#"{"method":"item/started","params":{"result":"nested","error":false}}"#,
        )
        .unwrap();
        assert_eq!(notification.kind(), RpcMessageKind::Notification);
        assert_eq!(notification.raw_id(), None);
        assert_eq!(notification.method(), Some("item/started"));
    }

    #[test]
    fn rejects_invalid_message_combinations() {
        assert!(matches!(
            classify_message(r#"{"id":1,"result":{},"error":{}}"#),
            Err(RpcMessageError::BothResultAndError)
        ));
        assert!(matches!(
            classify_message(r#"{"result":{}}"#),
            Err(RpcMessageError::MissingIdForResponse)
        ));
        assert!(matches!(
            classify_message(r#"{"id":1}"#),
            Err(RpcMessageError::InvalidCombination { .. })
        ));
        assert!(matches!(
            classify_message(r#"{"id":1,"method":"x","result":{}}"#),
            Err(RpcMessageError::InvalidCombination { .. })
        ));
        assert!(matches!(
            classify_message(r#"{"method":42}"#),
            Err(RpcMessageError::InvalidFieldType {
                field: "method",
                ..
            })
        ));
        assert!(matches!(
            classify_message(r#"["method","nested"]"#),
            Err(RpcMessageError::NotObject)
        ));
    }

    #[test]
    fn rewrites_only_the_top_level_id_and_keeps_unknown_values() {
        let original = r#"{  "id" : "original" , "method":"turn/start","params":{"text":"x","nested":{"id":1}},"unknown":{"keep":[1,{"value":true}]}}"#;
        let rewritten = rewrite_top_level_id(original, r#""proxy-7""#).unwrap();
        let original_value: Value = serde_json::from_str(original).unwrap();
        let rewritten_value: Value = serde_json::from_str(&rewritten).unwrap();

        assert_eq!(rewritten_value["id"], json!("proxy-7"));
        assert_eq!(rewritten_value["method"], original_value["method"]);
        assert_eq!(rewritten_value["params"], original_value["params"]);
        assert_eq!(rewritten_value["unknown"], original_value["unknown"]);
        assert_eq!(
            rewritten,
            r#"{  "id" : "proxy-7" , "method":"turn/start","params":{"text":"x","nested":{"id":1}},"unknown":{"keep":[1,{"value":true}]}}"#
        );
    }

    #[test]
    fn rewriting_id_preserves_escaped_keys_and_duplicate_id_safely() {
        let original = r#"{"\u0069d": 1, "method":"turn/start", "nested":{"id":2}, "id":"last"}"#;
        assert_eq!(
            rewrite_top_level_id(original, r#""proxy""#).unwrap(),
            r#"{"\u0069d": "proxy", "method":"turn/start", "nested":{"id":2}, "id":"proxy"}"#
        );
    }

    #[test]
    fn raw_object_preserves_extension_values_without_typed_decoding() {
        let line =
            r#"{"method":"event","params":{"future":{"deep":[1,2]}},"extension":{"raw":true}}"#;
        let object = raw_object(line).unwrap();
        assert_eq!(object["params"].get(), r#"{"future":{"deep":[1,2]}}"#);
        assert_eq!(object["extension"].get(), r#"{"raw":true}"#);
    }
}
