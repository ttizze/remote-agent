use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
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
/// IDs and payloads retain their raw JSON representation. Unknown envelope
/// fields are discarded; only the provider protocol fields are retained.
#[derive(Debug)]
pub struct RpcMessage<'a> {
    kind: RpcMessageKind,
    raw_id: Option<&'a str>,
    method: Option<String>,
    fields: BTreeMap<String, &'a RawValue>,
}

impl<'a> RpcMessage<'a> {
    pub fn parse(line: &'a str) -> Result<Self, RpcMessageError> {
        let object = serde_json::from_str(line).map_err(|error| {
            if error.is_data() {
                // Distinguish a valid non-object root from malformed JSON.
                serde_json::from_str::<&RawValue>(line)
                    .err()
                    .map_or(RpcMessageError::NotObject, RpcMessageError::Json)
            } else {
                RpcMessageError::Json(error)
            }
        })?;
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

    pub fn rewrite_id(&self, replacement_id: &str) -> Result<String, RpcMessageError> {
        if self.raw_id.is_none() {
            return Err(RpcMessageError::MissingIdForRewrite);
        }
        let mut fields = self.fields.clone();
        fields.insert("id".into(), serde_json::from_str(replacement_id)?);
        Ok(serde_json::to_string(&fields)?)
    }

    pub fn raw_result(&self) -> Option<&'a RawValue> {
        self.fields.get("result").copied()
    }

    pub fn raw_error(&self) -> Option<&'a RawValue> {
        self.fields.get("error").copied()
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
        struct Response<'a, T, E> {
            id: Option<&'a RawValue>,
            #[serde(skip_serializing_if = "Option::is_none")]
            jsonrpc: Option<&'a RawValue>,
            #[serde(flatten)]
            payload: Payload<T, E>,
        }
        Ok(serde_json::to_string(&Response {
            id: self.fields.get("id").copied(),
            jsonrpc: self.fields.get("jsonrpc").copied(),
            payload: match outcome {
                Ok(result) => Payload::Success { result },
                Err(error) => Payload::Failure { error },
            },
        })?)
    }

    pub fn error<C: Serialize>(
        &self,
        code: C,
        message: &dyn std::fmt::Display,
    ) -> Result<String, RpcMessageError> {
        #[derive(Serialize)]
        struct Fault<C> {
            code: C,
            message: String,
        }
        self.response::<(), _>(Err(Fault {
            code,
            message: message.to_string(),
        }))
    }
}

/// A typed provider result or its original error payload.
#[derive(Debug)]
pub struct RpcResponse<T> {
    pub outcome: Result<T, Box<RawValue>>,
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
        Ok(Self { outcome })
    }
}
fn classify_object<'a>(
    mut object: BTreeMap<String, &'a RawValue>,
) -> Result<RpcMessage<'a>, RpcMessageError> {
    object.retain(|key, _| {
        matches!(
            key.as_str(),
            "id" | "method" | "params" | "result" | "error" | "jsonrpc"
        )
    });
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
    #[error("invalid binary message: {0}")]
    Binary(#[from] std::io::Error),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

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
    }

    #[test]
    fn rewriting_id_preserves_escaped_keys_and_duplicate_id_safely() {
        let original = r#"{"\u0069d": 1, "method":"turn/start", "nested":{"id":2}, "id":"last"}"#;
        let rewritten = RpcMessage::parse(original)
            .unwrap()
            .rewrite_id(r#""proxy""#)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&rewritten).unwrap(),
            json!({"id":"proxy","method":"turn/start"})
        );
        #[derive(Deserialize)]
        struct RoutedId {
            id: String,
        }
        // Derived decoding rejects a duplicate field, including escaped aliases.
        assert_eq!(
            serde_json::from_str::<RoutedId>(&rewritten).unwrap().id,
            "proxy"
        );
        let original = r#" {"先頭":"値\\\"}]", "id" : [1,{"id":2}], "method":"x", "params":[{"text":"[{}]"}], "\u0069d": {"nested":true} } "#;
        let rewritten = RpcMessage::parse(original)
            .unwrap()
            .rewrite_id("null")
            .unwrap();
        let mut expected: Value = serde_json::from_str(original).unwrap();
        expected["id"] = Value::Null;
        expected.as_object_mut().unwrap().remove("先頭");
        assert_eq!(serde_json::from_str::<Value>(&rewritten).unwrap(), expected);
        assert!(
            RpcMessage::parse(original)
                .unwrap()
                .rewrite_id("1 2")
                .is_err()
        );
    }

    #[test]
    fn response_keeps_protocol_version_and_raw_id() {
        let line = r#"{"jsonrpc":"2.0","id":"mobile-1","method":"host/thread/list","params":{"future":{"id":9}},"extension":{"keep":[1,true]}}"#;
        let response = RpcMessage::parse(line)
            .unwrap()
            .response::<_, ()>(Ok(json!({"data":[]})))
            .unwrap();
        let object: BTreeMap<String, &RawValue> = serde_json::from_str(&response).unwrap();
        assert_eq!(object["id"].get(), r#""mobile-1""#);
        assert_eq!(object["jsonrpc"].get(), r#""2.0""#);
        assert!(!object.contains_key("extension"));
        assert_eq!(object["result"].get(), r#"{"data":[]}"#);
        assert!(!object.contains_key("params"));
        assert!(!object.contains_key("method"));
    }
}
