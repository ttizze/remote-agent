use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeMap};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Integer(u64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRequest {
    pub id: RpcId,
    pub method: String,
    #[serde(default = "empty_params")]
    pub params: Value,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcNotification {
    pub method: String,
    #[serde(default = "empty_params")]
    pub params: Value,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcResponse {
    pub id: RpcId,
    pub outcome: RpcOutcome,
    pub extensions: Map<String, Value>,
}

impl Serialize for RpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(2 + self.extensions.len()))?;
        map.serialize_entry("id", &self.id)?;
        match &self.outcome {
            RpcOutcome::Success { result } => map.serialize_entry("result", result)?,
            RpcOutcome::Failure { error } => map.serialize_entry("error", error)?,
        }
        for (key, value) in &self.extensions {
            if matches!(key.as_str(), "id" | "result" | "error") {
                continue;
            }
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut object = Map::<String, Value>::deserialize(deserializer)?;
        let id = object
            .remove("id")
            .ok_or_else(|| D::Error::custom("RPC response is missing id"))
            .and_then(|id| serde_json::from_value(id).map_err(D::Error::custom))?;
        let result = object.remove("result");
        let error = object.remove("error");
        let outcome = match (result, error) {
            (Some(result), None) => RpcOutcome::Success { result },
            (None, Some(error)) => RpcOutcome::Failure {
                error: serde_json::from_value(error).map_err(D::Error::custom)?,
            },
            (Some(_), Some(_)) => {
                return Err(D::Error::custom(
                    "RPC response cannot contain both result and error",
                ));
            }
            (None, None) => {
                return Err(D::Error::custom(
                    "RPC response must contain result or error",
                ));
            }
        };
        Ok(Self {
            id,
            outcome,
            extensions: object,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcOutcome {
    Success { result: Value },
    Failure { error: RpcError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: Value,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcMessage {
    Request(RpcRequest),
    Response(RpcResponse),
    Notification(RpcNotification),
}

fn empty_params() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_codex_messages_round_trip_with_string_ids_and_extensions() {
        let message = RpcMessage::Request(RpcRequest {
            id: RpcId::String("approval-7".to_owned()),
            method: "item/commandExecution/requestApproval".to_owned(),
            params: json!({"command": "cargo test"}),
            extensions: Map::from_iter([(String::from("jsonrpc"), json!("2.0"))]),
        });

        let encoded = serde_json::to_value(&message).unwrap();
        assert_eq!(
            encoded,
            json!({
                "id": "approval-7",
                "method": "item/commandExecution/requestApproval",
                "params": {"command": "cargo test"},
                "jsonrpc": "2.0"
            })
        );
        assert_eq!(
            serde_json::from_value::<RpcMessage>(encoded).unwrap(),
            message
        );
    }

    #[test]
    fn numeric_error_codes_and_response_extensions_are_preserved() {
        let response = json!({
            "id": 42,
            "error": {
                "code": -32001,
                "message": "approval unavailable",
                "data": {"retryable": true},
                "futureField": [1, 2, 3]
            },
            "jsonrpc": "2.0"
        });

        let decoded = serde_json::from_value::<RpcMessage>(response.clone()).unwrap();
        let RpcMessage::Response(decoded) = decoded else {
            panic!("expected response")
        };
        assert_eq!(decoded.id, RpcId::Integer(42));
        assert_eq!(decoded.extensions.get("jsonrpc"), Some(&json!("2.0")));
        let RpcOutcome::Failure { ref error } = decoded.outcome else {
            panic!("expected failure")
        };
        assert_eq!(error.code, json!(-32001));
        assert_eq!(error.extensions.get("futureField"), Some(&json!([1, 2, 3])));
        assert_eq!(
            serde_json::to_value(RpcMessage::Response(decoded)).unwrap(),
            response
        );
    }

    #[test]
    fn notification_extensions_are_preserved() {
        let message = json!({
            "method": "thread/event",
            "params": {"newField": {"nested": true}},
            "vendorExtension": "kept"
        });

        let decoded = serde_json::from_value::<RpcMessage>(message.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), message);
    }
}
