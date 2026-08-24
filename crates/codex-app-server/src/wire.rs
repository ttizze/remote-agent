use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The result of an App Server response together with members that live at
/// the JSON-RPC response's top level.  The result itself remains untyped so
/// newer Codex fields cross this boundary unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct RawResponse {
    pub result: Value,
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    Notification {
        method: String,
        params: Value,
        extensions: Map<String, Value>,
    },
    Request {
        id: RequestId,
        method: String,
        params: Value,
        extensions: Map<String, Value>,
    },
}

/// A response to a request initiated by the Codex App Server.
///
/// App Server requests are intentionally kept as JSON. Codex can add new
/// approval and other server-request payloads without requiring this crate to
/// understand them first, and callers can return either a successful result
/// or a structured JSON-RPC error without losing fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ServerResponse {
    Result { result: Value },
    Error { error: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum RequestId {
    Integer(u64),
    String(String),
}
