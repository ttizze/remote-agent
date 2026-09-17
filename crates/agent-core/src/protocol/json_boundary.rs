//! Adapters for recorded JSON fixtures and external provider replies.
use super::*;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Opaque(#[serde(with = "super::json")] pub Value);
impl From<Opaque> for Body {
    fn from(value: Opaque) -> Self {
        value.0.into()
    }
}
pub use super::requests::{
    fixture_reply as reply, from_json as call, provider_response as response,
};
pub fn notification(method: &str, params: Value) -> Result<Notification, serde_json::Error> {
    match method {
        "host/session/activity"
        | "process/outputDelta"
        | "process/exited"
        | "host/terminal/failed" => serde_json::from_value(serde_json::json!({method:params})),
        _ => Ok(Notification::Provider {
            method: method.into(),
            params,
        }),
    }
}
