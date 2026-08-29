use host_daemon::{
    HOST_PROJECT_LIST_METHOD, HOST_THREAD_LIST_METHOD, HOST_THREAD_READ_METHOD,
    HOST_THREAD_START_METHOD,
};
use serde::Serialize;
use serde_json::{Map, Value};

/// Describes how a desktop-facing method is handled after it crosses the
/// Tauri boundary. The route contains no I/O and can therefore be tested
/// independently from a running Codex App Server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestRoute {
    ProjectList,
    ThreadList,
    ThreadRead,
    ThreadStart,
    Upstream,
}

impl RequestRoute {
    pub(super) fn for_method(method: &str) -> Self {
        match method {
            HOST_PROJECT_LIST_METHOD => Self::ProjectList,
            HOST_THREAD_LIST_METHOD => Self::ThreadList,
            HOST_THREAD_READ_METHOD => Self::ThreadRead,
            HOST_THREAD_START_METHOD => Self::ThreadStart,
            _ => Self::Upstream,
        }
    }

    pub(super) const fn is_project_list(self) -> bool {
        matches!(self, Self::ProjectList)
    }

    pub(super) const fn enriches_threads(self) -> bool {
        matches!(
            self,
            Self::ThreadList | Self::ThreadRead | Self::ThreadStart
        )
    }

    pub(super) fn upstream_method(self, requested: &str) -> &str {
        match self {
            Self::ThreadList => "thread/list",
            Self::ThreadRead => "thread/read",
            Self::ThreadStart => "thread/start",
            Self::ProjectList | Self::Upstream => requested,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponseKind {
    Result,
    Error,
}

impl ResponseKind {
    const fn field(self) -> &'static str {
        match self {
            Self::Result => "result",
            Self::Error => "error",
        }
    }
}

#[derive(Serialize)]
struct RequestEnvelope<'a> {
    id: u64,
    method: &'a str,
    params: &'a Value,
}

pub(super) fn request_line(id: u64, method: &str, params: &Value) -> Result<String, String> {
    serde_json::to_string(&RequestEnvelope { id, method, params })
        .map_err(|error| error.to_string())
}

pub(super) fn response_line(
    id: Value,
    kind: ResponseKind,
    payload: Value,
) -> Result<String, String> {
    let mut response = Map::from_iter([("id".to_owned(), id)]);
    response.insert(kind.field().to_owned(), payload);
    serde_json::to_string(&response).map_err(|error| error.to_string())
}

pub(super) fn response_result(line: &str) -> Result<Value, String> {
    let response: Map<String, Value> =
        serde_json::from_str(line).map_err(|error| format!("invalid Codex response: {error}"))?;
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Codex rejected the request");
        return Err(message.to_owned());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "Codex response is missing result".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_local_and_thread_methods() {
        assert_eq!(
            RequestRoute::for_method(HOST_PROJECT_LIST_METHOD),
            RequestRoute::ProjectList
        );
        assert!(RequestRoute::for_method(HOST_PROJECT_LIST_METHOD).is_project_list());

        let cases = [
            (
                HOST_THREAD_LIST_METHOD,
                RequestRoute::ThreadList,
                "thread/list",
            ),
            (
                HOST_THREAD_READ_METHOD,
                RequestRoute::ThreadRead,
                "thread/read",
            ),
            (
                HOST_THREAD_START_METHOD,
                RequestRoute::ThreadStart,
                "thread/start",
            ),
        ];
        for (requested, route, upstream) in cases {
            assert_eq!(RequestRoute::for_method(requested), route);
            assert_eq!(route.upstream_method(requested), upstream);
            assert!(route.enriches_threads());
        }
    }

    #[test]
    fn unknown_host_methods_are_forwarded_without_thread_enrichment() {
        let requested = "host/future-method";
        let route = RequestRoute::for_method(requested);

        assert_eq!(route, RequestRoute::Upstream);
        assert_eq!(route.upstream_method(requested), requested);
        assert!(!route.enriches_threads());
    }

    #[test]
    fn builds_request_and_response_envelopes() {
        assert_eq!(
            request_line(7, "thread/list", &json!({"nested": {"id": 1}})).unwrap(),
            r#"{"id":7,"method":"thread/list","params":{"nested":{"id":1}}}"#
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &response_line(
                    json!("approval-1"),
                    ResponseKind::Result,
                    json!({"ok": true})
                )
                .unwrap(),
            )
            .unwrap(),
            json!({"id": "approval-1", "result": {"ok": true}})
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                &response_line(json!(3), ResponseKind::Error, json!({"code": -1})).unwrap(),
            )
            .unwrap(),
            json!({"id": 3, "error": {"code": -1}})
        );
    }

    #[test]
    fn extracts_normal_and_null_results() {
        assert_eq!(
            response_result(r#"{"id":1,"result":{"value":7}}"#).unwrap(),
            json!({"value": 7})
        );
        assert_eq!(
            response_result(r#"{"id":1,"result":null}"#).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn preserves_upstream_error_message_fallback() {
        assert_eq!(
            response_result(r#"{"id":1,"error":{"code":-1,"message":"nope"}}"#).unwrap_err(),
            "nope"
        );
        assert_eq!(
            response_result(r#"{"id":1,"error":{"code":-1}}"#).unwrap_err(),
            "Codex rejected the request"
        );
    }

    #[test]
    fn reports_malformed_and_missing_result_responses() {
        let malformed = response_result("{").unwrap_err();
        assert!(malformed.starts_with("invalid Codex response: "));
        assert_eq!(
            response_result(r#"{"id":1}"#).unwrap_err(),
            "Codex response is missing result"
        );
    }
}
