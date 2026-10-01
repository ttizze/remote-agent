//! JSONL envelope encoding preserves raw IDs and payloads at the protocol boundary.
use super::{PeerError, invalid};
use serde::Serialize;
use serde_json::value::RawValue;

pub fn request_line<P: Serialize>(method: &str, params: &P) -> Result<String, PeerError> {
    request_line_with_id(0, method, params)
}
pub(super) fn request_line_with_id<P: Serialize>(
    id: u64,
    method: &str,
    params: &P,
) -> Result<String, PeerError> {
    #[derive(Serialize)]
    struct Request<'a, P> {
        id: u64,
        method: &'a str,
        params: &'a P,
    }
    serde_json::to_string(&Request { id, method, params }).map_err(invalid)
}
pub fn response_line(id: &str, field: &'static str, payload: &str) -> Result<String, PeerError> {
    if !matches!(field, "result" | "error") {
        return Err(invalid("response field must be result or error"));
    }
    let id: &RawValue = serde_json::from_str(id).map_err(invalid)?;
    let payload: &RawValue = serde_json::from_str(payload).map_err(invalid)?;
    #[derive(Serialize)]
    struct ResultResponse<'a> {
        id: &'a RawValue,
        result: &'a RawValue,
    }
    #[derive(Serialize)]
    struct ErrorResponse<'a> {
        id: &'a RawValue,
        error: &'a RawValue,
    }
    if field == "result" {
        serde_json::to_string(&ResultResponse {
            id,
            result: payload,
        })
        .map_err(invalid)
    } else {
        serde_json::to_string(&ErrorResponse { id, error: payload }).map_err(invalid)
    }
}
#[cfg(test)]
mod envelope_tests {
    use super::*;
    #[test]
    fn typed_envelopes_keep_raw_params_and_response_payloads() {
        let params = r#"{"future": {"id": "nested"}, "text": "hello"}"#;
        let raw: &RawValue = serde_json::from_str(params).unwrap();
        let method = "custom/\"日本語\\method";
        let line = request_line(method, &raw).unwrap();
        let object: std::collections::BTreeMap<String, &RawValue> =
            serde_json::from_str(&line).unwrap();
        assert_eq!(object["params"].get(), params);
        assert_eq!(
            serde_json::from_str::<String>(object["method"].get()).unwrap(),
            method
        );
        let error = r#"{ "code": -1, "message": "失敗", "data": [null,{"id":7}] }"#;
        let response = response_line(r#""request-7""#, "error", error).unwrap();
        let object: std::collections::BTreeMap<String, &RawValue> =
            serde_json::from_str(&response).unwrap();
        assert_eq!(object["id"].get(), r#""request-7""#);
        assert_eq!(object["error"].get(), error);
        assert!(response_line("7 8", "result", "null").is_err());
        assert!(response_line("7", "error", "{").is_err());
        assert!(response_line("7", "params", "{}").is_err());
    }
}
