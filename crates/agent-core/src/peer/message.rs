use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};
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
/// Envelope values borrow the original line without materializing payloads.
/// raw_id is the original top-level JSON
/// representation, not a parsed integer/string DTO. method is decoded only
/// because routing needs the method name; params, result, error, and unknown
/// fields are never deserialized here.
#[derive(Debug)]
pub struct RpcMessage<'a> {
    kind: RpcMessageKind,
    raw_id: Option<&'a str>,
    method: Option<String>,
    fields: BTreeMap<String, &'a RawValue>,
}

impl<'a> RpcMessage<'a> {
    pub fn parse(line: &'a str) -> Result<Self, RpcMessageError> {
        let object = parse_object(line)?;
        classify_object(object)
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

    /// Decode only the known request parameters; unknown envelope fields stay raw.
    pub fn params<T: Deserialize<'a>>(&self) -> Result<T, RpcMessageError> {
        Ok(serde_json::from_str(
            self.fields.get("params").map_or("{}", |raw| raw.get()),
        )?)
    }

    pub(crate) fn rewrite_id(&self, replacement_id: &str) -> Result<String, RpcMessageError> {
        if self.raw_id.is_none() {
            return Err(RpcMessageError::MissingIdForRewrite);
        }
        #[derive(Serialize)]
        struct Rewritten<'a, 'b> {
            id: &'b RawValue,
            #[serde(flatten)]
            retained: RetainedFields<'a, 'b>,
        }
        Ok(serde_json::to_string(&Rewritten {
            id: serde_json::from_str(replacement_id)?,
            retained: self.retained(&["id"]),
        })?)
    }

    pub fn raw_result(&self) -> Option<&'a RawValue> {
        self.fields.get("result").copied()
    }

    pub fn raw_error(&self) -> Option<&'a RawValue> {
        self.fields.get("error").copied()
    }

    /// Replace the routed method and parameters, retaining the caller's ID and extensions.
    pub fn request<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<String, RpcMessageError> {
        #[derive(Serialize)]
        struct Request<'a, 'b, P> {
            method: &'b str,
            params: &'b P,
            #[serde(flatten)]
            retained: RetainedFields<'a, 'b>,
        }
        Ok(serde_json::to_string(&Request {
            method,
            params,
            retained: self.retained(&["method", "params"]),
        })?)
    }

    /// Serialize a typed success or failure with the originating envelope's metadata.
    pub fn response<T: Serialize, E: Serialize>(
        &self,
        outcome: Result<T, E>,
    ) -> Result<String, RpcMessageError> {
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Payload<T, E> {
            Success { result: T },
            Failure { error: E },
        }
        #[derive(Serialize)]
        struct Response<'a, 'b, T, E> {
            #[serde(flatten)]
            retained: RetainedFields<'a, 'b>,
            #[serde(flatten)]
            payload: Payload<T, E>,
        }
        Ok(serde_json::to_string(&Response {
            retained: self.retained(&["method", "params", "result", "error"]),
            payload: match outcome {
                Ok(result) => Payload::Success { result },
                Err(error) => Payload::Failure { error },
            },
        })?)
    }

    /// Forward an enriched upstream envelope, or attach a local failure to the caller's envelope.
    pub fn forward_response<T: Serialize, E: Serialize>(
        &self,
        response: Result<RpcResponse<T>, E>,
    ) -> Result<String, RpcMessageError> {
        match response {
            Ok(response) => Ok(serde_json::to_string(&response)?),
            Err(error) => self.response::<(), E>(Err(error)),
        }
    }

    pub fn error<C: Serialize>(
        &self,
        code: C,
        message: &dyn std::fmt::Display,
    ) -> Result<String, RpcMessageError> {
        #[derive(Serialize)]
        struct Fault<'a, C> {
            code: C,
            message: DisplayMessage<'a>,
        }
        self.response::<(), _>(Err(Fault {
            code,
            message: DisplayMessage(message),
        }))
    }

    fn retained<'b>(&'b self, excluded: &'static [&'static str]) -> RetainedFields<'a, 'b> {
        RetainedFields {
            fields: &self.fields,
            excluded,
        }
    }
}

/// A typed response payload with opaque envelope extensions. Forwarders may
/// enrich the result without discarding upstream metadata or a remote error.
#[derive(Debug)]
pub struct RpcResponse<T> {
    pub outcome: Result<T, Box<RawValue>>,
    fields: BTreeMap<String, Box<RawValue>>,
}
impl<T: serde::de::DeserializeOwned> RpcResponse<T> {
    pub fn parse(line: &str) -> Result<Self, RpcMessageError> {
        let mut message = RpcMessage::parse(line)?;
        if message.kind != RpcMessageKind::Response {
            return Err(RpcMessageError::InvalidCombination {
                reason: "message was not a response",
            });
        }
        let outcome = if let Some(error) = message.fields.remove("error") {
            Err(error.to_owned())
        } else {
            let result = message
                .fields
                .remove("result")
                .expect("classified response has a payload");
            Ok(serde_json::from_str(result.get())?)
        };
        Ok(Self {
            outcome,
            fields: message
                .fields
                .into_iter()
                .map(|(key, value)| (key, value.to_owned()))
                .collect(),
        })
    }
}
impl<T> RpcResponse<T> {
    /// A successful internal read consumes its envelope. An upstream failure can
    /// cross forwarding stages unchanged because it contains no success payload.
    pub fn into_result<U>(self) -> Result<T, RpcResponse<U>> {
        match self.outcome {
            Ok(value) => Ok(value),
            Err(error) => Err(RpcResponse {
                outcome: Err(error),
                fields: self.fields,
            }),
        }
    }
}
impl<T: Serialize> Serialize for RpcResponse<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len() + 1))?;
        for (key, value) in &self.fields {
            map.serialize_entry(key, value)?;
        }
        match &self.outcome {
            Ok(result) => map.serialize_entry("result", result)?,
            Err(error) => map.serialize_entry("error", error)?,
        }
        map.end()
    }
}

struct RetainedFields<'a, 'b> {
    fields: &'b BTreeMap<String, &'a RawValue>,
    excluded: &'static [&'static str],
}
impl Serialize for RetainedFields<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.fields {
            if !self.excluded.contains(&key.as_str()) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}
struct DisplayMessage<'a>(&'a dyn std::fmt::Display);
impl Serialize for DisplayMessage<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self.0)
    }
}

fn parse_object<'a, T: serde::Deserialize<'a>>(
    line: &'a str,
) -> Result<BTreeMap<String, T>, RpcMessageError> {
    serde_json::from_str(line).map_err(|error| {
        if error.is_data() {
            // Validate malformed non-object roots only on the error path.
            serde_json::from_str::<&RawValue>(line)
                .err()
                .map_or(RpcMessageError::NotObject, RpcMessageError::Json)
        } else {
            RpcMessageError::Json(error)
        }
    })
}

fn classify_object<'a>(
    object: BTreeMap<String, &'a RawValue>,
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
        raw_id,
        method,
        fields: object,
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
    RpcMessage::parse(line)?.rewrite_id(replacement_id)
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
        let message = RpcMessage::parse(line).unwrap();

        assert_eq!(message.kind(), RpcMessageKind::Request);
        assert_eq!(message.raw_id(), Some(r#""r-1""#));
        assert_eq!(message.method(), Some("turn/start"));
    }

    #[test]
    fn classifies_responses_and_notifications() {
        let response = RpcMessage::parse(
            r#"{"id":42,"result":{"answer":{"id":"nested"}},"futureField":[true,null]}"#,
        )
        .unwrap();
        assert_eq!(response.kind(), RpcMessageKind::Response);
        assert_eq!(response.raw_id(), Some("42"));
        assert_eq!(response.method(), None);

        let notification = RpcMessage::parse(
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
            RpcMessage::parse(r#"{"id":1,"result":{},"error":{}}"#),
            Err(RpcMessageError::BothResultAndError)
        ));
        assert!(matches!(
            RpcMessage::parse(r#"{"result":{}}"#),
            Err(RpcMessageError::MissingIdForResponse)
        ));
        assert!(matches!(
            RpcMessage::parse(r#"{"id":1}"#),
            Err(RpcMessageError::InvalidCombination { .. })
        ));
        assert!(matches!(
            RpcMessage::parse(r#"{"id":1,"method":"x","result":{}}"#),
            Err(RpcMessageError::InvalidCombination { .. })
        ));
        assert!(matches!(
            RpcMessage::parse(r#"{"method":42}"#),
            Err(RpcMessageError::InvalidFieldType {
                field: "method",
                ..
            })
        ));
        assert!(matches!(
            RpcMessage::parse(r#"["method","nested"]"#),
            Err(RpcMessageError::NotObject)
        ));
        for malformed in [r#"{"id":1,"result":[}"#, r#"{"id":1,"result":{}} {}"#] {
            assert!(matches!(
                RpcMessage::parse(malformed),
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
    #[test]
    fn host_response_keeps_request_extensions_and_raw_id() {
        let line = r#"{"jsonrpc":"2.0","id":"mobile-1","method":"host/thread/list","params":{"future":{"id":9}},"extension":{"keep":[1,true]}}"#;
        let response = RpcMessage::parse(line)
            .unwrap()
            .response::<_, ()>(Ok(json!({"data":[]})))
            .unwrap();
        let object = raw_object(&response).unwrap();
        assert_eq!(object["id"].get(), r#""mobile-1""#);
        assert_eq!(object["extension"].get(), r#"{"keep":[1,true]}"#);
        assert_eq!(object["result"].get(), r#"{"data":[]}"#);
        assert!(!object.contains_key("params"));
        assert!(!object.contains_key("method"));
    }

    #[test]
    fn typed_forwarding_preserves_upstream_metadata_and_errors() {
        #[derive(Debug, Deserialize, Serialize)]
        struct Page {
            data: Vec<String>,
        }
        let request = RpcMessage::parse(
            r#"{"id":["mobile",1],"method":"thread/list","extension":"request"}"#,
        )
        .unwrap();
        let mut response = RpcResponse::<Page>::parse(
            r#"{"jsonrpc":"2.0","id":["mobile",1],"result":{"data":["first"]},"extension":{"upstream":[true,null]}}"#,
        )
        .unwrap();
        response
            .outcome
            .as_mut()
            .unwrap()
            .data
            .push("second".into());
        let line = request.forward_response::<_, ()>(Ok(response)).unwrap();
        let object = raw_object(&line).unwrap();
        assert_eq!(object["id"].get(), r#"["mobile",1]"#);
        assert_eq!(object["extension"].get(), r#"{"upstream":[true,null]}"#);
        assert_eq!(object["result"].get(), r#"{"data":["first","second"]}"#);

        let error = r#"{"id":["mobile",1],"error":{"code":"future","data":[1,{"nested":null}]},"extension":{"upstream":true}}"#;
        let forwarded: RpcResponse<Page> = RpcResponse::<String>::parse(error)
            .unwrap()
            .into_result()
            .unwrap_err();
        let line = request.forward_response::<_, ()>(Ok(forwarded)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            serde_json::from_str::<Value>(error).unwrap()
        );
    }
}
