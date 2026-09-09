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
/// The original line is borrowed verbatim (apart from the line delimiter
/// removed by the JSONL reader). raw_id is the original top-level JSON
/// representation, not a parsed integer/string DTO. method is decoded only
/// because routing needs the method name; params, result, error, and unknown
/// fields are never deserialized here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcMessage<'a> {
    kind: RpcMessageKind,
    raw_line: &'a str,
    raw_id: Option<&'a str>,
    method: Option<String>,
}

impl<'a> RpcMessage<'a> {
    pub fn parse(line: &'a str) -> Result<Self, RpcMessageError> {
        let object = parse_object(line)?;
        classify_object(line, &object)
    }

    pub const fn kind(&self) -> RpcMessageKind {
        self.kind
    }

    /// Returns the top-level id exactly as it appeared in the source JSON.
    pub fn raw_id(&self) -> Option<&str> {
        self.raw_id
    }

    pub fn method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// Returns the original JSON object without its trailing JSONL newline.
    pub fn raw_line(&self) -> &str {
        self.raw_line
    }
}

/// Classifies one already-delimited JSONL message without decoding its
/// params/result/error values.
pub fn classify_message(line: &str) -> Result<RpcMessage<'_>, RpcMessageError> {
    RpcMessage::parse(line)
}

fn parse_object<'a, T: serde::Deserialize<'a>>(
    line: &'a str,
) -> Result<BTreeMap<String, T>, RpcMessageError> {
    // Parsing to RawValue validates the complete JSON document without
    // materializing nested values. A second parse then checks the root shape.
    let raw: &RawValue = serde_json::from_str(line)?;
    serde_json::from_str(raw.get()).map_err(|_| RpcMessageError::NotObject)
}

fn classify_object<'a>(
    line: &'a str,
    object: &BTreeMap<String, &'a RawValue>,
) -> Result<RpcMessage<'a>, RpcMessageError> {
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
    let raw_id = object.get("id").copied().map(RawValue::get);

    Ok(RpcMessage {
        kind,
        raw_line: line,
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

/// Replaces the top-level JSON-RPC id while retaining every other raw value.
/// Outer whitespace and key order are normalized; duplicate ids collapse to one.
pub fn rewrite_top_level_id(line: &str, replacement_id: &str) -> Result<String, RpcMessageError> {
    let mut fields: BTreeMap<String, &RawValue> = parse_object(line)?;
    classify_object(line, &fields)?;
    if fields.remove("id").is_none() {
        return Err(RpcMessageError::MissingIdForRewrite);
    }
    #[derive(serde::Serialize)]
    struct Rewritten<'a> {
        id: &'a RawValue,
        #[serde(flatten)]
        fields: BTreeMap<String, &'a RawValue>,
    }
    Ok(serde_json::to_string(&Rewritten {
        id: serde_json::from_str(replacement_id)?,
        fields,
    })?)
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
        for malformed in [r#"{"id":1,"result":[}"#, r#"{"id":1,"result":{}} {}"#] {
            assert!(matches!(
                classify_message(malformed),
                Err(RpcMessageError::Json(_))
            ));
        }
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
            r#"{"id":"proxy-7","method":"turn/start","params":{"text":"x","nested":{"id":1}},"unknown":{"keep":[1,{"value":true}]}}"#
        );
    }

    #[test]
    fn rewriting_id_preserves_escaped_keys_and_duplicate_id_safely() {
        let original = r#"{"\u0069d": 1, "method":"turn/start", "nested":{"id":2}, "id":"last"}"#;
        assert_eq!(
            rewrite_top_level_id(original, r#""proxy""#).unwrap(),
            r#"{"id":"proxy","method":"turn/start","nested":{"id":2}}"#
        );
        let original = r#" {"先頭":"値\\\"}]", "id" : [1,{"id":2}], "method":"x", "params":[{"text":"[{}]"}], "\u0069d": {"nested":true} } "#;
        let rewritten = rewrite_top_level_id(original, "null").unwrap();
        let mut expected: Value = serde_json::from_str(original).unwrap();
        expected["id"] = Value::Null;
        assert_eq!(serde_json::from_str::<Value>(&rewritten).unwrap(), expected);
        assert!(rewrite_top_level_id(original, "1 2").is_err());
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
